//! `aisc amend`: moves uncommitted hunks into several existing commits in one call (algorithm 6.4).
//! Also hosts the helpers that `split`/`uncommit` share for rewriting a commit chain.
use super::apply::{ApplyError, join_lines, splice, split_lines, unsplice};
use super::model::{Block, ChangeSet, FileChange, Kind, Line};
use super::plumbing::{Entry, Git, HeadState, TreeEdit};
use super::render;
use super::rewrite::{IndexChange, Plan, RefMove, Session, dependent_moves, stale};
use super::selector::{self, FileSel, Selection};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use crate::store::Kind as ObjectKind;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};

// ---------------------------------------------------------------------------------------------
// Helpers shared with split.rs
// ---------------------------------------------------------------------------------------------

fn is_checked_out(branch: &str, head: &HeadState) -> bool {
    head.symbolic
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        == Some(branch)
}

/// `--into COMMIT` on another local branch: one group, folded in from this worktree.
fn elsewhere(
    args: &cli::Amend,
    groups: &[Group],
    branch: &str,
    git: &Git,
    session: Session,
) -> Result<Outcome> {
    let [group] = groups else {
        return Err(fail_hint(
            Code::Usage,
            format!("amending another branch ({branch}) takes one --into"),
            "run aisc amend --branch BRANCH --into COMMIT SEL... once per commit",
        ));
    };
    super::amend_elsewhere::run(args, branch, &group.spec, &group.tokens, git, session)
}

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
    let after = super::summary::after(git, None, true)?;
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
        super::summary::text(&after, &text)
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
    let patch = git.tree_patch(from, to, &[])?;
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
    let pairs: Vec<(String, String)> = chain
        .windows(2)
        .map(|pair| (pair[0].tree.clone(), pair[1].tree.clone()))
        .collect();
    git.prefetch_patches(&pairs, paths);
    for pair in chain.windows(2) {
        let mut step = BTreeMap::new();
        let patch = git.tree_patch(&pair[0].tree, &pair[1].tree, paths)?;
        for file in super::diff::parse_patch(&patch)? {
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

/// Follows the lines `start..start + len` of `path` across the recorded diffs of the commits
/// `steps` (chain positions, in that order). `Err(k)` names the first commit whose own change
/// leaves them no single place. Deterministic: it follows each diff's own alignment, as `git show`
/// displays it, never a 3-way merge's realignment of two distant versions.
pub(super) fn follow(
    recorded: &Recorded,
    path: &str,
    steps: impl IntoIterator<Item = usize>,
    way: Way,
    start: usize,
    len: usize,
) -> Result<usize, usize> {
    let mut at = start;
    for k in steps {
        match recorded[k].get(path) {
            None => {}
            Some(Some(blocks)) => at = map_range(blocks, way, at, at + len).ok_or(k)?,
            Some(None) => return Err(k),
        }
    }
    Ok(at)
}

/// Sets each block's `new_pos` for `blocks` taken as one diff from the version they apply to.
fn with_new_positions(mut blocks: Vec<Block>) -> Vec<Block> {
    let mut shift = 0isize;
    for block in &mut blocks {
        block.new_pos = block.old_pos.saturating_add_signed(shift);
        shift += block.added.len() as isize - block.removed.len() as isize;
    }
    blocks
}

/// One file of one rewritten commit, for [`proves`].
pub(super) struct Rewrite<'a> {
    /// The commit's original version.
    pub ours: &'a [Line],
    /// Original -> rewritten version, as blocks positioned on the original (sorted).
    pub delta: Vec<Block>,
    /// Per block of `delta`: whether this commit gains it (amend's routed hunks); the others its
    /// rewritten parent already has.
    pub fresh: Vec<bool>,
    /// The commit's own recorded blocks for the file (`None`: not a line-level change).
    pub own: Option<&'a [Block]>,
    pub rewritten: &'a [u8],
    /// The rewritten parent's version; `None` when the parent is not rewritten for this file.
    pub parent: Option<&'a [u8]>,
}

/// Position-aware proof that a rewritten commit changes this file exactly as intended:
/// - the rewritten version is the original with `delta` applied at its positions;
/// - taking out the fresh blocks and then the commit's own blocks, each moved only past the delta
///   lines, gives exactly the rewritten parent (or, with no rewritten parent, the original).
///
/// So the rewritten commit's diff is its own diff, line for line and place for place, plus the
/// fresh blocks. Nothing depends on how a diff of the rewritten versions would be aligned.
pub(super) fn proves(file: &Rewrite<'_>) -> bool {
    let delta = with_new_positions(file.delta.clone());
    let all: Vec<&Block> = delta.iter().collect();
    let Ok(built) = splice(file.ours, &all) else {
        return false;
    };
    if join_lines(&built) != file.rewritten {
        return false;
    }
    let fresh: Vec<&Block> = delta
        .iter()
        .zip(&file.fresh)
        .filter_map(|(block, fresh)| fresh.then_some(block))
        .collect();
    let Ok(without) = unsplice(&built, &fresh) else {
        return false;
    };
    let kept = with_new_positions(
        delta
            .iter()
            .zip(&file.fresh)
            .filter(|(_, fresh)| !**fresh)
            .map(|(block, _)| block.clone())
            .collect(),
    );
    let Some(parent) = file.parent else {
        return kept.is_empty() && without == file.ours;
    };
    let kept_refs: Vec<&Block> = kept.iter().collect();
    if splice(file.ours, &kept_refs).ok().as_ref() != Some(&without) {
        return false;
    }
    let Some(own) = file.own else {
        return false;
    };
    // The commit's own blocks, from its original version to the one that also holds `kept`.
    let moved: Option<Vec<Block>> = own
        .iter()
        .map(|block| {
            let end = block.new_pos + block.added.len();
            map_range(&kept, Way::Forward, block.new_pos, end).map(|new_pos| Block {
                new_pos,
                ..block.clone()
            })
        })
        .collect();
    let Some(moved) = moved else {
        return false;
    };
    let moved: Vec<&Block> = moved.iter().collect();
    unsplice(&without, &moved).is_ok_and(|lines| join_lines(&lines) == parent)
}

/// Quotes a selector or commit id for a shell when it needs it.
pub(super) fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/:~+-@,=".contains(&byte));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
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
    let mut pairs = Vec::new();
    for (j, commit) in chain.iter().enumerate() {
        pairs.push((old_prev.clone(), commit.tree.clone()));
        pairs.push((new_prev.clone(), trees[j].clone()));
        old_prev.clone_from(&commit.tree);
        new_prev.clone_from(&trees[j]);
    }
    git.prefetch_patches(&pairs, &[]);
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
    /// The block (index into the file's blocks) does not apply.
    Apply(usize),
    /// The block's lines are changed by the commit at that chain position.
    Clash(usize, usize),
    /// The file's mode is changed by the commit at that chain position.
    Mode(usize),
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
    /// The chain's recorded diffs of the modified files.
    steps: Recorded,
    /// Modified files whose blocks rebuild the worktree file byte for byte.
    exact: BTreeSet<String>,
    /// Chain position of the tip.
    tip: usize,
    /// For a whole file whose mode changes: the newest commit (chain position) that changes the
    /// file's mode itself. The mode change can only go there or later.
    mode_last: BTreeMap<usize, usize>,
    /// Files whose blocks were rebuilt from the stored (filtered) blob, not the listing.
    rebuilt: BTreeSet<usize>,
}

