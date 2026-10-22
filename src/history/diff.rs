//! Unified-diff parsing into blocks/hunks, status parsing, hunk ids, hints.
use super::model::{Block, ChangeSet, FileChange, HunkInfo, Kind, Line};
use super::{Code, fail};
use crate::output::escape_line;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Flags shared by every diff the history verbs read (uncommitted and per commit).
pub(crate) const DIFF_FLAGS: [&str; 11] = [
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-renames",
    "--full-index",
    "-U3",
    "--diff-algorithm=myers",
    "--indent-heuristic",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "-p",
];

pub(crate) fn worktree_diff_args() -> Vec<&'static str> {
    let mut args = vec!["-c", "core.quotePath=true", "diff", "HEAD"];
    args.extend(DIFF_FLAGS);
    args
}

pub(crate) fn commit_diff_args(root: bool) -> Vec<&'static str> {
    let mut args = vec!["-c", "core.quotePath=true", "diff-tree", "-r"];
    if root {
        args.push("--root");
    }
    args.extend(DIFF_FLAGS);
    args
}

const CONTEXT: usize = 3;
const ZERO_PREFIX: &str = "0000000";

/// Parses a C-quoted Git path (as printed with core.quotePath=true).
pub(crate) fn unquote(token: &[u8]) -> Result<Vec<u8>> {
    let inner = token
        .strip_prefix(b"\"")
        .and_then(|rest| rest.strip_suffix(b"\""))
        .context("malformed quoted path")?;
    let mut output = Vec::with_capacity(inner.len());
    let mut index = 0;
    while index < inner.len() {
        let byte = inner[index];
        index += 1;
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let escape = *inner.get(index).context("dangling escape in quoted path")?;
        index += 1;
        match escape {
            b'a' => output.push(7),
            b'b' => output.push(8),
            b'f' => output.push(12),
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'v' => output.push(11),
            b'\\' | b'"' => output.push(escape),
            b'0'..=b'3' => {
                let digits = inner.get(index..index + 2).context("short octal escape")?;
                let value = u32::from(escape - b'0') * 64
                    + u32::from(digits[0].wrapping_sub(b'0')) * 8
                    + u32::from(digits[1].wrapping_sub(b'0'));
                output.push(u8::try_from(value).context("octal escape out of range")?);
                index += 2;
            }
            _ => bail!("unknown escape in quoted path"),
        }
    }
    Ok(output)
}

fn path_string(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| fail(Code::Unsupported, "non-UTF-8 paths are unsupported"))
}

fn header_path(rest: &[u8]) -> Result<String> {
    if rest.starts_with(b"\"") {
        // Quoted: "a/X" "b/X"; read the first quoted token.
        let mut end = 1;
        while end < rest.len() {
            match rest[end] {
                b'\\' => end += 2,
                b'"' => break,
                _ => end += 1,
            }
        }
        let first = unquote(&rest[..=end.min(rest.len() - 1)])?;
        let path = first
            .strip_prefix(b"a/")
            .context("unexpected diff header")?;
        return path_string(path.to_vec());
    }
    // Unquoted without renames: "a/X b/X".
    if rest.len() < 5 || !(rest.len() - 5).is_multiple_of(2) {
        bail!("unexpected diff header");
    }
    let length = (rest.len() - 5) / 2;
    path_string(rest[2..2 + length].to_vec())
}

