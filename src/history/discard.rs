//! `aisc discard`: reverts selected uncommitted changes. The old bytes are kept in the op so that
//! `aisc undo` restores them exactly.
use super::amend::{finish, hash_as_staged, hunks_in, require_checked_out};
use super::apply::{Direction, join_lines, split_lines, transplant, unsplice};
use super::model::{Block, ChangeSet, FileChange, Kind};
use super::plumbing::{Entry, Git};
use super::rewrite::{IndexChange, Plan, Session, WorktreeWrite, stale};
use super::selector::{self, FileSel, Selection};
use super::{Code, Outcome, fail, fail_hint, view};
use crate::cli;
use crate::git::{Repo, text};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hunks whose removed and added text differ only by whitespace.
fn whitespace_selection(changes: &ChangeSet, within: Option<&Selection>) -> Result<Selection> {
    let mut out = Selection::default();
    for (index, file) in changes.files.iter().enumerate() {
        if file.kind != Kind::Modified {
            continue;
        }
        let allowed =
            |block: usize| within.is_none_or(|selection| selection.has_block(index, block));
        let blocks: BTreeSet<usize> = file
            .hunks
            .iter()
            .filter(|hunk| hunk.ws)
            .flat_map(|hunk| hunk.blocks.clone())
            .filter(|block| allowed(*block))
            .collect();
        if blocks.is_empty() {
            continue;
        }
        if blocks.len() == file.blocks.len() && !file.mode_changed() {
            out.files.insert(index, FileSel::Whole);
        } else {
            out.files.insert(index, FileSel::Blocks(blocks));
        }
    }
    if out.is_empty() {
        return Err(fail_hint(
            Code::Selector,
            "there are no whitespace-only changes to discard",
            "run aisc log; hunks marked [ws] differ only by whitespace",
        ));
    }
    Ok(out)
}

/// Stage-0 index entries for `paths`; a conflicted path is refused.
fn index_entries(git: &Git, paths: &[String]) -> Result<BTreeMap<String, Entry>> {
    let mut entries = BTreeMap::new();
    for chunk in paths.chunks(400) {
        let mut args = vec!["ls-files", "-s", "-z", "--"];
        args.extend(chunk.iter().map(String::as_str));
        let output = git.run(&args)?;
        for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .context("malformed ls-files record")?;
            let meta = text(&record[..tab])?;
            let path = String::from_utf8(record[tab + 1..].to_vec())?;
            let mut fields = meta.split(' ');
            let mode = fields.next().context("ls-files mode")?.to_owned();
            let oid = fields.next().context("ls-files oid")?.to_owned();
            let stage = fields.next().context("ls-files stage")?;
            if stage != "0" {
                return Err(fail(
                    Code::Dirty,
                    format!("{path}: unresolved conflict in the index"),
                ));
            }
            entries.insert(path, (mode, oid));
        }
    }
    Ok(entries)
}

/// Paths whose index entry is an intent-to-add placeholder: porcelain v2 reports them as
/// index-unchanged (`.`) and worktree-added (`A`).
fn intent_to_add(git: &Git, paths: &[String]) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for chunk in paths.chunks(400) {
        let mut args = vec![
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--no-renames",
            "--",
        ];
        args.extend(chunk.iter().map(String::as_str));
        let output = git.run(&args)?;
        for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let line = String::from_utf8_lossy(record);
            if line.starts_with("1 .A ")
                && let Some(path) = line.splitn(9, ' ').nth(8)
            {
                found.insert(path.to_owned());
            }
        }
    }
    Ok(found)
}

/// Current worktree bytes of a path (a symlink reads as its target); `None` when absent.
fn worktree_bytes(git: &Git, path: &str) -> Option<Vec<u8>> {
    let full = git.root.join(path);
    let metadata = std::fs::symlink_metadata(&full).ok()?;
    if metadata.file_type().is_symlink() {
        return std::fs::read_link(&full)
            .ok()
            .map(|target| target.as_os_str().as_encoded_bytes().to_vec());
    }
    std::fs::read(&full).ok()
}

/// Whether the file's status or content changed at or after `since`.
fn changed_since(git: &Git, path: &str, since: SystemTime) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(metadata) = std::fs::symlink_metadata(git.root.join(path)) else {
        return true;
    };
    let changed = u64::try_from(metadata.ctime())
        .ok()
        .zip(u32::try_from(metadata.ctime_nsec()).ok())
        .map(|(seconds, nanos)| UNIX_EPOCH + Duration::new(seconds, nanos));
    [changed, metadata.modified().ok()]
        .into_iter()
        .any(|time| time.is_none_or(|time| time >= since))
}

