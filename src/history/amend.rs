//! `aisc amend`: moves uncommitted hunks into several existing commits in one call (algorithm 6.4).
//! Also hosts the helpers that `split`/`uncommit` share for rewriting a commit chain.
use super::apply::{ApplyError, join_lines, splice, split_lines};
use super::model::{Block, ChangeSet, FileChange, Kind, Line};
use super::plumbing::{Entry, Favor, Git, HeadState, TreeEdit};
use super::render;
use super::rewrite::{IndexChange, Plan, RefMove, Session, dependent_moves, stale};
use super::selector::{self, FileSel, Selection};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------------------------
// Helpers shared with split.rs
// ---------------------------------------------------------------------------------------------

/// `--branch` must name the checked-out branch: these verbs act on the worktree's branch.
pub(super) fn require_checked_out(opts: &cli::MutationOpts, head: &HeadState) -> Result<()> {
    if let Some(branch) = &opts.branch {
        let current = head
            .symbolic
            .as_deref()
            .and_then(|name| name.strip_prefix("refs/heads/"));
        if current != Some(branch.as_str()) {
            return Err(fail_hint(
                Code::Usage,
                format!("'{branch}' is not the checked-out branch"),
                "this verb rewrites the checked-out branch; switch to it first (aisc switch)",
            ));
        }
    }
    Ok(())
}

pub(super) fn out_of_range(view: &View, oid: &str) -> anyhow::Error {
    let short: String = oid.chars().take(7).collect();
    match &view.base {
        Some(base) => fail_hint(
            Code::Selector,
            format!(
                "commit {short} is on {} or outside this branch (published history)",
                base.label
            ),
            format!("only commits above {} can be rewritten", base.label),
        ),
        None => fail_hint(
            Code::Selector,
            format!(
                "commit {short} is not among the last {} commits of this branch",
                view.commits.len()
            ),
            "run aisc log for the commits that can be rewritten",
        ),
    }
}

/// Position of `oid` in the (newest first) view, or the published-history error.
pub(super) fn position(view: &View, oid: &str) -> Result<usize> {
    view.commits
        .iter()
        .position(|commit| commit.oid == oid)
        .ok_or_else(|| out_of_range(view, oid))
}

/// Commits from view position `oldest` up to the tip, oldest first; every one must be linear.
pub(super) fn chain_to_tip(view: &View, oldest: usize) -> Result<Vec<CommitInfo>> {
    let chain: Vec<CommitInfo> = view.commits[..=oldest].iter().rev().cloned().collect();
    for commit in &chain {
        if commit.parents.len() > 1 {
            return Err(fail_hint(
                Code::Unsupported,
                format!("commit {} is a merge", commit.short),
                "history verbs need a linear range",
            ));
        }
    }
    Ok(chain)
}

pub(super) fn short_of(commit: &CommitInfo) -> String {
    format!("{} \"{}\"", commit.short, escape_line(&commit.subject))
}

/// Re-creates `chain` (oldest first) on `parent` with the given trees, keeping author, date and
/// message. A commit whose parent and tree are unchanged is reused. Returns the (old, new) pairs
/// of commits that changed and the new tip.
type Rebuilt = (Vec<(String, String)>, Option<String>);

pub(super) fn rebuild(
    git: &Git,
    chain: &[CommitInfo],
    trees: &[String],
    mut parent: Option<String>,
) -> Result<Rebuilt> {
    let mut rewritten = Vec::new();
    for (commit, tree) in chain.iter().zip(trees) {
        let new = if parent.as_ref() == commit.parents.first() && *tree == commit.tree {
            commit.oid.clone()
        } else {
            let meta = git.read_commit_meta(&commit.oid)?.with_current_committer();
            git.commit_tree(tree, parent.as_slice(), &meta.message, Some(&meta))?
        };
        if new != commit.oid {
            rewritten.push((commit.oid.clone(), new.clone()));
        }
        parent = Some(new);
    }
    Ok((rewritten, parent))
}

/// Adds the target ref move and the moves of local branches that pointed at rewritten commits.
pub(super) fn move_refs(
    git: &Git,
    view: &View,
    head: &HeadState,
    plan: &mut Plan,
    rewritten: &[(String, String)],
    new_tip: &str,
) -> Result<Vec<RefMove>> {
    plan.refs.push(RefMove {
        name: head.symbolic.clone().unwrap_or_else(|| "HEAD".to_owned()),
        old: Some(head.oid.clone()),
        new: Some(new_tip.to_owned()),
    });
    let skip: Vec<&str> = view.branch.as_deref().into_iter().collect();
    let dependents = dependent_moves(&view.locals, rewritten, &skip);
    if !dependents.is_empty() {
        let occupied = git.checked_out()?;
        for moved in &dependents {
            if occupied.contains(&moved.name) {
                return Err(fail_hint(
                    Code::State,
                    format!(
                        "branch {} is checked out in another worktree",
                        moved.name.trim_start_matches("refs/heads/")
                    ),
                    "it points at a commit this operation rewrites",
                ));
            }
        }
    }
    plan.refs.extend(dependents.iter().cloned());
    plan.rewritten = rewritten.to_vec();
    Ok(dependents)
}