fn lines_of(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn parse_range(token: &str) -> Result<(usize, usize)> {
    let token = token.trim_start_matches(['-', '+']);
    let (start, count) = match token.split_once(',') {
        Some((start, count)) => (start, count),
        None => (token, "1"),
    };
    Ok((start.parse()?, count.parse()?))
}

struct Pending {
    path: String,
    old_mode: Option<String>,
    new_mode: Option<String>,
    old_oid: Option<String>,
    new_oid: Option<String>,
    binary: bool,
    blocks: Vec<Block>,
    hunk_ranges: Vec<std::ops::Range<usize>>,
}

fn zero(oid: &str) -> bool {
    oid.bytes().all(|byte| byte == b'0') && oid.starts_with(ZERO_PREFIX)
}

/// Parses `git diff`/`git diff-tree -p` output (flags: `DIFF_FLAGS`).
pub(crate) fn parse_patch(patch: &[u8]) -> Result<Vec<FileChange>> {
    let lines = lines_of(patch);
    let mut files: Vec<Pending> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some(rest) = line.strip_prefix(b"diff --git ") else {
            // diff-tree prints the commit id first when given a single commit.
            index += 1;
            continue;
        };
        index += 1;
        let mut file = Pending {
            path: header_path(rest)?,
            old_mode: None,
            new_mode: None,
            old_oid: None,
            new_oid: None,
            binary: false,
            blocks: Vec::new(),
            hunk_ranges: Vec::new(),
        };
        while index < lines.len() && !lines[index].starts_with(b"diff --git ") {
            let line = lines[index];
            let text = String::from_utf8_lossy(line);
            if let Some(mode) = text.strip_prefix("old mode ") {
                file.old_mode = Some(mode.to_owned());
            } else if let Some(mode) = text.strip_prefix("new mode ") {
                file.new_mode = Some(mode.to_owned());
            } else if let Some(mode) = text.strip_prefix("new file mode ") {
                file.new_mode = Some(mode.to_owned());
            } else if let Some(mode) = text.strip_prefix("deleted file mode ") {
                file.old_mode = Some(mode.to_owned());
            } else if let Some(rest) = text.strip_prefix("index ") {
                let mut parts = rest.split(' ');
                let oids = parts.next().unwrap_or_default();
                let (old, new) = oids.split_once("..").context("malformed index line")?;
                file.old_oid = (!zero(old)).then(|| old.to_owned());
                file.new_oid = (!zero(new)).then(|| new.to_owned());
                if let Some(mode) = parts.next() {
                    file.old_mode.get_or_insert_with(|| mode.to_owned());
                    file.new_mode.get_or_insert_with(|| mode.to_owned());
                }
            } else if text.starts_with("Binary files ") || text.starts_with("GIT binary patch") {
                file.binary = true;
            } else if text.starts_with("@@ ") {
                index = parse_hunk(&lines, index, &mut file)?;
                continue;
            }
            index += 1;
        }
        files.push(file);
    }
    let mut merged: Vec<Pending> = Vec::new();
    for file in files {
        if let Some(previous) = merged.last_mut()
            && previous.path == file.path
        {
            // Type change: Git prints a deletion followed by an addition.
            previous.new_mode = file.new_mode;
            previous.new_oid = file.new_oid;
            previous.blocks.clear();
            previous.hunk_ranges.clear();
            previous.binary = true;
            continue;
        }
        merged.push(file);
    }
    merged.into_iter().map(finish).collect()
}

