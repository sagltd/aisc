//! Selector resolution against a change set (uncommitted changes or one commit's diff).
use super::model::{ChangeSet, FileChange, Kind};
use super::{Code, fail, fail_hint};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

/// What is chosen from one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileSel {
    /// Everything: all hunks, mode changes, deletion, whole-file items.
    Whole,
    /// A subset of the file's blocks (indices into `FileChange::blocks`).
    Blocks(BTreeSet<usize>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    pub files: BTreeMap<usize, FileSel>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn add(&mut self, file: usize, selected: FileSel) {
        match (self.files.get_mut(&file), selected) {
            (Some(FileSel::Whole), _) => {}
            (_, FileSel::Whole) => {
                self.files.insert(file, FileSel::Whole);
            }
            (Some(FileSel::Blocks(existing)), FileSel::Blocks(more)) => existing.extend(more),
            (None, blocks) => {
                self.files.insert(file, blocks);
            }
        }
    }

    /// Collapses a block set that covers an unchanged-mode file into `Whole`.
    fn normalize(&mut self, changes: &ChangeSet) {
        for (index, selected) in &mut self.files {
            let file = &changes.files[*index];
            if let FileSel::Blocks(blocks) = selected
                && file.kind == Kind::Modified
                && blocks.len() == file.blocks.len()
                && !file.mode_changed()
            {
                *selected = FileSel::Whole;
            }
        }
    }

    /// True when `block` of `file` is chosen.
    pub fn has_block(&self, file: usize, block: usize) -> bool {
        match self.files.get(&file) {
            Some(FileSel::Whole) => true,
            Some(FileSel::Blocks(blocks)) => blocks.contains(&block),
            None => false,
        }
    }
}

pub(crate) fn all(changes: &ChangeSet) -> Result<Selection> {
    if let Some(nested) = changes.nested.first() {
        return Err(fail(
            Code::Unsupported,
            format!("{nested}: nested repositories cannot be committed"),
        ));
    }
    let mut selection = Selection::default();
    for index in 0..changes.files.len() {
        selection.files.insert(index, FileSel::Whole);
    }
    if selection.is_empty() {
        return Err(fail(Code::Selector, "there are no changes to select"));
    }
    Ok(selection)
}

fn path_list(changes: &ChangeSet) -> String {
    let mut paths = changes
        .files
        .iter()
        .take(8)
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if changes.files.len() > 8 {
        paths.push_str(", …");
    }
    paths
}