/// Index rule changes for the paths whose entries differ between the old and new tip trees.
pub(super) fn index_changes(
    git: &Git,
    old_tree: &str,
    new_tree: &str,
    paths: &[String],
) -> Result<Vec<IndexChange>> {
    let unique: BTreeSet<&String> = paths.iter().collect();
    let paths: Vec<String> = unique.into_iter().cloned().collect();
    let old = git.ls_tree(old_tree, &paths)?;
    let new = git.ls_tree(new_tree, &paths)?;
    Ok(paths
        .iter()
        .filter_map(|path| {
            let (from, to) = (old.get(path).cloned(), new.get(path).cloned());
            (from != to).then(|| IndexChange {
                path: path.clone(),
                from,
                to,
            })
        })
        .collect())
}

pub(super) fn block_of(error: &ApplyError) -> usize {
    match error {
        ApplyError::Mismatch { block }
        | ApplyError::Overlap { block }
        | ApplyError::Missing { block }
        | ApplyError::Ambiguous { block } => *block,
    }
}

/// Short id of the hunk that contains `block`.
pub(super) fn hunk_name(file: &FileChange, block: usize) -> String {
    file.hunks
        .iter()
        .find(|hunk| hunk.blocks.contains(&block))
        .map_or_else(|| "?".to_owned(), |hunk| hunk.short.clone())
}

pub(super) struct Finished<'a> {
    pub verb: &'a str,
    pub result: String,
    pub op: String,
    pub quiet: bool,
    pub old_tip: String,
    pub new_tip: String,
    pub rewritten: &'a [(String, String)],
    pub subjects: BTreeMap<String, String>,
    pub moved: &'a [RefMove],
    pub extra: Value,
}