fn parse_hunk(lines: &[&[u8]], start: usize, file: &mut Pending) -> Result<usize> {
    let header = String::from_utf8_lossy(lines[start]).into_owned();
    let mut parts = header.split(' ');
    parts.next();
    let (old_start, old_count) = parse_range(parts.next().context("malformed hunk header")?)?;
    let (new_start, new_count) = parse_range(parts.next().context("malformed hunk header")?)?;
    let mut old_line = if old_count == 0 {
        old_start
    } else {
        old_start - 1
    };
    let mut new_line = if new_count == 0 {
        new_start
    } else {
        new_start - 1
    };
    let (mut remaining_old, mut remaining_new) = (old_count, new_count);
    let mut index = start + 1;
    let first_block = file.blocks.len();
    let mut context: Vec<Line> = Vec::new();
    let mut open: Option<Block> = None;
    let mut finished: Vec<Block> = Vec::new();
    while remaining_old > 0 || remaining_new > 0 {
        let line = *lines.get(index).context("truncated hunk")?;
        index += 1;
        let (marker, body) = line.split_first().context("empty diff line")?;
        let mut content = body.to_vec();
        if lines.get(index).is_some_and(|next| next.starts_with(b"\\")) {
            index += 1;
        } else {
            content.push(b'\n');
        }
        match marker {
            b' ' => {
                if let Some(mut block) = open.take() {
                    block.after.clear();
                    finished.push(block);
                    context.clear();
                }
                context.push(content);
                old_line += 1;
                new_line += 1;
                remaining_old = remaining_old
                    .checked_sub(1)
                    .context("hunk count mismatch")?;
                remaining_new = remaining_new
                    .checked_sub(1)
                    .context("hunk count mismatch")?;
            }
            b'-' | b'+' => {
                let block = open.get_or_insert_with(|| {
                    let keep = context.len().saturating_sub(CONTEXT);
                    let before = context[keep..].to_vec();
                    if let Some(previous) = finished.last_mut() {
                        previous.after = context.iter().take(CONTEXT).cloned().collect();
                    }
                    context.clear();
                    Block {
                        old_pos: old_line,
                        new_pos: new_line,
                        removed: Vec::new(),
                        added: Vec::new(),
                        before,
                        after: Vec::new(),
                        at_start: old_line == 0,
                        at_eof: false,
                    }
                });
                if *marker == b'-' {
                    block.removed.push(content);
                    old_line += 1;
                    remaining_old = remaining_old
                        .checked_sub(1)
                        .context("hunk count mismatch")?;
                } else {
                    block.added.push(content);
                    new_line += 1;
                    remaining_new = remaining_new
                        .checked_sub(1)
                        .context("hunk count mismatch")?;
                }
            }
            _ => bail!("unexpected line in hunk"),
        }
    }
    // A closing no-newline marker after the final line is consumed above; skip stray markers.
    while lines.get(index).is_some_and(|next| next.starts_with(b"\\")) {
        index += 1;
    }
    if let Some(block) = open.take() {
        finished.push(block);
    }
    // Context after the last block of the hunk (the trailing run).
    if let Some(last) = finished.last_mut() {
        last.after = context.iter().take(CONTEXT).cloned().collect();
        last.at_eof = context.len() < CONTEXT;
    }
    // Context between blocks was assigned as `after` of the earlier block when the next opened.
    file.blocks.extend(finished);
    file.hunk_ranges.push(first_block..file.blocks.len());
    Ok(index)
}

fn finish(file: Pending) -> Result<FileChange> {
    for mode in [&file.old_mode, &file.new_mode].into_iter().flatten() {
        if mode == "160000" {
            return Err(fail(
                Code::Unsupported,
                format!("{}: submodules are unsupported", file.path),
            ));
        }
    }
    let symlink = |mode: &Option<String>| mode.as_deref() == Some("120000");
    let kind = if file.old_mode.is_some()
        && file.new_mode.is_some()
        && symlink(&file.old_mode) != symlink(&file.new_mode)
    {
        Kind::TypeChange
    } else if symlink(&file.old_mode) || symlink(&file.new_mode) {
        Kind::Symlink
    } else if file.binary {
        Kind::Binary
    } else if file.old_mode.is_none() {
        Kind::Added
    } else if file.new_mode.is_none() {
        Kind::Deleted
    } else if file.blocks.is_empty() && file.old_mode != file.new_mode {
        Kind::ModeOnly
    } else {
        Kind::Modified
    };
    let whole = kind != Kind::Modified;
    let blocks = if matches!(kind, Kind::Added | Kind::Deleted | Kind::Modified) {
        file.blocks
    } else {
        Vec::new()
    };
    let lines = match kind {
        Kind::Added => blocks.iter().map(|block| block.added.len()).sum(),
        Kind::Deleted => blocks.iter().map(|block| block.removed.len()).sum(),
        _ => 0,
    };
    let ranges = if whole {
        std::iter::once(0..blocks.len()).collect()
    } else {
        file.hunk_ranges
    };
    let hunks = ranges
        .into_iter()
        .map(|range| describe(&blocks, range))
        .collect::<Vec<_>>();
    let ws = kind == Kind::Modified && !hunks.is_empty() && hunks.iter().all(|hunk| hunk.ws);
    Ok(FileChange {
        path: file.path,
        kind,
        old_mode: file.old_mode,
        new_mode: file.new_mode,
        old_oid: file.old_oid,
        new_oid: file.new_oid,
        blocks,
        hunks,
        lines,
        binary: kind == Kind::Binary,
        digest: String::new(),
        ws,
    })
}

fn strip_ws(lines: &[&Line]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|line| line.iter())
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect()
}