impl Context<'_> {
    /// The blocks moved from HEAD to their places in the commit at chain position `j`, through
    /// every later commit's own diff. `Err((block, k))`: commit `k` changes that block's lines.
    fn carried(
        &self,
        file: &FileChange,
        blocks: &BTreeSet<usize>,
        j: usize,
    ) -> Result<Vec<Block>, (usize, usize)> {
        blocks
            .iter()
            .map(|index| {
                let block = &file.blocks[*index];
                let steps = (j + 1..=self.tip).rev();
                follow(
                    &self.steps,
                    &file.path,
                    steps,
                    Way::Back,
                    block.old_pos,
                    block.removed.len(),
                )
                .map(|old_pos| Block {
                    old_pos,
                    ..block.clone()
                })
                .map_err(|k| (*index, k))
            })
            .collect()
    }
}

/// What a placement produced: the new entry and, for a text file, the placed blocks (positions on
/// the commit's version).
type Placement = (Option<Entry>, Option<Vec<Block>>);

/// Where each file's selected parts go: (chain position, selection).
type Routes = BTreeMap<usize, Vec<(usize, FileSel)>>;

fn chosen<'a>(file: &'a FileChange, blocks: &BTreeSet<usize>) -> Vec<&'a Block> {
    blocks.iter().map(|block| &file.blocks[*block]).collect()
}

