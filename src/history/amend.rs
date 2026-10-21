//! `aisc amend`: moves uncommitted hunks into several existing commits in one call (algorithm 6.4).
//! Also hosts the helpers that `split`/`uncommit` share for rewriting a commit chain.
use super::apply::{ApplyError, Direction, join_lines, splice, split_lines, transplant};
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
            let meta = git.read_commit_meta(&commit.oid)?;
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
    blobs: &BTreeMap<String, Vec<Line>>,
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
                let lines = transplant(&blobs[oid], &blocks_here, Direction::Forward)
                    .map_err(|error| Miss::Apply(nth(blocks, block_of(&error))))?;
                context.git.write_blob(&join_lines(&lines), true)?
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
                let Ok(mut blobs) = context.git.read_blobs(std::slice::from_ref(oid)) else {
                    return false;
                };
                let lines = split_lines(&blobs.remove(0));
                transplant(&lines, &[&file.blocks[block]], Direction::Forward).is_ok()
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
    let mut blob_cache: BTreeMap<String, Vec<Line>> = BTreeMap::new();
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
        let missing: Vec<String> = here
            .iter()
            .filter_map(|index| {
                let file = &changes.files[*index];
                let (_, oid) = entries.get(&file.path)?;
                (file.kind == Kind::Modified
                    && file.old_oid.as_ref() != Some(oid)
                    && !blob_cache.contains_key(oid))
                .then(|| oid.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for (oid, blob) in missing.iter().zip(git.read_blobs(&missing)?) {
            blob_cache.insert(oid.clone(), split_lines(&blob));
        }
        let mut edits = Vec::new();
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
                    let entry = place(&context, &blob_cache, index, &blocks, whole, current)
                        .map_err(|miss| explain(&context, &chain, &routes, index, j, miss))?;
                    memo.insert(key, entry.clone());
                    entry
                }
            };
            edits.push(TreeEdit {
                path: file.path.clone(),
                entry,
            });
        }
        trees.push(git.tree_with(&commit.tree, &edits)?);
    }

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