fn describe(blocks: &[Block], range: std::ops::Range<usize>) -> HunkInfo {
    let slice = &blocks[range.clone()];
    let added: Vec<&Line> = slice.iter().flat_map(|block| &block.added).collect();
    let removed: Vec<&Line> = slice.iter().flat_map(|block| &block.removed).collect();
    let alnum = |line: &Line| {
        line.iter()
            .filter(|byte| byte.is_ascii_alphanumeric())
            .count()
            >= 3
    };
    let hint = added
        .iter()
        .find(|line| alnum(line))
        .map(|line| clean(line, ""))
        .or_else(|| removed.first().map(|line| clean(line, "- ")))
        .unwrap_or_default();
    HunkInfo {
        line: slice.first().map_or(1, |block| block.new_pos + 1),
        added: added.len(),
        removed: removed.len(),
        ws: !slice.is_empty() && strip_ws(&added) == strip_ws(&removed),
        blocks: range,
        id: String::new(),
        short: String::new(),
        hint,
    }
}

fn clean(line: &[u8], prefix: &str) -> String {
    let text = String::from_utf8_lossy(line);
    let text = escape_line(text.trim());
    let text = format!("{prefix}{text}");
    if text.chars().count() > 72 {
        format!("{}…", text.chars().take(72).collect::<String>())
    } else {
        text
    }
}

/// Letters g..v for nibbles 0..15, so ids can never be mistaken for hex commit ids.
fn letters(digest: &[u8]) -> String {
    digest
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 15])
        .map(|nibble| char::from(b'g' + nibble))
        .collect()
}

fn content_key(file: &FileChange, hunk: &HunkInfo) -> Vec<u8> {
    let mut key = Vec::new();
    if file.whole() {
        key.extend_from_slice(format!("!{}\n", file.kind.label()).as_bytes());
        match file.kind {
            Kind::Untracked => key.extend_from_slice(file.digest.as_bytes()),
            Kind::Binary | Kind::Symlink | Kind::TypeChange | Kind::ModeOnly => {
                key.extend_from_slice(
                    format!(
                        "{:?} {:?} {:?} {:?}",
                        file.old_mode, file.new_mode, file.old_oid, file.new_oid
                    )
                    .as_bytes(),
                );
            }
            _ => {}
        }
    }
    for block in &file.blocks[hunk.blocks.clone()] {
        key.push(b'-');
        key.extend(block.removed.iter().flatten());
        key.push(b'+');
        key.extend(block.added.iter().flatten());
        key.push(b'\n');
    }
    key
}