/// New entry for one file in the commit at chain position `j`, whose entry is `current`.
fn place(
    context: &Context<'_>,
    file_index: usize,
    (blocks, whole): (&BTreeSet<usize>, bool),
    current: Option<&Entry>,
    j: usize,
) -> Result<Placement, Miss> {
    let file = &context.changes.files[file_index];
    let head = context.head.get(&file.path);
    let new_mode = file.new_mode.clone().unwrap_or_else(|| "100644".to_owned());
    match file.kind {
        Kind::Modified => {
            let (mode, oid) = current.ok_or(Miss::Absent)?;
            let moded = whole && file.mode_changed();
            if moded && let Some(k) = context.mode_last.get(&file_index).filter(|k| **k > j) {
                // A later commit changes the mode itself: the change goes there or later.
                return Err(Miss::Mode(*k));
            }
            let mode = if moded { new_mode } else { mode.clone() };
            let exact = context.exact.contains(&file.path);
            if file.old_oid.as_ref() == Some(oid) {
                // HEAD's version: the blocks go exactly where they were recorded.
                if whole {
                    let placed = exact.then(|| chosen(file, blocks).into_iter().cloned().collect());
                    return Ok((Some((mode, context.hashed[&file.path].clone())), placed));
                }
                let lines = splice(&context.old_lines[&file.path], &chosen(file, blocks))
                    .map_err(|error| Miss::Apply(nth(blocks, block_of(&error))))?;
                let placed = chosen(file, blocks).into_iter().cloned().collect();
                let oid = context
                    .git
                    .write_object(ObjectKind::Blob, &join_lines(&lines))?;
                return Ok((Some((mode, oid)), Some(placed)));
            }
            if !exact {
                // Unreachable: a whole file's blocks are rebuilt from the stored blob.
                return Err(Miss::Other(fail(
                    Code::Unsupported,
                    format!("{}: its change cannot be placed line by line", file.path),
                )));
            }
            // Another version of the file: each block goes where the later commits' own diffs
            // carry it, never to a look-alike place a 3-way merge might pick.
            let placed = context
                .carried(file, blocks, j)
                .map_err(|(block, k)| Miss::Clash(block, k))?;
            let ours = context
                .git
                .read_blobs(std::slice::from_ref(oid))?
                .pop()
                .unwrap_or_default();
            let refs: Vec<&Block> = placed.iter().collect();
            let lines = splice(&split_lines(&ours), &refs)
                .map_err(|error| Miss::Apply(nth(blocks, block_of(&error))))?;
            let oid = context
                .git
                .write_object(ObjectKind::Blob, &join_lines(&lines))?;
            Ok((Some((mode, oid)), Some(placed)))
        }
        Kind::Untracked | Kind::Added => {
            if current.is_some() {
                return Err(Miss::Exists);
            }
            Ok((Some((new_mode, context.hashed[&file.path].clone())), None))
        }
        Kind::Deleted => {
            if current != head {
                return Err(Miss::Differs);
            }
            Ok((None, None))
        }
        Kind::ModeOnly => {
            if current != head {
                return Err(Miss::Differs);
            }
            let (_, oid) = head.ok_or(Miss::Absent)?;
            Ok((Some((new_mode, oid.clone())), None))
        }
        Kind::Binary | Kind::TypeChange | Kind::Symlink => {
            if current != head {
                return Err(Miss::Differs);
            }
            Ok((Some((new_mode, context.hashed[&file.path].clone())), None))
        }
    }
}

/// The blocks a selection names.
pub(super) fn selected_blocks(file: &FileChange, selected: &FileSel) -> BTreeSet<usize> {
    match selected {
        FileSel::Whole => (0..file.blocks.len()).collect(),
        FileSel::Blocks(set) => set.clone(),
    }
}