fn is_id(token: &str) -> bool {
    (3..=64).contains(&token.len()) && token.bytes().all(|byte| (b'g'..=b'v').contains(&byte))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn text_matches(root: &std::path::Path, file: &FileChange, needle: &[u8]) -> Vec<usize> {
    match file.kind {
        Kind::Untracked => {
            let found = !file.binary
                && std::fs::read(root.join(&file.path)).is_ok_and(|bytes| contains(&bytes, needle));
            if found { vec![0] } else { Vec::new() }
        }
        Kind::Modified | Kind::Added | Kind::Deleted => file
            .hunks
            .iter()
            .enumerate()
            .filter(|(_, hunk)| {
                file.blocks[hunk.blocks.clone()].iter().any(|block| {
                    block
                        .removed
                        .iter()
                        .chain(&block.added)
                        .any(|line| contains(line, needle))
                })
            })
            .map(|(index, _)| index)
            .collect(),
        _ => Vec::new(),
    }
}

fn hunk_selection(file: &FileChange, hunks: &[usize]) -> FileSel {
    if file.whole() {
        return FileSel::Whole;
    }
    let mut blocks = BTreeSet::new();
    for hunk in hunks {
        blocks.extend(file.hunks[*hunk].blocks.clone());
    }
    FileSel::Blocks(blocks)
}

fn parse_lines(spec: &str) -> Option<(usize, usize)> {
    let (start, end) = match spec.split_once('-') {
        Some((start, end)) => (start, end),
        None => (spec, spec),
    };
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// Resolves selector tokens (one group) to a selection.
pub(crate) fn resolve(
    root: &std::path::Path,
    changes: &ChangeSet,
    tokens: &[String],
) -> Result<Selection> {
    let mut selection = Selection::default();
    for token in tokens {
        resolve_token(root, changes, token, &mut selection)?;
    }
    if selection.is_empty() {
        return Err(fail(Code::Selector, "no selector given"));
    }
    selection.normalize(changes);
    Ok(selection)
}

fn resolve_token(
    root: &std::path::Path,
    changes: &ChangeSet,
    token: &str,
    selection: &mut Selection,
) -> Result<()> {
    // 1. an exact changed path
    if let Some(index) = changes.index_of(token) {
        selection.add(index, FileSel::Whole);
        return Ok(());
    }
    // 2. PATH:L-R or PATH:L
    if let Some((path, range)) = token.rsplit_once(':')
        && let Some(index) = changes.index_of(path)
        && let Some((start, end)) = parse_lines(range)
    {
        if start == 0 || end < start {
            return Err(fail(
                Code::Usage,
                format!("invalid line range in '{token}'"),
            ));
        }
        let file = &changes.files[index];
        if file.whole() || file.kind == Kind::Untracked {
            let has_lines = matches!(file.kind, Kind::Untracked | Kind::Added) && !file.binary;
            if !has_lines {
                let what = if file.binary {
                    "binary"
                } else {
                    file.kind.label()
                };
                return Err(fail_hint(
                    Code::Selector,
                    format!("{path} is a {what} change; line ranges do not apply"),
                    "select the whole path instead",
                ));
            }
            let lines = file.lines;
            if start > lines {
                return Err(fail(
                    Code::Selector,
                    format!("{path} has {lines} lines; none in {start}-{end}"),
                ));
            }
            selection.add(index, FileSel::Whole);
            return Ok(());
        }
        let blocks: BTreeSet<usize> = file
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                let (from, count) = if block.added.is_empty() {
                    (block.old_pos + 1, block.removed.len())
                } else {
                    (block.new_pos + 1, block.added.len())
                };
                let last = from + count.max(1) - 1;
                from <= end && last >= start
            })
            .map(|(position, _)| position)
            .collect();
        if blocks.is_empty() {
            return Err(fail_hint(
                Code::Selector,
                format!("no change in {path} intersects lines {start}-{end}"),
                "run aisc log for hunk positions",
            ));
        }
        selection.add(index, FileSel::Blocks(blocks));
        return Ok(());
    }
    // 3. PATH~TEXT at the first '~' whose prefix is a changed path
    for (position, _) in token.match_indices('~') {
        let (path, needle) = (&token[..position], &token[position + 1..]);
        if let Some(index) = changes.index_of(path) {
            if needle.is_empty() {
                return Err(fail(Code::Usage, format!("empty text in '{token}'")));
            }
            let file = &changes.files[index];
            let hunks = text_matches(root, file, needle.as_bytes());
            if hunks.is_empty() {
                return Err(fail_hint(
                    Code::Selector,
                    format!("no changed line in {path} contains {needle:?}"),
                    "run aisc log or aisc show to see the changes",
                ));
            }
            selection.add(index, hunk_selection(file, &hunks));
            return Ok(());
        }
    }
    // 4. hunk id prefix
    if is_id(token) {
        let matches: Vec<(usize, usize)> = changes
            .files
            .iter()
            .enumerate()
            .flat_map(|(file_index, file)| {
                file.hunks
                    .iter()
                    .enumerate()
                    .filter(|(_, hunk)| hunk.id.starts_with(token))
                    .map(move |(hunk_index, _)| (file_index, hunk_index))
            })
            .collect();
        return match matches.as_slice() {
            [] => Err(fail_hint(
                Code::Selector,
                format!("no hunk with id '{token}'"),
                "the change may have been edited; run aisc log for current ids",
            )),
            [(file_index, hunk_index)] => {
                let file = &changes.files[*file_index];
                selection.add(*file_index, hunk_selection(file, &[*hunk_index]));
                Ok(())
            }
            many => {
                let candidates = many
                    .iter()
                    .take(6)
                    .map(|(file, hunk)| changes.files[*file].hunks[*hunk].short.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(fail_hint(
                    Code::Selector,
                    format!("hunk id '{token}' is ambiguous: {candidates}"),
                    "use a longer prefix",
                ))
            }
        };
    }
    Err(fail_hint(
        Code::Selector,
        format!("unknown selector '{token}' (paths: {})", path_list(changes)),
        "selectors: PATH, hunk id, PATH:START-END, PATH~text",
    ))
}

/// Rejects blocks or files claimed by more than one group.
pub(crate) fn check_disjoint(changes: &ChangeSet, groups: &[Selection]) -> Result<()> {
    let mut claimed: BTreeMap<(usize, Option<usize>), ()> = BTreeMap::new();
    let describe = |file: usize, block: Option<usize>| match block {
        Some(block) => {
            let file = &changes.files[file];
            let hunk = file
                .hunks
                .iter()
                .find(|hunk| hunk.blocks.contains(&block))
                .map_or("?", |hunk| hunk.short.as_str());
            format!("hunk {hunk} ({}) selected twice", file.path)
        }
        None => format!("{} selected twice", changes.files[file].path),
    };
    for group in groups {
        let mut mine = Vec::new();
        for (file, selected) in &group.files {
            match selected {
                FileSel::Whole => mine.push((*file, None)),
                FileSel::Blocks(blocks) => {
                    mine.extend(blocks.iter().map(|block| (*file, Some(*block))));
                }
            }
        }
        for (file, block) in &mine {
            let conflict = claimed.contains_key(&(*file, *block))
                || claimed.contains_key(&(*file, None))
                || (block.is_none() && claimed.keys().any(|(other, _)| other == file));
            if conflict {
                return Err(fail_hint(
                    Code::Selector,
                    describe(*file, *block),
                    "each change can go into one group only",
                ));
            }
        }
        for key in mine {
            claimed.insert(key, ());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::diff::{assign_ids, parse_patch};

    const PATCH: &str = "diff --git a/f.txt b/f.txt\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n-alpha one\n+alpha ONE\n c1\n c2\n c3\n@@ -20,4 +20,4 @@\n c4\n c5\n c6\n-omega two\n+omega TWO\n";

    fn changes() -> ChangeSet {
        let mut files = parse_patch(PATCH.as_bytes()).unwrap();
        assign_ids(&mut files);
        ChangeSet {
            files,
            nested: Vec::new(),
        }
    }

    fn select(tokens: &[&str]) -> Result<Selection> {
        let tokens: Vec<String> = tokens.iter().map(|token| (*token).to_owned()).collect();
        resolve(std::path::Path::new("."), &changes(), &tokens)
    }

    fn code(error: anyhow::Error) -> Code {
        error
            .downcast_ref::<crate::history::HistoryError>()
            .unwrap()
            .code
    }

    #[test]
    fn selects_by_path_text_lines_and_id() {
        assert_eq!(select(&["f.txt"]).unwrap().files[&0], FileSel::Whole);
        let by_text = select(&["f.txt~omega"]).unwrap();
        assert_eq!(by_text.files[&0], FileSel::Blocks(BTreeSet::from([1])));
        let by_lines = select(&["f.txt:1"]).unwrap();
        assert_eq!(by_lines.files[&0], FileSel::Blocks(BTreeSet::from([0])));
        let id = changes().files[0].hunks[1].short.clone();
        assert_eq!(select(&[&id]).unwrap(), by_text);
        assert_eq!(
            select(&["f.txt~alpha", "f.txt~omega"]).unwrap().files[&0],
            FileSel::Whole
        );
    }

    #[test]
    fn reports_unknown_zero_match_and_stale_selectors() {
        assert_eq!(code(select(&["nope.txt"]).unwrap_err()), Code::Selector);
        assert_eq!(code(select(&["f.txt~absent"]).unwrap_err()), Code::Selector);
        assert_eq!(code(select(&["f.txt:10"]).unwrap_err()), Code::Selector);
        assert_eq!(code(select(&["ggg"]).unwrap_err()), Code::Selector);
        assert_eq!(code(select(&["f.txt:0-2"]).unwrap_err()), Code::Usage);
    }

    #[test]
    fn ambiguous_id_prefixes_list_candidates() {
        let mut changes = changes();
        changes.files[0].hunks[0].id = format!("hhh{}", "g".repeat(61));
        changes.files[0].hunks[0].short = "hhhg".to_owned();
        changes.files[0].hunks[1].id = format!("hhi{}", "g".repeat(61));
        changes.files[0].hunks[1].short = "hhi".to_owned();
        let tokens = vec!["hh".to_owned() + "h"];
        assert!(resolve(std::path::Path::new("."), &changes, &tokens).is_ok());
        // Both ids share the prefix "hh", which is shorter than the 3-letter minimum.
        let tokens = vec!["hhh".to_owned(), "hhi".to_owned()];
        assert!(resolve(std::path::Path::new("."), &changes, &tokens).is_ok());
        changes.files[0].hunks[1].id = format!("hhh{}", "h".repeat(61));
        let error = resolve(std::path::Path::new("."), &changes, &["hhh".to_owned()]).unwrap_err();
        assert_eq!(code(error), Code::Selector);
        let message = resolve(std::path::Path::new("."), &changes, &["hhh".to_owned()])
            .unwrap_err()
            .to_string();
        assert!(message.contains("ambiguous"), "{message}");
    }

    #[test]
    fn overlapping_groups_are_rejected() {
        let changes = changes();
        let one = select(&["f.txt~alpha"]).unwrap();
        let two = select(&["f.txt:1"]).unwrap();
        assert!(check_disjoint(&changes, &[one.clone(), two]).is_err());
        let other = select(&["f.txt~omega"]).unwrap();
        assert!(check_disjoint(&changes, &[one, other]).is_ok());
    }
}