/// Reads the resulting view and renders the result line, moved branches and compact log.
pub(super) fn finish(git: &Git, done: &Finished<'_>) -> Result<Outcome> {
    let after = view::read(git, None, 20, true)?;
    let mut text = done.result.clone();
    let mut moved_json = Vec::new();
    for moved in done.moved {
        let name = moved.name.trim_start_matches("refs/heads/");
        let short = |oid: &Option<String>| -> String {
            oid.as_deref()
                .map(|value| value.chars().take(4).collect())
                .unwrap_or_default()
        };
        if moved.name.starts_with("refs/heads/") {
            text.push_str(&format!(
                "\nmoved branch {}: {}→{}",
                escape_line(name),
                short(&moved.old),
                short(&moved.new)
            ));
        }
        moved_json.push(json!({"ref": moved.name, "old": moved.old, "new": moved.new}));
    }
    let text = if done.quiet {
        text
    } else {
        format!("{text}\n{}", render::compact_text(&after))
    };
    let uncommitted: Vec<&str> = after
        .changes
        .as_ref()
        .map(|changes| {
            changes
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut value = json!({
        "op": done.op,
        "verb": done.verb,
        "branch": after.label(),
        "old": done.old_tip,
        "new": done.new_tip,
        "rewritten": done.rewritten.iter().map(|(old, new)| json!({
            "old": old,
            "new": new,
            "subject": done.subjects.get(old).cloned().unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "moved_refs": moved_json,
        "uncommitted": uncommitted,
        "log": render::log_json(&after, false),
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), done.extra.as_object()) {
        for (key, item) in extra {
            target.insert(key.clone(), item.clone());
        }
    }
    Ok(Outcome::with_text(value, text))
}

pub(super) fn hunks_in(file: &FileChange, selected: &FileSel) -> usize {
    match selected {
        FileSel::Whole => file.hunks.len().max(1),
        FileSel::Blocks(blocks) => file
            .hunks
            .iter()
            .filter(|hunk| hunk.blocks.clone().any(|block| blocks.contains(&block)))
            .count(),
    }
}

/// Final verification: what a tree-to-tree diff changes, per path, in comparable form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Sig {
    Text {
        removed: Vec<Line>,
        added: Vec<Line>,
    },
    Whole(&'static str),
}

pub(super) fn signature(git: &Git, from: &str, to: &str) -> Result<BTreeMap<String, Sig>> {
    let mut args = super::diff::commit_diff_args(false);
    args.push(from);
    args.push(to);
    let patch = git.run(&args)?;
    Ok(super::diff::parse_patch(&patch)?
        .into_iter()
        .map(|file| {
            let sig = if file.kind == Kind::Modified && !file.binary {
                Sig::Text {
                    removed: file.blocks.iter().flat_map(|b| b.removed.clone()).collect(),
                    added: file.blocks.iter().flat_map(|b| b.added.clone()).collect(),
                }
            } else {
                Sig::Whole(file.kind.label())
            };
            (file.path, sig)
        })
        .collect())
}

/// Lines added minus lines removed, position-insensitive.
fn net(sig: Option<&Sig>, routed: Option<&(Vec<Line>, Vec<Line>)>) -> BTreeMap<Line, i64> {
    let mut counts: BTreeMap<Line, i64> = BTreeMap::new();
    let mut add = |added: &[Line], removed: &[Line]| {
        for line in added {
            *counts.entry(line.clone()).or_default() += 1;
        }
        for line in removed {
            *counts.entry(line.clone()).or_default() -= 1;
        }
    };
    if let Some(Sig::Text { removed, added }) = sig {
        add(added, removed);
    }
    if let Some((removed, added)) = routed {
        add(added, removed);
    }
    counts.retain(|_, count| *count != 0);
    counts
}

pub(super) fn mismatch(path: &str, commit: &CommitInfo, names: &str) -> anyhow::Error {
    fail_hint(
        Code::Conflict,
        format!(
            "{path}: {} would not keep its own changes{names}; nothing was changed",
            short_of(commit)
        ),
        "the hunk's lines also occur elsewhere; target the commit that contains it, or use PATH:START-END",
    )
}

// ---------------------------------------------------------------------------------------------
// Exact line positions through the commits' own diffs (shared with split.rs)
// ---------------------------------------------------------------------------------------------

/// Each commit's own line changes, as Git records them (`git show`): entry `k` maps a path to the
/// blocks of `chain[k - 1] -> chain[k]`, with `None` for a change that is not line-level (binary,
/// added, deleted or type change). A path that the commit leaves alone is absent. Entry 0 is empty.
pub(super) type Recorded = Vec<BTreeMap<String, Option<Vec<Block>>>>;

pub(super) fn recorded(git: &Git, chain: &[CommitInfo], paths: &[String]) -> Result<Recorded> {
    let mut steps = vec![BTreeMap::new()];
    for pair in chain.windows(2) {
        let mut args = super::diff::commit_diff_args(false);
        args.extend([pair[0].tree.as_str(), pair[1].tree.as_str(), "--"]);
        args.extend(paths.iter().map(String::as_str));
        let mut step = BTreeMap::new();
        for file in super::diff::parse_patch(&git.run(&args)?)? {
            let blocks = match file.kind {
                Kind::Modified if !file.binary => Some(file.blocks),
                Kind::ModeOnly => Some(Vec::new()),
                _ => None,
            };
            step.insert(file.path, blocks);
        }
        steps.push(step);
    }
    Ok(steps)
}

/// Which side of each recorded diff a position starts on.
#[derive(Clone, Copy)]
pub(super) enum Way {
    /// From a commit to its parent (new side to old side).
    Back,
    /// From a parent to its commit.
    Forward,
}

/// Maps the lines `start..end` (an insertion point when empty) across one recorded diff. `None`
/// when the diff changes any of those lines or deletes lines between them, or when the insertion
/// point falls inside lines the diff changes or exactly where it deletes lines (either side of
/// the deleted lines would fit).
fn map_range(blocks: &[Block], way: Way, start: usize, end: usize) -> Option<usize> {
    let mut shift = 0isize;
    for block in blocks {
        let (from, from_len, to_len) = match way {
            Way::Back => (block.new_pos, block.added.len(), block.removed.len()),
            Way::Forward => (block.old_pos, block.removed.len(), block.added.len()),
        };
        let until = from + from_len;
        let clash = if start == end {
            (from < start && start < until) || (from == until && from == start)
        } else {
            start.max(from) < end.min(until) || (from == until && start < from && from < end)
        };
        if clash {
            return None;
        }
        if until <= start {
            shift += to_len as isize - from_len as isize;
        }
    }
    start.checked_add_signed(shift)
}

/// Where each range (by position on the first version) lands on the last version of `steps`,
/// following every diff's own alignment; `Err` holds the index of the first range that has no
/// single place. Deterministic, and independent of how a 3-way merge aligns the two versions.
pub(super) fn carry(
    ranges: &[(usize, usize)],
    steps: &[&[Block]],
    way: Way,
) -> Result<Vec<usize>, usize> {
    ranges
        .iter()
        .enumerate()
        .map(|(order, (start, end))| {
            let length = end - start;
            steps
                .iter()
                .try_fold(*start, |at, blocks| map_range(blocks, way, at, at + length))
                .ok_or(order)
        })
        .collect()
}

/// A hunk whose place in a commit depends on how Git aligns repeated lines.
pub(super) fn no_single_place(
    path: &str,
    hunk: &str,
    commit: &CommitInfo,
    hint: &str,
) -> anyhow::Error {
    fail_hint(
        Code::Conflict,
        format!(
            "hunk {hunk} ({path}) has no single place in {}: the lines around it repeat; nothing was changed",
            short_of(commit)
        ),
        hint,
    )
}

/// Per path the (removed, added) lines routed into a commit, and the paths to skip.
pub(super) type Routed = (BTreeMap<String, (Vec<Line>, Vec<Line>)>, BTreeSet<String>);

/// Every rewritten commit must differ from its new parent exactly as the original differed from
/// its parent (plus, at a routed commit, exactly the routed lines). `routed(j)` lists, per path,
/// the removed and added lines of the hunks routed into commit `j`, and the paths to skip.
pub(super) fn verify_amend(
    git: &Git,
    chain: &[CommitInfo],
    trees: &[String],
    parent_tree: &str,
    routed: &dyn Fn(usize) -> Routed,
) -> Result<()> {
    let (mut old_prev, mut new_prev) = (parent_tree.to_owned(), parent_tree.to_owned());
    for (j, commit) in chain.iter().enumerate() {
        let original = signature(git, &old_prev, &commit.tree)?;
        let rewritten = signature(git, &new_prev, &trees[j])?;
        let (lines, skip) = routed(j);
        let paths: BTreeSet<&String> = original.keys().chain(rewritten.keys()).collect();
        for path in paths {
            if skip.contains(path) {
                continue;
            }
            let ok = match lines.get(path) {
                None => original.get(path) == rewritten.get(path),
                // A file this commit creates has no line-level diff to compare here; its content
                // is checked line by line by `verify_placements`.
                Some(_) if matches!(original.get(path), Some(Sig::Whole(_))) => {
                    original.get(path) == rewritten.get(path)
                }
                Some(extra) => {
                    net(original.get(path), Some(extra)) == net(rewritten.get(path), None)
                }
            };
            if !ok {
                return Err(mismatch(path, commit, ""));
            }
        }
        old_prev.clone_from(&commit.tree);
        new_prev.clone_from(&trees[j]);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// amend
// ---------------------------------------------------------------------------------------------

struct Group {
    spec: String,
    tokens: Vec<String>,
}

fn parse_groups(args: &cli::Amend) -> Result<Vec<Group>> {
    if args.groups.is_empty() {
        return Err(fail_hint(
            Code::Usage,
            "name where each change goes",
            "aisc amend --into COMMIT SELECTOR… [--into COMMIT SELECTOR…]",
        ));
    }
    let mut groups = Vec::new();
    for group in &args.groups {
        let (spec, tokens) = group.split_first().expect("clap requires two values");
        if tokens.is_empty() {
            return Err(fail(
                Code::Usage,
                format!("--into {spec} needs at least one selector"),
            ));
        }
        groups.push(Group {
            spec: spec.clone(),
            tokens: tokens.to_vec(),
        });
    }
    Ok(groups)
}

/// Why a change could not be placed in a commit.
enum Miss {
    /// The path does not exist in that commit.
    Absent,
    /// An added file's path already exists in that commit.
    Exists,
    /// A whole-file change needs the commit to still hold the HEAD version.
    Differs,
    /// The block (index into the file's blocks) has no unique place.
    Apply(usize),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Miss {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

struct Context<'a> {
    git: &'a Git,
    changes: &'a ChangeSet,
    head: BTreeMap<String, Entry>,
    hashed: BTreeMap<String, String>,
    old_lines: BTreeMap<String, Vec<Line>>,
}

/// Where each file's selected parts go: (chain position, selection).
type Routes = BTreeMap<usize, Vec<(usize, FileSel)>>;

fn chosen<'a>(file: &'a FileChange, blocks: &BTreeSet<usize>) -> Vec<&'a Block> {
    blocks.iter().map(|block| &file.blocks[*block]).collect()
}

/// New entry for one file in a commit whose current entry is `current`.
fn place(
    context: &Context<'_>,
    file_index: usize,
    blocks: &BTreeSet<usize>,
    whole: bool,
    current: Option<&Entry>,
) -> Result<Option<Entry>, Miss> {
    let file = &context.changes.files[file_index];
    let head = context.head.get(&file.path);
    let new_mode = file.new_mode.clone().unwrap_or_else(|| "100644".to_owned());
    match file.kind {
        Kind::Modified => {
            let (mode, oid) = current.ok_or(Miss::Absent)?;
            let blocks_here = chosen(file, blocks);
            let mode = if whole && file.mode_changed() {
                new_mode
            } else {
                mode.clone()
            };
            let result = if file.old_oid.as_ref() == Some(oid) {
                if whole {
                    context.hashed[&file.path].clone()
                } else {
                    let lines = splice(&context.old_lines[&file.path], &blocks_here)
                        .map_err(|error| Miss::Apply(nth(blocks, block_of(&error))))?;
                    context.git.write_blob(&join_lines(&lines), true)?
                }
            } else {
                // A different version of the file: 3-way merge of HEAD -> HEAD+hunks onto it.
                // The merge conflicts when a hunk touches lines this commit does not have, so a
                // hunk can never land in a look-alike place.
                let base = file.old_oid.as_deref().ok_or(Miss::Absent)?;
                let theirs = if whole {
                    context.hashed[&file.path].clone()
                } else {
                    let lines = splice(&context.old_lines[&file.path], &blocks_here)
                        .map_err(|error| Miss::Apply(nth(blocks, block_of(&error))))?;
                    context.git.write_blob(&join_lines(&lines), true)?
                };
                match merge3(context.git, base, oid, &theirs)? {
                    Some(bytes) => context.git.write_blob(&bytes, true)?,
                    None => {
                        return Err(Miss::Apply(first_conflicting(context, file, blocks, oid)?));
                    }
                }
            };
            Ok(Some((mode, result)))
        }
        Kind::Untracked | Kind::Added => {
            if current.is_some() {
                return Err(Miss::Exists);
            }
            Ok(Some((new_mode, context.hashed[&file.path].clone())))
        }
        Kind::Deleted => {
            if current != head {
                return Err(Miss::Differs);
            }
            Ok(None)
        }
        Kind::ModeOnly => {
            if current != head {
                return Err(Miss::Differs);
            }
            let (_, oid) = head.ok_or(Miss::Absent)?;
            Ok(Some((new_mode, oid.clone())))
        }
        Kind::Binary | Kind::TypeChange | Kind::Symlink => {
            if current != head {
                return Err(Miss::Differs);
            }
            Ok(Some((new_mode, context.hashed[&file.path].clone())))
        }
    }
}

/// Applies base -> `theirs` onto `ours`; `None` when the merge conflicts.
pub(super) fn merge3(git: &Git, base: &str, ours: &str, theirs: &str) -> Result<Option<Vec<u8>>> {
    let (bytes, conflicts) = git.merge_file(
        base,
        ours,
        theirs,
        ["ours", "base", "theirs"],
        Favor::None,
        false,
    )?;
    Ok((conflicts == 0).then_some(bytes))
}

/// The first block that cannot be merged on its own (the first one when only the set conflicts).
fn first_conflicting(
    context: &Context<'_>,
    file: &FileChange,
    blocks: &BTreeSet<usize>,
    ours: &str,
) -> Result<usize> {
    let base = file.old_oid.as_deref().unwrap_or_default();
    for block in blocks {
        let lines = splice(&context.old_lines[&file.path], &[&file.blocks[*block]])
            .map_err(|error| fail(Code::Unsupported, format!("{}: {error}", file.path)))?;
        let theirs = context.git.write_blob(&join_lines(&lines), true)?;
        if merge3(context.git, base, ours, &theirs)?.is_none() {
            return Ok(*block);
        }
    }
    Ok(blocks.iter().copied().next().unwrap_or(0))
}

fn nth(blocks: &BTreeSet<usize>, order: usize) -> usize {
    blocks.iter().copied().nth(order).unwrap_or(0)
}

/// What a file contributes to the commit at chain position `j`.
fn selection_at(
    file: &FileChange,
    routes: &[(usize, FileSel)],
    j: usize,
) -> (BTreeSet<usize>, bool) {
    let mut blocks = BTreeSet::new();
    let mut whole = false;
    for (target, selected) in routes {
        if *target > j {
            continue;
        }
        match selected {
            FileSel::Whole => {
                whole = true;
                blocks.extend(0..file.blocks.len());
            }
            FileSel::Blocks(set) => blocks.extend(set),
        }
    }
    (blocks, whole)
}

#[allow(clippy::too_many_arguments)]
fn explain(
    context: &Context<'_>,
    chain: &[CommitInfo],
    routes: &Routes,
    file_index: usize,
    j: usize,
    miss: Miss,
) -> anyhow::Error {
    let file = &context.changes.files[file_index];
    let here = &chain[j];
    let target_of = |block: usize| -> usize {
        routes[&file_index]
            .iter()
            .find(|(_, selected)| match selected {
                FileSel::Whole => true,
                FileSel::Blocks(set) => set.contains(&block),
            })
            .map_or(0, |(target, _)| *target)
    };
    match miss {
        Miss::Other(error) => error,
        Miss::Absent => {
            let later = (j + 1..chain.len()).find(|candidate| {
                context
                    .git
                    .ls_tree(&chain[*candidate].tree, std::slice::from_ref(&file.path))
                    .is_ok_and(|entries| entries.contains_key(&file.path))
            });
            match later {
                Some(candidate) => fail_hint(
                    Code::Conflict,
                    format!(
                        "{} is added by {}; it does not exist in {}",
                        file.path,
                        short_of(&chain[candidate]),
                        short_of(here)
                    ),
                    format!("target {} or later", chain[candidate].short),
                ),
                None => fail(
                    Code::Conflict,
                    format!("{} does not exist in {}", file.path, short_of(here)),
                ),
            }
        }
        Miss::Exists => fail_hint(
            Code::Conflict,
            format!(
                "{} already exists in {}; an untracked file cannot go into a commit before it",
                file.path,
                short_of(here)
            ),
            "target the commit that comes before it exists, or a later one",
        ),
        Miss::Differs => fail_hint(
            Code::Conflict,
            format!("{} is changed again by {}", file.path, short_of(here)),
            "target that commit or a later one",
        ),
        Miss::Apply(block) => {
            let name = hunk_name(file, block);
            let target = target_of(block);
            if j > target {
                return fail_hint(
                    Code::Conflict,
                    format!(
                        "hunk {name} ({}) cannot be carried through {}",
                        file.path,
                        short_of(here)
                    ),
                    format!("target {} or later", here.short),
                );
            }
            // The first later commit where this block alone finds its place.
            let later = (j + 1..chain.len()).find(|candidate| {
                let Ok(entries) = context
                    .git
                    .ls_tree(&chain[*candidate].tree, std::slice::from_ref(&file.path))
                else {
                    return false;
                };
                let Some((_, oid)) = entries.get(&file.path) else {
                    return false;
                };
                let Ok(lines) = splice(&context.old_lines[&file.path], &[&file.blocks[block]])
                else {
                    return false;
                };
                let Ok(theirs) = context.git.write_blob(&join_lines(&lines), true) else {
                    return false;
                };
                let base = file.old_oid.as_deref().unwrap_or_default();
                merge3(context.git, base, oid, &theirs).is_ok_and(|merged| merged.is_some())
            });
            match later {
                Some(candidate) => fail_hint(
                    Code::Conflict,
                    format!(
                        "hunk {name} ({}) edits lines from {}",
                        file.path,
                        short_of(&chain[candidate])
                    ),
                    format!("target {} or later", chain[candidate].short),
                ),
                None => fail_hint(
                    Code::Conflict,
                    format!(
                        "hunk {name} ({}) does not apply to {} or any later commit",
                        file.path,
                        short_of(here)
                    ),
                    "select a different hunk, or use PATH:START-END",
                ),
            }
        }
    }
}

/// A file placed in one commit: (file index, entry before, entry after).
type Placed = (usize, Option<Entry>, Option<Entry>);

/// Position check, independent of how the 3-way merge aligned the two versions: where a commit
/// holds another version of a modified file than HEAD, the merged file must be exactly that
/// version with the selected blocks carried to it through the later commits' own diffs. When
/// repeated lines let the merge choose a look-alike place, the two differ and nothing changes.
/// This also checks, line by line, a file that the target commit creates.
fn verify_placements(
    context: &Context<'_>,
    chain: &[CommitInfo],
    routes: &Routes,
    placed: &[Vec<Placed>],
) -> Result<()> {
    let files = &context.changes.files;
    // (chain position, file index, version in the commit, merged version)
    let mut pending: Vec<(usize, usize, &String, &String)> = Vec::new();
    for (j, here) in placed.iter().enumerate() {
        for (index, before, after) in here {
            let file = &files[*index];
            if let (Kind::Modified, Some((_, ours)), Some((_, merged))) = (file.kind, before, after)
                && file.old_oid.as_ref() != Some(ours)
            {
                pending.push((j, *index, ours, merged));
            }
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let paths: Vec<String> = pending
        .iter()
        .map(|(_, index, _, _)| files[*index].path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let steps = recorded(context.git, chain, &paths)?;
    let oids: Vec<String> = pending
        .iter()
        .flat_map(|(_, _, ours, merged)| [(*ours).clone(), (*merged).clone()])
        .collect();
    let blobs = context.git.read_blobs(&oids)?;
    for ((j, index, _, _), pair) in pending.iter().zip(blobs.chunks(2)) {
        let file = &files[*index];
        let blocks: Vec<usize> = selection_at(file, &routes[index], *j)
            .0
            .into_iter()
            .collect();
        let refuse = |order: usize| {
            let block = blocks.get(order).copied().unwrap_or_default();
            no_single_place(
                &file.path,
                &hunk_name(file, block),
                &chain[*j],
                "target the commit that last changed those lines, or a later one",
            )
        };
        // HEAD back to this commit: the diffs of every later commit, newest first.
        let later: Option<Vec<&[Block]>> = (j + 1..chain.len())
            .rev()
            .map(|k| match steps[k].get(&file.path) {
                None => Some(&[][..]),
                Some(blocks) => blocks.as_deref(),
            })
            .collect();
        let later = later.ok_or_else(|| refuse(0))?;
        let ranges: Vec<(usize, usize)> = blocks
            .iter()
            .map(|block| {
                let block = &file.blocks[*block];
                (block.old_pos, block.old_pos + block.removed.len())
            })
            .collect();
        let starts = carry(&ranges, &later, Way::Back).map_err(refuse)?;
        let moved: Vec<Block> = blocks
            .iter()
            .zip(starts)
            .map(|(block, old_pos)| Block {
                old_pos,
                ..file.blocks[*block].clone()
            })
            .collect();
        let moved: Vec<&Block> = moved.iter().collect();
        let expected =
            splice(&split_lines(&pair[0]), &moved).map_err(|error| refuse(block_of(&error)))?;
        if join_lines(&expected) != pair[1] {
            return Err(refuse(0));
        }
    }
    Ok(())
}

pub(crate) fn run(args: &cli::Amend) -> Result<Outcome> {
    let groups = parse_groups(args)?;
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, true, false)?;
    let head_oid = session.head.oid.clone();
    require_checked_out(&args.opts, &session.head)?;
    let view = view::read(&git, None, 1_000, true)?;
    if view.tip != head_oid {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    let (Some(changes), Some(status)) = (&view.changes, &view.status) else {
        return Err(fail(Code::State, "uncommitted changes are unavailable"));
    };
    if status.oid.as_deref() != Some(head_oid.as_str()) {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    if !status.unmerged.is_empty() {
        return Err(fail(Code::Dirty, "resolve the unmerged paths first"));
    }

    // Targets and selections.
    let mut positions = Vec::new();
    let mut selections = Vec::new();
    for group in &groups {
        let oid = view.resolve(&git, &group.spec)?;
        positions.push(position(&view, &oid)?);
        selections.push(selector::resolve(&git.root, changes, &group.tokens)?);
    }
    selector::check_disjoint(changes, &selections)?;
    let oldest = positions.iter().copied().max().expect("at least one group");
    let chain = chain_to_tip(&view, oldest)?;
    let chain_position = |view_position: usize| oldest - view_position;

    let mut routes: Routes = BTreeMap::new();
    for (selection, view_position) in selections.iter().zip(&positions) {
        for (file, selected) in &selection.files {
            routes
                .entry(*file)
                .or_default()
                .push((chain_position(*view_position), selected.clone()));
        }
    }

    // Everything the placements need, gathered with few processes.
    let tip_tree = chain.last().expect("chain has the tip").tree.clone();
    let touched: Vec<String> = routes
        .keys()
        .map(|index| changes.files[*index].path.clone())
        .collect();
    let head_entries = git.ls_tree(&tip_tree, &touched)?;
    let mut to_hash = Vec::new();
    for (index, file_routes) in &routes {
        let file = &changes.files[*index];
        let whole = file_routes
            .iter()
            .any(|(_, selected)| *selected == FileSel::Whole);
        let needs_bytes = matches!(
            file.kind,
            Kind::Untracked | Kind::Added | Kind::Binary | Kind::TypeChange | Kind::Symlink
        ) || (file.kind == Kind::Modified && whole);
        if needs_bytes {
            to_hash.push(file.path.clone());
        }
    }
    let hashes = git.hash_worktree(&to_hash)?;
    let mut hashed = BTreeMap::new();
    for (path, oid) in to_hash.iter().zip(hashes) {
        let file = &changes.files[changes.index_of(path).expect("path from the change set")];
        if file.kind != Kind::Untracked && file.new_oid.as_deref().is_some_and(|known| known != oid)
        {
            return Err(stale(&format!("{path} changed while aisc was working")));
        }
        hashed.insert(path.clone(), oid);
    }
    let modified: Vec<usize> = routes
        .keys()
        .copied()
        .filter(|index| changes.files[*index].kind == Kind::Modified)
        .collect();
    let mut old_oids = Vec::new();
    for index in &modified {
        let file = &changes.files[*index];
        old_oids.push(file.old_oid.clone().ok_or_else(|| {
            fail(
                Code::Unsupported,
                format!("{}: cannot select part of this change", file.path),
            )
        })?);
    }
    let old_blobs = git.read_blobs(&old_oids)?;
    let mut old_lines = BTreeMap::new();
    for (index, blob) in modified.iter().zip(old_blobs) {
        let file = &changes.files[*index];
        let lines = split_lines(&blob);
        let partial = routes[index]
            .iter()
            .any(|(_, selected)| matches!(selected, FileSel::Blocks(_)));
        if partial {
            // Exactness gate: all blocks together must reproduce the worktree blob exactly.
            let every: Vec<&Block> = file.blocks.iter().collect();
            let rebuilt = splice(&lines, &every)
                .map_err(|error| fail(Code::Unsupported, format!("{}: {error}", file.path)))?;
            let oid = git.write_blob(&join_lines(&rebuilt), true)?;
            if file.new_oid.as_deref() != Some(oid.as_str()) {
                return Err(fail_hint(
                    Code::Unsupported,
                    format!("{}: Git filters or line-ending conversion apply", file.path),
                    "select the whole file",
                ));
            }
        }
        old_lines.insert(file.path.clone(), lines);
    }
    let context = Context {
        git: &git,
        changes,
        head: head_entries,
        hashed,
        old_lines,
    };

    // One tree per commit from the oldest target to the tip.
    let mut trees = Vec::new();
    // Per commit: (file, entry before, entry after) of every file placed there.
    let mut placed: Vec<Vec<Placed>> = Vec::new();
    let mut memo: BTreeMap<(String, usize, Vec<usize>, bool), Option<Entry>> = BTreeMap::new();
    for (j, commit) in chain.iter().enumerate() {
        let here: Vec<usize> = routes
            .iter()
            .filter(|(_, file_routes)| file_routes.iter().any(|(target, _)| *target <= j))
            .map(|(index, _)| *index)
            .collect();
        let paths: Vec<String> = here
            .iter()
            .map(|index| changes.files[*index].path.clone())
            .collect();
        let entries = git.ls_tree(&commit.tree, &paths)?;
        let mut edits = Vec::new();
        let mut placed_here = Vec::new();
        for index in here {
            let file = &changes.files[index];
            let (blocks, whole) = selection_at(file, &routes[&index], j);
            let current = entries.get(&file.path);
            let key = (
                current
                    .map(|(mode, oid)| format!("{mode} {oid}"))
                    .unwrap_or_default(),
                index,
                blocks.iter().copied().collect::<Vec<_>>(),
                whole,
            );
            let entry = match memo.get(&key) {
                Some(entry) => entry.clone(),
                None => {
                    let entry = place(&context, index, &blocks, whole, current)
                        .map_err(|miss| explain(&context, &chain, &routes, index, j, miss))?;
                    memo.insert(key, entry.clone());
                    entry
                }
            };
            placed_here.push((index, current.cloned(), entry.clone()));
            edits.push(TreeEdit {
                path: file.path.clone(),
                entry,
            });
        }
        trees.push(git.tree_with(&commit.tree, &edits)?);
        placed.push(placed_here);
    }
    verify_placements(&context, &chain, &routes, &placed)?;

    // Final verification, at line level, before any commit is created.
    let parent_tree = match chain[0].parents.first() {
        Some(parent) => git.commit_tree_of(parent)?,
        None => git.empty_tree()?,
    };
    let routed_at = |j: usize| {
        let mut lines: BTreeMap<String, (Vec<Line>, Vec<Line>)> = BTreeMap::new();
        let mut skip = BTreeSet::new();
        for (index, file_routes) in &routes {
            let file = &changes.files[*index];
            for (target, selected) in file_routes.iter().filter(|(target, _)| *target == j) {
                let _ = target;
                if file.kind != Kind::Modified {
                    skip.insert(file.path.clone());
                    continue;
                }
                let blocks: Vec<usize> = match selected {
                    FileSel::Whole => (0..file.blocks.len()).collect(),
                    FileSel::Blocks(set) => set.iter().copied().collect(),
                };
                let entry = lines.entry(file.path.clone()).or_default();
                for block in blocks {
                    entry.0.extend(file.blocks[block].removed.clone());
                    entry.1.extend(file.blocks[block].added.clone());
                }
            }
        }
        (lines, skip)
    };
    verify_amend(&git, &chain, &trees, &parent_tree, &routed_at)?;

    let (rewritten, tip) = rebuild(&git, &chain, &trees, chain[0].parents.first().cloned())?;
    let tip = tip.expect("chain is not empty");
    let new_tip_tree = trees.last().expect("chain is not empty").clone();

    let mut plan = Plan {
        verb: "amend".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    plan.index = index_changes(&git, &tip_tree, &new_tip_tree, &touched)?;
    let moved = move_refs(&git, &view, &session.head, &mut plan, &rewritten, &tip)?;

    // Result line: counts per target commit, oldest first (ids as `aisc log` showed them).
    let mut per_commit: BTreeMap<usize, usize> = BTreeMap::new();
    for (selection, view_position) in selections.iter().zip(&positions) {
        for (file, selected) in &selection.files {
            *per_commit
                .entry(chain_position(*view_position))
                .or_default() += hunks_in(&changes.files[*file], selected);
        }
    }
    let mut parts = Vec::new();
    for (index, (chain_index, count)) in per_commit.iter().enumerate() {
        let short = &chain[*chain_index].short;
        if index == 0 {
            parts.push(format!(
                "{short} +{count} hunk{}",
                if *count == 1 { "" } else { "s" }
            ));
        } else {
            parts.push(format!("{short} +{count}"));
        }
    }
    let summary = format!("amended {}; rewrote {}", parts.join(", "), rewritten.len());
    plan.summary = summary.clone();
    let old_tip = head_oid.clone();
    let applied = session.apply(&plan)?;
    let subjects = chain
        .iter()
        .map(|commit| (commit.oid.clone(), commit.subject.clone()))
        .collect();
    finish(
        &git,
        &Finished {
            verb: "amend",
            result: format!("{summary}; op {}", applied.op),
            op: applied.op,
            quiet: args.opts.quiet,
            old_tip,
            new_tip: tip,
            rewritten: &rewritten,
            subjects,
            moved: &moved,
            extra: json!({}),
        },
    )
}

/// Selection helpers used by split: `Selection` over the files of a commit diff.
pub(super) fn selection_hunks(changes: &ChangeSet, selection: &Selection) -> usize {
    selection
        .files
        .iter()
        .map(|(file, selected)| hunks_in(&changes.files[*file], selected))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block that turns `removed` lines at `old_pos` into `added` lines at `new_pos`.
    fn block(old_pos: usize, removed: usize, new_pos: usize, added: usize) -> Block {
        Block {
            old_pos,
            new_pos,
            removed: vec![b"r\n".to_vec(); removed],
            added: vec![b"a\n".to_vec(); added],
            before: Vec::new(),
            after: Vec::new(),
            at_start: false,
            at_eof: false,
        }
    }

    #[test]
    fn ranges_shift_past_earlier_changes_and_stay_before_later_ones() {
        // Lines 2..4 became 2..5 (one line more); line 8 was deleted.
        let diff = [block(2, 2, 2, 3), block(7, 1, 8, 0)];
        assert_eq!(map_range(&diff, Way::Back, 0, 2), Some(0));
        assert_eq!(map_range(&diff, Way::Back, 5, 6), Some(4));
        assert_eq!(map_range(&diff, Way::Back, 9, 10), Some(9));
        assert_eq!(map_range(&diff, Way::Forward, 4, 5), Some(5));
        assert_eq!(map_range(&diff, Way::Forward, 9, 9), Some(9));
        // Lines right next to a change keep their single place.
        assert_eq!(map_range(&diff, Way::Back, 1, 2), Some(1));
        assert_eq!(map_range(&diff, Way::Back, 5, 8), Some(4));
    }

    #[test]
    fn ranges_touching_changed_lines_have_no_single_place() {
        let diff = [block(2, 2, 2, 3), block(7, 1, 8, 0)];
        // Overlaps the changed lines.
        assert_eq!(map_range(&diff, Way::Back, 4, 6), None);
        // Spans the deleted line.
        assert_eq!(map_range(&diff, Way::Back, 7, 9), None);
        // An insertion inside changed lines, or exactly where lines were deleted.
        assert_eq!(map_range(&diff, Way::Back, 3, 3), None);
        assert_eq!(map_range(&diff, Way::Back, 8, 8), None);
        // At the edges of changed lines it is still unique.
        assert_eq!(map_range(&diff, Way::Back, 2, 2), Some(2));
        assert_eq!(map_range(&diff, Way::Back, 5, 5), Some(4));
    }

    #[test]
    fn carry_follows_every_step_and_names_the_first_range_without_a_place() {
        let first = [block(0, 0, 0, 2)];
        let second = [block(5, 1, 5, 1)];
        let steps: Vec<&[Block]> = vec![&first, &second];
        assert_eq!(
            carry(&[(1, 2), (3, 3)], &steps, Way::Forward),
            Ok(vec![3, 5])
        );
        assert_eq!(carry(&[(1, 2), (3, 4)], &steps, Way::Forward), Err(1));
        assert_eq!(carry(&[(6, 7)], &[&second, &first], Way::Back), Ok(vec![4]));
    }
}