/// The one `aisc amend` call that places every change: each block goes into its own target, or
/// into the newest later commit that changes its lines (`needs`); a whole file with a mode change
/// goes, whole, into the newest commit any of its parts or its mode needs. The call is checked
/// before it is offered: every group must resolve to exactly the blocks meant for it, with no
/// overlap. `None` when no checked call exists.
fn one_call(
    context: &Context<'_>,
    chain: &[CommitInfo],
    routes: &Routes,
    needs: &BTreeMap<(usize, usize), usize>,
) -> Option<String> {
    let changes = context.changes;
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    // What each group must select: file -> its blocks (`None`: the whole file).
    let mut meant: BTreeMap<usize, BTreeMap<usize, Option<BTreeSet<usize>>>> = BTreeMap::new();
    for (index, file_routes) in routes {
        let file = &changes.files[*index];
        let finals: BTreeMap<usize, usize> = file_routes
            .iter()
            .flat_map(|(target, selected)| {
                selected_blocks(file, selected).into_iter().map(|block| {
                    (
                        block,
                        needs.get(&(*index, block)).copied().unwrap_or(*target),
                    )
                })
            })
            .collect();
        let whole = file_routes
            .iter()
            .any(|(_, selected)| *selected == FileSel::Whole);
        let targets: BTreeSet<usize> = finals.values().copied().collect();
        let together = file.kind != Kind::Modified
            || (whole
                && (targets.len() <= 1 || file.mode_changed() || context.rebuilt.contains(index)));
        if together {
            let mode = if whole && file.mode_changed() {
                context.mode_last.get(index).copied()
            } else {
                None
            };
            let target = targets
                .iter()
                .copied()
                .chain(mode)
                .chain(file_routes.iter().map(|(target, _)| *target))
                .max()
                .unwrap_or_default();
            groups.entry(target).or_default().push(file.path.clone());
            meant.entry(target).or_default().insert(*index, None);
            continue;
        }
        for hunk in &file.hunks {
            let picked: Vec<usize> = hunk
                .blocks
                .clone()
                .filter(|block| finals.contains_key(block))
                .collect();
            let into: BTreeSet<usize> = picked.iter().map(|block| finals[block]).collect();
            if picked.len() == hunk.blocks.len() && into.len() == 1 {
                let target = finals[&picked[0]];
                groups.entry(target).or_default().push(hunk.short.clone());
                let blocks = meant
                    .entry(target)
                    .or_default()
                    .entry(*index)
                    .or_insert(Some(BTreeSet::new()));
                blocks.get_or_insert_with(BTreeSet::new).extend(picked);
                continue;
            }
            for block in picked {
                let lines = &file.blocks[block];
                // New-side lines; a deletion is named by the line it falls at.
                let from = lines.new_pos + 1;
                let last = from + lines.added.len().max(1) - 1;
                let target = finals[&block];
                groups
                    .entry(target)
                    .or_default()
                    .push(format!("{}:{from}-{last}", file.path));
                let blocks = meant
                    .entry(target)
                    .or_default()
                    .entry(*index)
                    .or_insert(Some(BTreeSet::new()));
                blocks.get_or_insert_with(BTreeSet::new).insert(block);
            }
        }
    }
    // The check: resolve each group exactly as the offered call would.
    let mut selections = Vec::new();
    for (target, tokens) in &groups {
        let selection = selector::resolve(&context.git.root, changes, tokens).ok()?;
        let want = &meant[target];
        if selection.files.len() != want.len() {
            return None;
        }
        for (index, selected) in &selection.files {
            let file = &changes.files[*index];
            let fits = match want.get(index)? {
                None => *selected == FileSel::Whole,
                Some(blocks) => {
                    selected_blocks(file, selected) == *blocks
                        && !(*selected == FileSel::Whole && file.mode_changed())
                }
            };
            if !fits {
                return None;
            }
        }
        selections.push(selection);
    }
    selector::check_disjoint(changes, &selections).ok()?;
    let mut call = "aisc amend".to_owned();
    for (target, tokens) in groups {
        call.push_str(&format!(" --into {}", shell_word(&chain[target].short)));
        for token in tokens {
            call.push(' ');
            call.push_str(&shell_word(&token));
        }
    }
    Some(call)
}

/// The hint for a change that a later commit (chain position `at`) stands in the way of: the one
/// call that works, checked, or else the commit to target.
fn retarget(context: &Context<'_>, chain: &[CommitInfo], routes: &Routes, at: usize) -> String {
    let mut needs = BTreeMap::new();
    for (index, file_routes) in routes {
        let file = &context.changes.files[*index];
        if file.kind != Kind::Modified {
            continue;
        }
        for (target, selected) in file_routes {
            for one in selected_blocks(file, selected) {
                if let Err((_, k)) = context.carried(file, &BTreeSet::from([one]), *target) {
                    needs.insert((*index, one), k);
                }
            }
        }
    }
    match one_call(context, chain, routes, &needs) {
        Some(call) => format!("target {} instead: {call}", chain[at].short),
        None => format!("target {} or later", chain[at].short),
    }
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
        Miss::Clash(block, k) => fail_hint(
            Code::Conflict,
            format!(
                "hunk {} ({}) edits lines from {}; nothing was changed",
                hunk_name(file, block),
                file.path,
                short_of(&chain[k])
            ),
            retarget(context, chain, routes, k),
        ),
        Miss::Mode(k) => fail_hint(
            Code::Conflict,
            format!(
                "{}: {} changes its mode too; the mode change cannot go into {}; nothing was changed",
                file.path,
                short_of(&chain[k]),
                short_of(here)
            ),
            retarget(context, chain, routes, k),
        ),
        Miss::Apply(block) => fail_hint(
            Code::Conflict,
            format!(
                "hunk {} ({}) does not apply to {}",
                hunk_name(file, block),
                file.path,
                short_of(here)
            ),
            "run aisc log and retry",
        ),
    }
}

/// A file placed in one commit.
struct Placed {
    index: usize,
    before: Option<Entry>,
    after: Option<Entry>,
    /// The blocks placed there (index into the file's blocks, block positioned on the commit's
    /// version); `None` when the file is not placed line by line.
    blocks: Option<Vec<(usize, Block)>>,
}