/// Assigns content-based hunk ids and the shortest unique display prefixes.
pub(crate) fn assign_ids(files: &mut [FileChange]) {
    for file in files.iter_mut() {
        let mut seen: Vec<(Vec<u8>, usize)> = Vec::new();
        let keys: Vec<Vec<u8>> = file
            .hunks
            .iter()
            .map(|hunk| content_key(file, hunk))
            .collect();
        for (hunk, key) in file.hunks.iter_mut().zip(keys) {
            let ordinal = match seen.iter_mut().find(|(known, _)| *known == key) {
                Some((_, count)) => {
                    *count += 1;
                    *count
                }
                None => {
                    seen.push((key.clone(), 0));
                    0
                }
            };
            let mut hasher = Sha256::new();
            hasher.update(b"aisc-hunk-v1\n");
            hasher.update(file.path.as_bytes());
            hasher.update(b"\n");
            hasher.update(&key);
            hasher.update(format!("#{ordinal}"));
            hunk.id = letters(&hasher.finalize());
        }
    }
    let mut ids: Vec<&str> = files
        .iter()
        .flat_map(|file| file.hunks.iter().map(|hunk| hunk.id.as_str()))
        .collect();
    ids.sort_unstable();
    let common = |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let mut needed = std::collections::HashMap::new();
    for (position, id) in ids.iter().enumerate() {
        let before = position.checked_sub(1).map_or(0, |p| common(ids[p], id));
        let after = ids.get(position + 1).map_or(0, |next| common(next, id));
        needed.insert(
            (*id).to_owned(),
            (before.max(after) + 1).max(3).min(id.len()),
        );
    }
    for file in files.iter_mut() {
        for hunk in &mut file.hunks {
            let length = needed.get(&hunk.id).copied().unwrap_or(3);
            hunk.short = hunk.id[..length].to_owned();
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Status {
    pub oid: Option<String>,
    /// `None` when detached.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub untracked: Vec<String>,
    pub unmerged: Vec<String>,
}

/// Parses `git status --porcelain=v2 --branch -z --untracked-files=all`.
pub(crate) fn parse_status(bytes: &[u8]) -> Result<Status> {
    let mut status = Status::default();
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        if let Some(value) = text.strip_prefix("# branch.oid ") {
            status.oid = (value != "(initial)").then(|| value.to_owned());
        } else if let Some(value) = text.strip_prefix("# branch.head ") {
            status.head = (value != "(detached)").then(|| value.to_owned());
        } else if let Some(value) = text.strip_prefix("# branch.upstream ") {
            status.upstream = Some(value.to_owned());
        } else if let Some(value) = text.strip_prefix("# branch.ab ") {
            let mut parts = value.split(' ');
            status.ahead = parts
                .next()
                .and_then(|part| part.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            status.behind = parts
                .next()
                .and_then(|part| part.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
        } else if text.starts_with('#') {
        } else if let Some(path) = text.strip_prefix("? ") {
            status.untracked.push(path.to_owned());
        } else if text.starts_with("u ") {
            let path = text.splitn(11, ' ').last().unwrap_or_default();
            status.unmerged.push(path.to_owned());
        } else if text.starts_with("2 ") {
            records.next();
        }
    }
    Ok(status)
}

/// Builds the uncommitted change set: tracked diff against HEAD plus untracked files.
pub(crate) fn build_changes(root: &Path, patch: &[u8], untracked: &[String]) -> Result<ChangeSet> {
    let mut files = parse_patch(patch)?;
    let mut nested = Vec::new();
    for path in untracked {
        if path.ends_with('/') {
            nested.push(path.clone());
            continue;
        }
        let full = root.join(path);
        let Ok(metadata) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let (mode, lines, binary, digest) = if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&full).unwrap_or_default();
            let bytes = target.as_os_str().as_encoded_bytes().to_vec();
            ("120000", 0, false, hex(&Sha256::digest(&bytes)))
        } else if metadata.is_file() {
            let mode = if metadata.permissions().mode() & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            };
            if metadata.len() > 16 * 1_024 * 1_024 {
                (mode, 0, true, format!("size-{}", metadata.len()))
            } else {
                let bytes = std::fs::read(&full).unwrap_or_default();
                let binary = bytes.contains(&0);
                let lines = if binary {
                    0
                } else {
                    super::apply::split_lines(&bytes).len()
                };
                (mode, lines, binary, hex(&Sha256::digest(&bytes)))
            }
        } else {
            continue;
        };
        files.push(FileChange {
            path: path.clone(),
            kind: Kind::Untracked,
            old_mode: None,
            new_mode: Some(mode.to_owned()),
            old_oid: None,
            new_oid: None,
            blocks: Vec::new(),
            hunks: vec![HunkInfo {
                blocks: 0..0,
                id: String::new(),
                short: String::new(),
                line: 1,
                added: lines,
                removed: 0,
                hint: String::new(),
                ws: false,
            }],
            lines,
            binary,
            digest,
            ws: false,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    assign_ids(&mut files);
    Ok(ChangeSet { files, nested })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/src/a.txt b/src/a.txt\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/src/a.txt\n+++ b/src/a.txt\n@@ -1,7 +1,8 @@\n l1\n l2\n l3\n-old line\n+new line\n+extra line\n l5\n l6\n l7\n\\ No newline at end of file\n";

    #[test]
    fn parses_blocks_context_and_no_newline() {
        let files = parse_patch(PATCH.as_bytes()).unwrap();
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.path, "src/a.txt");
        assert_eq!(file.kind, Kind::Modified);
        let block = &file.blocks[0];
        assert_eq!((block.old_pos, block.new_pos), (3, 3));
        assert_eq!(block.removed, vec![b"old line\n".to_vec()]);
        assert_eq!(block.added.len(), 2);
        assert_eq!(block.before.len(), 3);
        assert_eq!(block.after.last().unwrap(), b"l7");
        assert!(
            !block.at_eof,
            "three trailing context lines: not known to be EOF"
        );
        assert_eq!(file.hunks[0].line, 4);
        assert_eq!((file.hunks[0].added, file.hunks[0].removed), (2, 1));
        assert_eq!(file.hunks[0].hint, "new line");
    }

    #[test]
    fn quoted_paths_and_whole_file_kinds() {
        let patch = "diff --git \"a/caf\\303\\251 \\\"x\\\".txt\" \"b/caf\\303\\251 \\\"x\\\".txt\"\nnew file mode 100755\nindex 0000000000000000000000000000000000000000..3333333333333333333333333333333333333333\n--- /dev/null\n+++ \"b/caf\\303\\251 \\\"x\\\".txt\"\n@@ -0,0 +1,2 @@\n+a\n+b\ndiff --git a/gone.bin b/gone.bin\ndeleted file mode 100644\nindex 4444444444444444444444444444444444444444..0000000000000000000000000000000000000000\nBinary files a/gone.bin and /dev/null differ\ndiff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let files = parse_patch(patch.as_bytes()).unwrap();
        assert_eq!(files[0].path, "café \"x\".txt");
        assert_eq!(files[0].kind, Kind::Added);
        assert_eq!(files[0].lines, 2);
        assert_eq!(files[1].kind, Kind::Binary);
        assert_eq!(files[2].kind, Kind::ModeOnly);
        assert!(files.iter().all(|file| file.whole()));
    }

    #[test]
    fn submodule_diffs_are_refused() {
        let patch = "diff --git a/sub b/sub\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 160000\n";
        assert!(parse_patch(patch.as_bytes()).is_err());
    }

    #[test]
    fn hunk_ids_use_g_to_v_depend_only_on_content_and_count_duplicates() {
        let patch = "diff --git a/f b/f\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n-dup\n+DUP\n k1\n k2\n k3\n@@ -20,4 +20,4 @@\n-dup\n+DUP\n k1\n k2\n k3\n";
        let mut first = parse_patch(patch.as_bytes()).unwrap();
        assign_ids(&mut first);
        let mut second = parse_patch(
            patch
                .replace("@@ -20,4 +20,4 @@", "@@ -30,4 +31,4 @@")
                .as_bytes(),
        )
        .unwrap();
        assign_ids(&mut second);
        let ids = |files: &[FileChange]| {
            files[0]
                .hunks
                .iter()
                .map(|h| h.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second), "line numbers do not affect ids");
        assert_ne!(
            ids(&first)[0],
            ids(&first)[1],
            "ordinal separates duplicates"
        );
        assert!(ids(&first)[0].chars().all(|c| ('g'..='v').contains(&c)));
        assert_eq!(ids(&first)[0].len(), 64);
        assert!(first[0].hunks[0].short.len() >= 3);
    }

    #[test]
    fn whitespace_only_and_hint_fallback() {
        let patch = "diff --git a/f b/f\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n-a  b\n+a b\n k1\n k2\n";
        let files = parse_patch(patch.as_bytes()).unwrap();
        assert!(files[0].hunks[0].ws && files[0].ws);
        let removal = "diff --git a/f b/f\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,2 @@\n-gone line\n k1\n k2\n";
        let files = parse_patch(removal.as_bytes()).unwrap();
        assert_eq!(files[0].hunks[0].hint, "- gone line");
        assert!(!files[0].hunks[0].ws);
    }

    #[test]
    fn status_parser_reads_branch_and_untracked() {
        let raw = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +1 -2\0? new file.txt\0u UU N... 1 1 1 1 h1 h2 h3 conflict.txt\0";
        let status = parse_status(raw).unwrap();
        assert_eq!(status.head.as_deref(), Some("main"));
        assert_eq!((status.ahead, status.behind), (1, 2));
        assert_eq!(status.untracked, vec!["new file.txt"]);
        assert_eq!(status.unmerged, vec!["conflict.txt"]);
    }
}