/// Whether `full` holds exactly `expected` (`None`: absent). A regular file is compared in
/// chunks, so a large file is streamed rather than read into memory a second time.
fn holds(full: &Path, expected: Option<&[u8]>) -> bool {
    let metadata = match std::fs::symlink_metadata(full) {
        Ok(metadata) => metadata,
        Err(error) => return error.kind() == ErrorKind::NotFound && expected.is_none(),
    };
    let Some(expected) = expected else {
        return false;
    };
    if metadata.file_type().is_symlink() {
        return std::fs::read_link(full)
            .is_ok_and(|target| target.as_os_str().as_encoded_bytes() == expected);
    }
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(full) else {
        return false;
    };
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return offset == expected.len(),
            Ok(count) => {
                let end = offset + count;
                if end > expected.len() || buffer[..count] != expected[offset..end] {
                    return false;
                }
                offset = end;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

fn worktree_mode(git: &Git, path: &str) -> String {
    match std::fs::symlink_metadata(git.root.join(path)) {
        Ok(metadata) if metadata.file_type().is_symlink() => "120000".to_owned(),
        Ok(metadata) if metadata.permissions().mode() & 0o111 != 0 => "100755".to_owned(),
        _ => "100644".to_owned(),
    }
}

pub(crate) fn run(args: &cli::Discard) -> Result<Outcome> {
    if args.selectors.is_empty() && !args.whitespace_only {
        return Err(fail_hint(
            Code::Usage,
            "name what to discard",
            "pass selectors (PATH, hunk id, PATH~text) or --whitespace-only",
        ));
    }
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    git.keep_subprocesses();
    let session = Session::open(&repo, &git, true, false)?;
    let head_oid = session.head.oid.clone();
    require_checked_out(&args.opts, &session.head)?;
    // Anything that changes a file from here on is a concurrent edit.
    let started = SystemTime::now();
    let (status, changes) = view::read_changes(&git)?;
    if status.oid.as_deref() != Some(head_oid.as_str()) {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    if !status.unmerged.is_empty() {
        return Err(fail(Code::Dirty, "resolve the unmerged paths first"));
    }
    let selection = if args.selectors.is_empty() {
        whitespace_selection(&changes, None)?
    } else {
        let picked = selector::resolve(&git.root, &changes, &args.selectors)?;
        if args.whitespace_only {
            whitespace_selection(&changes, Some(&picked))?
        } else {
            picked
        }
    };

    // Gather HEAD entries and blobs, index entries and verified worktree hashes.
    let paths: Vec<String> = selection
        .files
        .keys()
        .map(|index| changes.files[*index].path.clone())
        .collect();
    let head_tree = git.commit_tree_of(&head_oid)?;
    let head_entries = git.ls_tree(&head_tree, &paths)?;
    let index = index_entries(&git, &paths)?;
    let placeholders = intent_to_add(&git, &paths)?;
    let modified: Vec<String> = selection
        .files
        .keys()
        .map(|index| &changes.files[*index])
        .filter(|file| {
            !matches!(file.kind, Kind::Untracked | Kind::Deleted) && file.new_oid.is_some()
        })
        .map(|file| file.path.clone())
        .collect();
    // Each file is read once; every check below is about these bytes, which the op keeps.
    let mut current: BTreeMap<String, Option<Vec<u8>>> = paths
        .iter()
        .map(|path| (path.clone(), worktree_bytes(&git, path)))
        .collect();
    // Untracked files have no object id: compare the digest taken when they were listed. Above
    // 16 MiB the listing records only the size, so the size must match and the file must not
    // have changed since this run started.
    for index in selection.files.keys() {
        let file = &changes.files[*index];
        if file.kind != Kind::Untracked {
            continue;
        }
        let bytes = current[&file.path].as_deref();
        let listed = match file.digest.strip_prefix("size-") {
            Some(size) => {
                bytes.is_some_and(|bytes| size == bytes.len().to_string())
                    && !changed_since(&git, &file.path, started)
            }
            None => bytes.is_some_and(|bytes| {
                use sha2::{Digest, Sha256};
                let digest: String = Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                digest == file.digest
            }),
        };
        if !listed {
            return Err(stale(&format!(
                "{} changed while aisc was working",
                file.path
            )));
        }
    }
    let hashes = hash_as_staged(&git, &modified)?;
    for (path, oid) in modified.iter().zip(&hashes) {
        let file = &changes.files[changes.index_of(path).expect("path from the change set")];
        if file.new_oid.as_deref() != Some(oid.as_str()) {
            return Err(stale(&format!("{path} changed while aisc was working")));
        }
    }
    let wanted: Vec<String> = paths
        .iter()
        .filter_map(|path| head_entries.get(path).map(|(_, oid)| oid.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let blobs: BTreeMap<String, Vec<u8>> = wanted
        .iter()
        .cloned()
        .zip(git.read_blobs(&wanted)?)
        .collect();
    // read_blobs reads blobs only; symlink targets are blobs too.

    let mut plan = Plan {
        verb: "discard".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    let mut hunks = 0;
    let mut partial_files = 0;
    for (file_index, selected) in &selection.files {
        let file = &changes.files[*file_index];
        hunks += hunks_in(file, selected);
        let current = current.remove(&file.path).flatten();
        let head = head_entries.get(&file.path);
        match (file.kind, selected) {
            (Kind::Modified, FileSel::Blocks(blocks)) => {
                partial_files += 1;
                let (write, change) = partial(
                    &git,
                    file,
                    blocks,
                    current,
                    head,
                    &blobs,
                    index.get(&file.path),
                )?;
                plan.worktree.push(write);
                plan.index.extend(change);
            }
            _ => {
                let before_mode = current.is_some().then(|| worktree_mode(&git, &file.path));
                let (after, mode) = match head {
                    Some((mode, oid)) => (Some(blobs[oid].clone()), mode.clone()),
                    None => (None, worktree_mode(&git, &file.path)),
                };
                plan.worktree.push(WorktreeWrite {
                    path: file.path.clone(),
                    before: current,
                    after,
                    mode,
                    before_mode,
                });
                if placeholders.contains(&file.path) {
                    // `git add -N` placeholder: counts as absent and is removed with the file.
                    plan.index.push(IndexChange {
                        path: file.path.clone(),
                        from: None,
                        to: head.cloned(),
                    });
                    continue;
                }
                let present = index.get(&file.path);
                if present != head {
                    plan.index.push(IndexChange {
                        path: file.path.clone(),
                        from: present.cloned(),
                        to: head.cloned(),
                    });
                }
            }
        }
    }
    let count = selection.files.len();
    let mut summary = format!(
        "discarded {count} file{}",
        if count == 1 { "" } else { "s" }
    );
    if partial_files > 0 {
        summary.push_str(&format!(
            " ({hunks} hunk{})",
            if hunks == 1 { "" } else { "s" }
        ));
    }
    plan.summary = summary.clone();
    // Last check before anything is written: the files still hold the bytes the op keeps.
    for write in &plan.worktree {
        if !holds(&git.root.join(&write.path), write.before.as_deref()) {
            return Err(stale(&format!(
                "{} changed while aisc was working",
                write.path
            )));
        }
    }
    let applied = session.apply(&plan)?;
    finish(
        &git,
        &super::amend::Finished {
            verb: "discard",
            result: format!("{summary}; op {}", applied.op),
            op: applied.op,
            quiet: args.opts.quiet,
            old_tip: head_oid.clone(),
            new_tip: head_oid,
            rewritten: &[],
            subjects: BTreeMap::new(),
            moved: &[],
            extra: json!({"discarded": paths}),
        },
    )
}

/// Reverts some blocks of a modified file; returns the worktree write and the index change.
fn partial(
    git: &Git,
    file: &FileChange,
    blocks: &BTreeSet<usize>,
    current: Option<Vec<u8>>,
    head: Option<&Entry>,
    blobs: &BTreeMap<String, Vec<u8>>,
    index: Option<&Entry>,
) -> Result<(WorktreeWrite, Option<IndexChange>)> {
    let current = current.ok_or_else(|| stale(&format!("{} disappeared", file.path)))?;
    let (_, head_oid) =
        head.ok_or_else(|| stale(&format!("{} is no longer tracked", file.path)))?;
    let lines = split_lines(&current);
    // Exactness gate: reverting every block must give the HEAD blob byte for byte.
    let every: Vec<&Block> = file.blocks.iter().collect();
    let reverted = unsplice(&lines, &every)
        .map_err(|error| fail(Code::Unsupported, format!("{}: {error}", file.path)))?;
    if join_lines(&reverted) != blobs[head_oid] {
        return Err(fail_hint(
            Code::Unsupported,
            format!("{}: Git filters or line-ending conversion apply", file.path),
            "select the whole file",
        ));
    }
    let chosen: Vec<&Block> = blocks.iter().map(|block| &file.blocks[*block]).collect();
    let new_lines =
        unsplice(&lines, &chosen).map_err(|error| stale(&format!("{}: {error}", file.path)))?;
    let bytes = join_lines(&new_lines);
    let write = WorktreeWrite {
        path: file.path.clone(),
        before: Some(current),
        after: Some(bytes.clone()),
        mode: file.new_mode.clone().unwrap_or_else(|| "100644".to_owned()),
        before_mode: Some(file.new_mode.clone().unwrap_or_else(|| "100644".to_owned())),
    };
    let change = match index {
        Some((mode, oid)) if file.new_oid.as_deref() == Some(oid.as_str()) => {
            // Staged exactly as the worktree: the staged copy follows.
            Some(IndexChange {
                path: file.path.clone(),
                from: Some((mode.clone(), oid.clone())),
                to: Some((mode.clone(), git.write_blob(&bytes, true)?)),
            })
        }
        Some((_, oid)) if oid == head_oid => None,
        Some((mode, oid)) => {
            // Staged differently: take the reverted hunks out of the staged copy too, or refuse.
            let staged = git
                .read_blobs(std::slice::from_ref(oid))?
                .pop()
                .unwrap_or_default();
            let placed =
                transplant(&split_lines(&staged), &chosen, Direction::Reverse).map_err(|_| {
                    fail_hint(
                        Code::Dirty,
                        format!(
                            "{} has staged changes that overlap the discarded hunks",
                            file.path
                        ),
                        format!("unstage it first: git restore --staged {}", file.path),
                    )
                })?;
            Some(IndexChange {
                path: file.path.clone(),
                from: Some((mode.clone(), oid.clone())),
                to: Some((mode.clone(), git.write_blob(&join_lines(&placed), true)?)),
            })
        }
        None => None,
    };
    Ok((write, change))
}