/// Proves, position-aware, every text file of every rewritten commit (see [`proves`]): each
/// rewritten commit changes its rewritten parent by exactly its own recorded blocks plus the
/// hunks routed into it. Returns the (chain position, path) pairs it proved.
fn verify_commits(
    context: &Context<'_>,
    chain: &[CommitInfo],
    routes: &Routes,
    placed: &[Vec<Placed>],
) -> Result<BTreeSet<(usize, String)>> {
    let mut oids = BTreeSet::new();
    for here in placed {
        for item in here.iter().filter(|item| item.blocks.is_some()) {
            oids.extend(
                item.before
                    .iter()
                    .chain(&item.after)
                    .map(|(_, oid)| oid.clone()),
            );
        }
    }
    let oids: Vec<String> = oids.into_iter().collect();
    let blobs: BTreeMap<&String, Vec<u8>> =
        oids.iter().zip(context.git.read_blobs(&oids)?).collect();
    let mut proved = BTreeSet::new();
    for (j, here) in placed.iter().enumerate() {
        for item in here {
            let Some(blocks) = &item.blocks else {
                continue;
            };
            let file = &context.changes.files[item.index];
            let unproved = || mismatch(&file.path, &chain[j], "");
            let (Some((mode_before, before)), Some((mode_after, after))) =
                (&item.before, &item.after)
            else {
                return Err(unproved());
            };
            // Modes: the worktree's mode change goes in with the whole file, from HEAD's mode;
            // otherwise the commit keeps its own mode, so its own mode change stays as it was.
            let mode_routed = file.mode_changed()
                && routes[&item.index]
                    .iter()
                    .any(|(target, selected)| *selected == FileSel::Whole && *target <= j);
            let modes_kept = if mode_routed {
                file.old_mode.as_ref() == Some(mode_before)
                    && file.new_mode.as_ref() == Some(mode_after)
            } else {
                mode_before == mode_after
            };
            if !modes_kept {
                return Err(unproved());
            }
            // The same file in the rewritten parent, when the parent is rewritten too.
            let previous = j
                .checked_sub(1)
                .and_then(|i| placed[i].iter().find(|other| other.index == item.index));
            let (parent, had) = match previous {
                Some(Placed {
                    after: Some((_, oid)),
                    blocks: Some(had),
                    ..
                }) => (
                    Some(blobs[oid].as_slice()),
                    had.iter().map(|(b, _)| *b).collect(),
                ),
                Some(_) => return Err(unproved()),
                None => (None, BTreeSet::new()),
            };
            let ours = split_lines(&blobs[before]);
            let rewrite = Rewrite {
                ours: &ours,
                delta: blocks.iter().map(|(_, block)| block.clone()).collect(),
                fresh: blocks.iter().map(|(b, _)| !had.contains(b)).collect(),
                own: match context.steps[j].get(&file.path) {
                    None => Some(&[]),
                    Some(own) => own.as_deref(),
                },
                rewritten: &blobs[after],
                parent,
            };
            if !proves(&rewrite) {
                return Err(unproved());
            }
            proved.insert((j, file.path.clone()));
        }
    }
    Ok(proved)
}

/// Whether `blocks` applied to `lines` give exactly the blob `oid`.
pub(super) fn rebuilds(
    git: &Git,
    lines: &[Line],
    blocks: &[Block],
    oid: Option<&str>,
) -> Result<bool> {
    let every: Vec<&Block> = blocks.iter().collect();
    let (Some(oid), Ok(rebuilt)) = (oid, splice(lines, &every)) else {
        return Ok(false);
    };
    Ok(git.blob_id(&join_lines(&rebuilt))? == oid)
}

/// The file's change as the diff of HEAD's blob and `stored` (the blob Git stores for the
/// worktree file): exact blocks by construction, whatever filters produced `stored`.
pub(super) fn stored_blocks(
    git: &Git,
    tip_tree: &str,
    file: &FileChange,
    stored: &str,
) -> Result<FileChange> {
    let mode = file
        .new_mode
        .clone()
        .or_else(|| file.old_mode.clone())
        .unwrap_or_else(|| "100644".to_owned());
    let tree = git.tree_with(
        tip_tree,
        &[TreeEdit {
            path: file.path.clone(),
            entry: Some((mode, stored.to_owned())),
        }],
    )?;
    let mut args = super::diff::commit_diff_args(false);
    args.extend([tip_tree, tree.as_str(), "--", file.path.as_str()]);
    let mut files = super::diff::parse_patch(&git.run(&args)?)?;
    super::diff::assign_ids(&mut files);
    files
        .into_iter()
        .find(|other| other.path == file.path && other.kind == Kind::Modified && !other.binary)
        .ok_or_else(|| {
            fail(
                Code::Unsupported,
                format!("{}: its change cannot be placed line by line", file.path),
            )
        })
}

/// With no conversion for any of `paths` ([`Git::no_conversion_all`]), `git add` stores a regular
/// file's bytes as they are: store them in process. `None` (a symlink, directory or unreadable
/// path, an index entry flagged assume-valid or skip-worktree, or any attribute or autocrlf
/// rule) leaves every path to Git.
fn stored_raw(git: &Git, paths: &[String]) -> Option<Vec<String>> {
    let names: Vec<&str> = paths.iter().map(String::as_str).collect();
    // Without in-process writes each file would cost a `hash-object` instead of one shared run.
    if !git.writes_in_process() || !git.no_conversion_all(&names) {
        return None;
    }
    let index = crate::store::read_index(&git.dir).ok()?;
    let wanted: HashSet<&str> = names.iter().copied().collect();
    if index.iter().any(|entry| {
        wanted.contains(entry.path.as_str()) && (entry.assume_valid || entry.skip_worktree)
    }) {
        return None;
    }
    paths
        .iter()
        .map(|path| {
            let full = git.root.join(path);
            if !std::fs::symlink_metadata(&full).ok()?.is_file() {
                return None;
            }
            let bytes = std::fs::read(&full).ok()?;
            git.write_object(ObjectKind::Blob, &bytes).ok()
        })
        .collect()
}

/// Blob ids of worktree files exactly as `git add` would store them: through Git's filters and
/// line-ending rules, several of which consult the index (a file whose staged version has CRLF
/// keeps it under `core.autocrlf`). Works on a copy of the index; the real one is untouched.
pub(super) fn hash_as_staged(git: &Git, paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(oids) = stored_raw(git, paths) {
        return Ok(oids);
    }
    let real = git.dir.join("index");
    let copy = git
        .dir
        .join(format!("aisc-hash-index-{}", std::process::id()));
    let result = (|| -> Result<Vec<String>> {
        match std::fs::copy(&real, &copy) {
            Ok(_) => {
                // Git's racy-clean check compares entries with the index file's own time.
                let modified = std::fs::metadata(&real)?.modified()?;
                std::fs::File::options()
                    .write(true)
                    .open(&copy)?
                    .set_modified(modified)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let env = [
            ("GIT_INDEX_FILE", copy.as_os_str()),
            ("GIT_LITERAL_PATHSPECS", std::ffi::OsStr::new("1")),
        ];
        let mut stored = BTreeMap::new();
        for chunk in paths.chunks(400) {
            let mut args = vec!["update-index", "--add", "--"];
            args.extend(chunk.iter().map(String::as_str));
            git.run_with(&args, &env, None)?;
            if let Some(entries) = git.index_file_entries(&copy, chunk) {
                stored.extend(entries);
                continue;
            }
            let mut args = vec!["ls-files", "-s", "-z", "--"];
            args.extend(chunk.iter().map(String::as_str));
            let output = git.run_with(&args, &env, None)?;
            for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
                let text = String::from_utf8_lossy(record);
                if let Some((meta, path)) = text.split_once('\t')
                    && let Some(oid) = meta.split(' ').nth(1)
                {
                    stored.insert(path.to_owned(), oid.to_owned());
                }
            }
        }
        paths
            .iter()
            .map(|path| {
                stored
                    .get(path)
                    .cloned()
                    .ok_or_else(|| fail(Code::State, format!("{path}: cannot read file")))
            })
            .collect()
    })();
    let _ = std::fs::remove_file(&copy);
    result
}

pub(crate) fn run(args: &cli::Amend) -> Result<Outcome> {
    let groups = parse_groups(args)?;
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, true, false)?;
    let head_oid = session.head.oid.clone();
    if let Some(branch) = &args.opts.branch
        && !is_checked_out(branch, &session.head)
    {
        // Another local branch: the selection is folded into one of its commits from here.
        return elsewhere(args, &groups, branch, &git, session);
    }
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
        let resolved = view.resolve_opt(&git, &group.spec)?;
        let Some(at) = resolved
            .as_deref()
            .and_then(|oid| position(&view, oid).ok())
        else {
            // Not on this branch: on exactly one other local branch it is amended there.
            let holders = super::amend_elsewhere::branches_holding(&git, &view, &group.spec)?;
            return match holders.as_slice() {
                [branch] => elsewhere(args, &groups, branch, &git, session),
                [] => Err(match resolved {
                    Some(oid) => out_of_range(&view, &oid),
                    None => fail_hint(
                        Code::Selector,
                        format!("unknown commit '{}'", group.spec),
                        "run aisc log for ids and subjects",
                    ),
                }),
                many => Err(fail_hint(
                    Code::Selector,
                    format!(
                        "'{}' is on several branches: {}",
                        group.spec,
                        many.join(", ")
                    ),
                    "add --branch BRANCH",
                )),
            };
        };
        positions.push(at);
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
    // Hashed as `git add` would store them, so this compares with what the listing saw.
    let hashes = hash_as_staged(&git, &to_hash)?;
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
    let mut exact = BTreeSet::new();
    let mut owned = changes.clone();
    let mut rebuilt = BTreeSet::new();
    for (index, blob) in modified.iter().zip(old_blobs) {
        let file = &changes.files[*index];
        let lines = split_lines(&blob);
        let partial = routes[index]
            .iter()
            .any(|(_, selected)| matches!(selected, FileSel::Blocks(_)));
        // Exactness gate: all blocks together must reproduce the worktree blob exactly.
        if rebuilds(&git, &lines, &file.blocks, file.new_oid.as_deref())? {
            exact.insert(file.path.clone());
        } else if partial {
            return Err(fail_hint(
                Code::Unsupported,
                format!("{}: Git filters or line-ending conversion apply", file.path),
                "select the whole file",
            ));
        } else {
            // A whole file: its exact blocks are the diff of HEAD's blob and the blob Git
            // stores for the worktree file (through every filter).
            let stored = &hashed[&file.path];
            let blocks = stored_blocks(&git, &tip_tree, file, stored)?;
            if !rebuilds(&git, &lines, &blocks.blocks, Some(stored))? {
                return Err(fail(
                    Code::Unsupported,
                    format!("{}: its change cannot be placed line by line", file.path),
                ));
            }
            let target = &mut owned.files[*index];
            target.blocks = blocks.blocks;
            target.hunks = blocks.hunks;
            target.new_oid = Some(stored.clone());
            rebuilt.insert(*index);
            exact.insert(file.path.clone());
        }
        old_lines.insert(file.path.clone(), lines);
    }
    let changes = &owned;
    let modified_paths: Vec<String> = modified
        .iter()
        .map(|index| changes.files[*index].path.clone())
        .collect();
    let steps = if modified_paths.is_empty() {
        vec![BTreeMap::new(); chain.len()]
    } else {
        recorded(&git, &chain, &modified_paths)?
    };
    // Whole files whose mode changes: the newest commit that changes their mode itself.
    let moded: Vec<usize> = routes
        .iter()
        .filter(|(index, file_routes)| {
            let file = &changes.files[**index];
            file.kind == Kind::Modified
                && file.mode_changed()
                && file_routes
                    .iter()
                    .any(|(_, selected)| *selected == FileSel::Whole)
        })
        .map(|(index, _)| *index)
        .collect();
    let mut mode_last = BTreeMap::new();
    if !moded.is_empty() && chain.len() > 1 {
        let paths: Vec<String> = moded
            .iter()
            .map(|index| changes.files[*index].path.clone())
            .collect();
        let mut previous: Option<BTreeMap<String, Entry>> = None;
        for (k, commit) in chain.iter().enumerate() {
            let entries = git.ls_tree(&commit.tree, &paths)?;
            if let Some(previous) = &previous {
                for index in &moded {
                    let path = &changes.files[*index].path;
                    let mode = |entries: &BTreeMap<String, Entry>| {
                        entries.get(path).map(|(mode, _)| mode.clone())
                    };
                    if mode(previous) != mode(&entries) {
                        mode_last.insert(*index, k);
                    }
                }
            }
            previous = Some(entries);
        }
    }
    let context = Context {
        git: &git,
        changes,
        head: head_entries,
        hashed,
        old_lines,
        steps,
        exact,
        tip: chain.len() - 1,
        mode_last,
        rebuilt,
    };

    // One tree per commit from the oldest target to the tip.
    let mut trees = Vec::new();
    // Per commit: every file placed there.
    let mut placed: Vec<Vec<Placed>> = Vec::new();
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
            let (entry, at) = place(&context, index, (&blocks, whole), current, j)
                .map_err(|miss| explain(&context, &chain, &routes, index, j, miss))?;
            placed_here.push(Placed {
                index,
                before: current.cloned(),
                after: entry.clone(),
                blocks: at.map(|at| blocks.iter().copied().zip(at).collect()),
            });
            edits.push(TreeEdit {
                path: file.path.clone(),
                entry,
            });
        }
        trees.push(git.tree_with(&commit.tree, &edits)?);
        placed.push(placed_here);
    }
    let proved = verify_commits(&context, &chain, &routes, &placed)?;

    // Final verification, at line level, before any commit is created.
    let parent_tree = match chain[0].parents.first() {
        Some(parent) => git.commit_tree_of(parent)?,
        None => git.empty_tree()?,
    };
    let routed_at = |j: usize| {
        let mut lines: BTreeMap<String, (Vec<Line>, Vec<Line>)> = BTreeMap::new();
        // Text files proved position-aware need no line-count comparison.
        let mut skip: BTreeSet<String> = proved
            .iter()
            .filter(|(k, _)| *k == j)
            .map(|(_, path)| path.clone())
            .collect();
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
    fn follow_crosses_every_step_and_names_the_commit_without_a_place() {
        let path = "f".to_owned();
        let steps: Recorded = vec![
            BTreeMap::new(),
            BTreeMap::from([(path.clone(), Some(vec![block(0, 0, 0, 2)]))]),
            BTreeMap::from([(path.clone(), Some(vec![block(5, 1, 5, 1)]))]),
            BTreeMap::new(),
            BTreeMap::from([(path.clone(), None)]),
        ];
        assert_eq!(follow(&steps, &path, 1..=3, Way::Forward, 1, 1), Ok(3));
        assert_eq!(follow(&steps, &path, 1..=3, Way::Forward, 3, 0), Ok(5));
        assert_eq!(follow(&steps, &path, 1..=3, Way::Forward, 3, 1), Err(2));
        assert_eq!(follow(&steps, &path, (1..=2).rev(), Way::Back, 6, 1), Ok(4));
        assert_eq!(follow(&steps, &path, 1..=4, Way::Forward, 3, 0), Err(4));
        assert_eq!(follow(&steps, &path, 1..=4, Way::Forward, 0, 0), Err(1));
        assert_eq!(follow(&steps, "other", 1..=4, Way::Forward, 7, 2), Ok(7));
    }

    fn lines(text: &str) -> Vec<Line> {
        split_lines(text.as_bytes())
    }

    #[test]
    fn proves_accepts_only_the_commits_own_change_in_place() {
        // The commit turned "a b c d" into "a b C d"; the routed fix turns "a" into "A".
        let ours = lines("a\nb\nC\nd\n");
        let own = [Block {
            old_pos: 2,
            new_pos: 2,
            removed: lines("c\n"),
            added: lines("C\n"),
            ..block(0, 0, 0, 0)
        }];
        let fix = Block {
            old_pos: 0,
            new_pos: 0,
            removed: lines("a\n"),
            added: lines("A\n"),
            ..block(0, 0, 0, 0)
        };
        let check = |fresh: bool, rewritten: &str, parent: Option<&str>| {
            proves(&Rewrite {
                ours: &ours,
                delta: vec![fix.clone()],
                fresh: vec![fresh],
                own: Some(&own),
                rewritten: rewritten.as_bytes(),
                parent: parent.map(str::as_bytes),
            })
        };
        // Routed into this commit: the parent is the original (or rewritten without the fix).
        assert!(check(true, "A\nb\nC\nd\n", None));
        assert!(check(true, "A\nb\nC\nd\n", Some("a\nb\nc\nd\n")));
        // Carried from an older commit: the rewritten parent already has it.
        assert!(check(false, "A\nb\nC\nd\n", Some("A\nb\nc\nd\n")));
        // A parent that differs anywhere else, or a fix in another place, is refused.
        assert!(!check(false, "A\nb\nC\nd\n", Some("A\nb\nc\nD\n")));
        assert!(!check(false, "a\nb\nC\nA\n", Some("A\nb\nc\nd\n")));
        assert!(!check(true, "A\nb\nC\nd\n", Some("A\nb\nc\nd\n")));
    }

    fn sh(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "{args:?}");
    }

    #[test]
    fn stored_blocks_rebuild_the_stored_blob_exactly() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        sh(&root, &["init", "-q", "-b", "main"]);
        sh(&root, &["config", "user.name", "T"]);
        sh(&root, &["config", "user.email", "t@example.invalid"]);
        std::fs::write(root.join("f"), "a\r\nb\r\nc\r\n").unwrap();
        sh(&root, &["add", "."]);
        sh(&root, &["commit", "-q", "-m", "base"]);
        let git = Git {
            dir: root.join(".git"),
            root,
        };
        let head = git.head_state().unwrap();
        let tree = git.commit_tree_of(&head.oid).unwrap();
        let old = git.ls_tree(&tree, &["f".to_owned()]).unwrap()["f"]
            .1
            .clone();
        // Whatever a filter made of the worktree file: mixed endings, a changed and an added line.
        let stored = git.write_blob(b"A\nb\r\nc\r\nd\n", true).unwrap();
        let file = FileChange {
            path: "f".into(),
            kind: Kind::Modified,
            old_mode: Some("100644".into()),
            new_mode: Some("100644".into()),
            old_oid: Some(old.clone()),
            new_oid: None,
            blocks: Vec::new(),
            hunks: Vec::new(),
            lines: 0,
            binary: false,
            digest: String::new(),
            ws: false,
        };
        let change = stored_blocks(&git, &tree, &file, &stored).unwrap();
        let head_lines = split_lines(&git.read_blobs(&[old]).unwrap()[0]);
        assert_eq!(change.blocks.len(), 2);
        assert!(rebuilds(&git, &head_lines, &change.blocks, Some(&stored)).unwrap());
        assert!(!rebuilds(&git, &head_lines, &change.blocks[..1], Some(&stored)).unwrap());
    }
}
