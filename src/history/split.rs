//! `aisc split` and `aisc uncommit` (algorithm 6.5): cut a commit into parts, optionally moving
//! the rest into the worktree. Uncommit is a split whose single part is "everything but SEL".
use super::amend::{
    Finished, Way, block_of, carry, chain_to_tip, finish, hunk_name, index_changes, merge3,
    mismatch, move_refs, no_single_place, position, rebuild, recorded, require_checked_out,
    selection_hunks, short_of, signature,
};
use super::apply::{join_lines, splice, split_lines, unsplice};
use super::diff::{assign_ids, commit_diff_args, parse_patch};
use super::model::{Block, ChangeSet, FileChange, Kind, Line};
use super::plumbing::{Entry, Git, TreeEdit};
use super::rewrite::{Plan, Session, stale};
use super::selector::{self, FileSel, Selection};
use super::view::{self, CommitInfo};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn run_split(args: &cli::Split) -> Result<Outcome> {
    if args.groups.is_empty() {
        return Err(fail_hint(
            Code::Usage,
            "name the parts of the split",
            "aisc split COMMIT --into MESSAGE SELECTOR… [--into MESSAGE SELECTOR…]",
        ));
    }
    let mut groups = Vec::new();
    for group in &args.groups {
        let (message, tokens) = group.split_first().expect("clap requires two values");
        if message.trim().is_empty() {
            return Err(fail(Code::Usage, "a commit message must not be empty"));
        }
        if tokens.is_empty() {
            return Err(fail(
                Code::Usage,
                format!("--into \"{message}\" needs at least one selector"),
            ));
        }
        groups.push((message.clone(), tokens.to_vec()));
    }
    execute(&Request {
        verb: "split",
        commit: &args.commit,
        opts: &args.opts,
        mode: Mode::Split {
            groups,
            keep: args.rest == cli::Rest::Keep,
        },
    })
}

pub(crate) fn run_uncommit(args: &cli::Uncommit) -> Result<Outcome> {
    execute(&Request {
        verb: "uncommit",
        commit: &args.commit,
        opts: &args.opts,
        mode: Mode::Uncommit {
            tokens: args.selectors.clone(),
        },
    })
}

enum Mode {
    Split {
        groups: Vec<(String, Vec<String>)>,
        keep: bool,
    },
    Uncommit {
        tokens: Vec<String>,
    },
}

struct Request<'a> {
    verb: &'static str,
    commit: &'a str,
    opts: &'a cli::MutationOpts,
    mode: Mode,
}

/// What of a file stays out of every part.
enum RestFile {
    Whole,
    Part { blocks: BTreeSet<usize>, mode: bool },
}

fn commit_changes(git: &Git, commit: &CommitInfo) -> Result<ChangeSet> {
    let mut full: Vec<&str> = commit_diff_args(commit.parents.is_empty());
    if let Some(parent) = commit.parents.first() {
        full.push(parent);
    }
    full.push(&commit.oid);
    let patch = git.run(&full)?;
    let mut files = parse_patch(&patch)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    assign_ids(&mut files);
    Ok(ChangeSet {
        files,
        nested: Vec::new(),
    })
}

/// Everything of the commit's diff that `picked` does not select.
fn complement(changes: &ChangeSet, picked: &Selection) -> Result<Selection> {
    let mut out = Selection::default();
    for (index, file) in changes.files.iter().enumerate() {
        match picked.files.get(&index) {
            None => {
                out.files.insert(index, FileSel::Whole);
            }
            Some(FileSel::Whole) => {}
            Some(FileSel::Blocks(set)) => {
                let rest: BTreeSet<usize> = (0..file.blocks.len())
                    .filter(|block| !set.contains(block))
                    .collect();
                if !rest.is_empty() {
                    out.files.insert(index, FileSel::Blocks(rest));
                } else if file.mode_changed() {
                    return Err(fail_hint(
                        Code::Unsupported,
                        format!("{}: the mode change cannot be kept alone", file.path),
                        "select the whole file",
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn rest_of(changes: &ChangeSet, selections: &[Selection]) -> BTreeMap<usize, RestFile> {
    let mut rest = BTreeMap::new();
    for (index, file) in changes.files.iter().enumerate() {
        let mut whole = false;
        let mut any = false;
        let mut union: BTreeSet<usize> = BTreeSet::new();
        for selection in selections {
            match selection.files.get(&index) {
                Some(FileSel::Whole) => {
                    any = true;
                    whole = true;
                }
                Some(FileSel::Blocks(set)) => {
                    any = true;
                    union.extend(set);
                }
                None => {}
            }
        }
        if whole {
            continue;
        }
        if !any {
            rest.insert(index, RestFile::Whole);
            continue;
        }
        let blocks: BTreeSet<usize> = (0..file.blocks.len())
            .filter(|block| !union.contains(block))
            .collect();
        let mode = file.mode_changed();
        if !blocks.is_empty() || mode {
            rest.insert(index, RestFile::Part { blocks, mode });
        }
    }
    rest
}

struct Parts<'a> {
    git: &'a Git,
    changes: &'a ChangeSet,
    parent_entries: BTreeMap<String, Entry>,
    commit_entries: BTreeMap<String, Entry>,
    parent_lines: BTreeMap<String, Vec<Line>>,
}

impl Parts<'_> {
    /// The file's entry once the (cumulative) selection has been applied to the parent.
    fn entry(&self, index: usize, selected: &FileSel) -> Result<Option<Entry>> {
        let file = &self.changes.files[index];
        match selected {
            FileSel::Whole => Ok(self.commit_entries.get(&file.path).cloned()),
            FileSel::Blocks(blocks) => {
                let chosen: Vec<&Block> = blocks.iter().map(|block| &file.blocks[*block]).collect();
                let lines = splice(&self.parent_lines[&file.path], &chosen)
                    .map_err(|error| stale(&format!("{}: {error}", file.path)))?;
                let oid = self.git.write_blob(&join_lines(&lines), true)?;
                let mode = self
                    .parent_entries
                    .get(&file.path)
                    .map_or_else(|| "100644".to_owned(), |(mode, _)| mode.clone());
                Ok(Some((mode, oid)))
            }
        }
    }
}

fn merge(into: &mut BTreeMap<usize, FileSel>, more: &Selection) {
    for (file, selected) in &more.files {
        match (into.get_mut(file), selected) {
            (Some(FileSel::Whole), _) => {}
            (Some(FileSel::Blocks(existing)), FileSel::Blocks(extra)) => existing.extend(extra),
            _ => {
                into.insert(*file, selected.clone());
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn execute(request: &Request<'_>) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, true, false)?;
    let head_oid = session.head.oid.clone();
    require_checked_out(request.opts, &session.head)?;
    let view = view::read(&git, None, 1_000, false)?;
    if view.tip != head_oid {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    let oid = view.resolve(&git, request.commit)?;
    let chain = chain_to_tip(&view, position(&view, &oid)?)?;
    let target = &chain[0];
    let parent_tree = match target.parents.first() {
        Some(parent) => git.commit_tree_of(parent)?,
        None => git.empty_tree()?,
    };
    let changes = commit_changes(&git, target)?;
    if changes.files.is_empty() {
        return Err(fail(
            Code::Selector,
            format!("commit {} has no changes to split", short_of(target)),
        ));
    }
    let meta = git.read_commit_meta(&target.oid)?.with_current_committer();

    // Parts: (message, selection), and what happens to the rest.
    let mut parts: Vec<(Vec<u8>, Selection)> = Vec::new();
    let keep;
    match &request.mode {
        Mode::Split {
            groups,
            keep: keep_rest,
        } => {
            keep = *keep_rest;
            for (message, tokens) in groups {
                parts.push((
                    message.clone().into_bytes(),
                    selector::resolve(&git.root, &changes, tokens)?,
                ));
            }
        }
        Mode::Uncommit { tokens } => {
            keep = false;
            let picked = selector::resolve(&git.root, &changes, tokens)?;
            let kept = complement(&changes, &picked)?;
            if !kept.is_empty() {
                parts.push((meta.message.clone(), kept));
            }
        }
    }
    let selections: Vec<Selection> = parts
        .iter()
        .map(|(_, selection)| selection.clone())
        .collect();
    selector::check_disjoint(&changes, &selections)?;
    let rest = rest_of(&changes, &selections);
    if parts.is_empty() && target.parents.is_empty() {
        return Err(fail_hint(
            Code::Unsupported,
            "that would remove the only commit",
            "nothing would be left of the branch",
        ));
    }
    if parts.len() == 1 && rest.is_empty() {
        return Err(fail_hint(
            Code::Selector,
            "the split changes nothing: the part covers the whole commit",
            "leave something out, or add another --into",
        ));
    }

    // Entries of the commit and its parent for the files of the diff.
    let paths: Vec<String> = changes.files.iter().map(|file| file.path.clone()).collect();
    let commit_entries = git.ls_tree(&target.tree, &paths)?;
    let parent_entries = git.ls_tree(&parent_tree, &paths)?;
    let needs_lines: Vec<usize> = selections
        .iter()
        .flat_map(|selection| selection.files.iter())
        .filter(|(_, selected)| matches!(selected, FileSel::Blocks(_)))
        .map(|(index, _)| *index)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut old_oids = Vec::new();
    for index in &needs_lines {
        let file = &changes.files[*index];
        old_oids.push(file.old_oid.clone().ok_or_else(|| {
            fail(
                Code::Unsupported,
                format!("{}: cannot select part of this change", file.path),
            )
        })?);
    }
    let mut parent_lines = BTreeMap::new();
    for (index, blob) in needs_lines.iter().zip(git.read_blobs(&old_oids)?) {
        let file = &changes.files[*index];
        let lines = split_lines(&blob);
        // Exactness gate: all blocks together must reproduce the commit's blob exactly.
        let every: Vec<&Block> = file.blocks.iter().collect();
        let rebuilt = splice(&lines, &every)
            .map_err(|error| fail(Code::Unsupported, format!("{}: {error}", file.path)))?;
        let oid = git.write_blob(&join_lines(&rebuilt), true)?;
        if commit_entries.get(&file.path).map(|(_, known)| known) != Some(&oid) {
            return Err(fail_hint(
                Code::Unsupported,
                format!("{}: Git filters or line-ending conversion apply", file.path),
                "select the whole file",
            ));
        }
        parent_lines.insert(file.path.clone(), lines);
    }
    let builder = Parts {
        git: &git,
        changes: &changes,
        parent_entries,
        commit_entries,
        parent_lines,
    };

    // Part commits: tree k = parent + groups 1..k, spliced exactly.
    let mut cumulative: BTreeMap<usize, FileSel> = BTreeMap::new();
    let mut base_tree = parent_tree.clone();
    let mut base: Option<String> = target.parents.first().cloned();
    let mut created: Vec<String> = Vec::new();
    for (message, selection) in &parts {
        merge(&mut cumulative, selection);
        let mut edits = Vec::new();
        for index in selection.files.keys() {
            edits.push(TreeEdit {
                path: changes.files[*index].path.clone(),
                entry: builder.entry(*index, &cumulative[index])?,
            });
        }
        let tree = git.tree_with(&base_tree, &edits)?;
        let commit = git.commit_tree(&tree, base.as_slice(), message, Some(&meta))?;
        created.push(commit.clone());
        base = Some(commit);
        base_tree = tree;
    }
    // The entry of each rest file once all parts are in (what descendants fall back to).
    let mut after_parts: BTreeMap<usize, Option<Entry>> = BTreeMap::new();
    for index in rest.keys() {
        let entry = match cumulative.get(index) {
            Some(selected) => builder.entry(*index, selected)?,
            None => builder
                .parent_entries
                .get(&changes.files[*index].path)
                .cloned(),
        };
        after_parts.insert(*index, entry);
    }
    let kept_tail = keep && !rest.is_empty();
    if kept_tail {
        let commit = git.commit_tree(&target.tree, base.as_slice(), &meta.message, Some(&meta))?;
        created.push(commit.clone());
        base = Some(commit);
        base_tree.clone_from(&target.tree);
    }

    // Descendants: re-parented; with `uncommit` rest they also lose the rest.
    let descendants = &chain[1..];
    let mut trees = Vec::new();
    for commit in descendants {
        if keep || rest.is_empty() {
            trees.push(commit.tree.clone());
            continue;
        }
        let rest_paths: Vec<String> = rest
            .keys()
            .map(|index| changes.files[*index].path.clone())
            .collect();
        let entries = git.ls_tree(&commit.tree, &rest_paths)?;
        let mut edits = Vec::new();
        for (index, part) in &rest {
            let file = &changes.files[*index];
            let current = entries.get(&file.path);
            let entry = if current == builder.commit_entries.get(&file.path) {
                after_parts[index].clone()
            } else {
                Some(strip_rest(
                    file,
                    part,
                    current,
                    (
                        builder.commit_entries.get(&file.path),
                        after_parts[index].as_ref(),
                    ),
                    &git,
                    commit,
                )?)
            };
            edits.push(TreeEdit {
                path: file.path.clone(),
                entry,
            });
        }
        trees.push(git.tree_with(&commit.tree, &edits)?);
    }
    let new_tip_tree = trees.last().cloned().unwrap_or_else(|| base_tree.clone());

    // Final verification at line level, before any commit is rebuilt: every descendant must
    // differ from its new parent exactly as it did originally, and the new tip must differ from
    // the old one by exactly the rest.
    if !keep && !rest.is_empty() {
        let (mut old_prev, mut new_prev) = (target.tree.clone(), base_tree.clone());
        for (commit, tree) in descendants.iter().zip(&trees) {
            let original = signature(&git, &old_prev, &commit.tree)?;
            let rewritten = signature(&git, &new_prev, tree)?;
            if original != rewritten {
                let path = original
                    .keys()
                    .chain(rewritten.keys())
                    .find(|path| original.get(*path) != rewritten.get(*path))
                    .cloned()
                    .unwrap_or_default();
                return Err(mismatch(
                    &path,
                    commit,
                    ": taking the rest out would alter it",
                ));
            }
            old_prev.clone_from(&commit.tree);
            new_prev.clone_from(tree);
        }
        let old_tip_tree = chain.last().expect("chain has the tip").tree.clone();
        let delta = signature(&git, &new_tip_tree, &old_tip_tree)?;
        let rest_paths: BTreeSet<&str> = rest
            .keys()
            .map(|index| changes.files[*index].path.as_str())
            .collect();
        for path in delta.keys() {
            if !rest_paths.contains(path.as_str()) {
                return Err(mismatch(
                    path,
                    chain.last().expect("tip"),
                    ": it would lose other changes",
                ));
            }
        }
        verify_rest_positions(&git, &chain, &changes, &rest, &trees)?;
    }
    let (descendant_pairs, tip) = rebuild(&git, descendants, &trees, base.clone())?;
    let tip = tip
        .or(base.clone())
        .expect("a commit or the parent remains");

    let mut rewritten = Vec::new();
    if let Some(replacement) = &base
        && *replacement != target.oid
    {
        rewritten.push((target.oid.clone(), replacement.clone()));
    }
    rewritten.extend(descendant_pairs);

    let mut plan = Plan {
        verb: request.verb.to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    if !keep {
        let old_tip_tree = chain.last().expect("chain has the tip").tree.clone();
        let rest_paths: Vec<String> = rest
            .keys()
            .map(|index| changes.files[*index].path.clone())
            .collect();
        plan.index = index_changes(&git, &old_tip_tree, &new_tip_tree, &rest_paths)?;
    }
    let moved = move_refs(&git, &view, &session.head, &mut plan, &rewritten, &tip)?;

    let rest_names: Vec<String> = rest
        .keys()
        .map(|index| escape_line(&changes.files[*index].path))
        .collect();
    let summary = match request.mode {
        Mode::Split { .. } => {
            let count = created.len();
            let mut text = format!("split {} into {count}", target.short);
            if kept_tail {
                text.push_str(" (rest kept in the last commit)");
            } else if !rest_names.is_empty() {
                text.push_str(&format!("; rest uncommitted: {}", rest_names.join(" ")));
            }
            text
        }
        Mode::Uncommit { .. } => {
            let mut text = format!("uncommitted {} from {}", rest_names.join(" "), target.short);
            if parts.is_empty() {
                text.push_str(" (commit dropped)");
            }
            text
        }
    };
    plan.summary = summary.clone();
    let applied = session.apply(&plan)?;
    let subjects = chain
        .iter()
        .map(|commit| (commit.oid.clone(), commit.subject.clone()))
        .collect();
    let part_ids = created
        .iter()
        .map(|oid| json!({"oid": oid}))
        .collect::<Vec<_>>();
    finish(
        &git,
        &Finished {
            verb: request.verb,
            result: format!("{summary}; op {}", applied.op),
            op: applied.op,
            quiet: request.opts.quiet,
            old_tip: head_oid,
            new_tip: tip,
            rewritten: &rewritten,
            subjects,
            moved: &moved,
            extra: json!({
                "parts": part_ids,
                "hunks": parts.iter().map(|(_, s)| selection_hunks(&changes, s)).sum::<usize>(),
                "rest": rest.keys().map(|index| changes.files[*index].path.clone()).collect::<Vec<_>>(),
            }),
        },
    )
}

/// Position check for the rest, independent of how the 3-way merge aligned the versions: in every
/// rewritten descendant (the new tip included), each text file of the rest must be exactly that
/// commit's version with the rest's blocks taken out where the commits' own diffs carry them.
/// Line counts cannot tell a rest taken from a look-alike line; this can, and then nothing changes.
fn verify_rest_positions(
    git: &Git,
    chain: &[CommitInfo],
    changes: &ChangeSet,
    rest: &BTreeMap<usize, RestFile>,
    trees: &[String],
) -> Result<()> {
    let text: Vec<(&FileChange, Vec<usize>)> = rest
        .iter()
        .filter(|(index, _)| changes.files[**index].kind == Kind::Modified)
        .map(|(index, part)| {
            let file = &changes.files[*index];
            let blocks = match part {
                RestFile::Whole => (0..file.blocks.len()).collect(),
                RestFile::Part { blocks, .. } => blocks.iter().copied().collect(),
            };
            (file, blocks)
        })
        .filter(|(_, blocks): &(_, Vec<usize>)| !blocks.is_empty())
        .collect();
    if text.is_empty() || trees.is_empty() {
        // Without descendants the parts are spliced exactly.
        return Ok(());
    }
    let paths: Vec<String> = text.iter().map(|(file, _)| file.path.clone()).collect();
    let steps = recorded(git, chain, &paths)?;
    // (descendant position in the chain, file, its version there, the rewritten version)
    let mut pending = Vec::new();
    for (offset, tree) in trees.iter().enumerate() {
        let k = offset + 1;
        let original = git.ls_tree(&chain[k].tree, &paths)?;
        let rewritten = git.ls_tree(tree, &paths)?;
        for (file, blocks) in &text {
            let (Some((_, ours)), Some((_, theirs))) =
                (original.get(&file.path), rewritten.get(&file.path))
            else {
                return Err(mismatch(
                    &file.path,
                    &chain[k],
                    ": the rest is not where it was",
                ));
            };
            // Unchanged since the split commit: the parts' version, spliced exactly.
            if file.new_oid.as_ref() != Some(ours) {
                pending.push((k, *file, blocks, ours.clone(), theirs.clone()));
            }
        }
    }
    let oids: Vec<String> = pending
        .iter()
        .flat_map(|(_, _, _, ours, theirs)| [ours.clone(), theirs.clone()])
        .collect();
    let blobs = git.read_blobs(&oids)?;
    for ((k, file, blocks, _, _), pair) in pending.iter().zip(blobs.chunks(2)) {
        let refuse = |order: usize| {
            let block = blocks.get(order).copied().unwrap_or_default();
            no_single_place(
                &file.path,
                &hunk_name(file, block),
                &chain[*k],
                "split that commit first, or keep the rest with --rest keep",
            )
        };
        // The split commit up to this descendant, oldest first.
        let between: Option<Vec<&[Block]>> = (1..=*k)
            .map(|step| match steps[step].get(&file.path) {
                None => Some(&[][..]),
                Some(blocks) => blocks.as_deref(),
            })
            .collect();
        let between = between.ok_or_else(|| refuse(0))?;
        let ranges: Vec<(usize, usize)> = blocks
            .iter()
            .map(|block| {
                let block = &file.blocks[*block];
                (block.new_pos, block.new_pos + block.added.len())
            })
            .collect();
        let starts = carry(&ranges, &between, Way::Forward).map_err(refuse)?;
        let moved: Vec<Block> = blocks
            .iter()
            .zip(starts)
            .map(|(block, new_pos)| Block {
                new_pos,
                ..file.blocks[*block].clone()
            })
            .collect();
        let moved: Vec<&Block> = moved.iter().collect();
        let expected =
            unsplice(&split_lines(&pair[0]), &moved).map_err(|error| refuse(block_of(&error)))?;
        if join_lines(&expected) != pair[1] {
            return Err(refuse(0));
        }
    }
    Ok(())
}

/// A descendant's entry for a rest file with the rest taken out: the change commit -> parts is
/// merged (3-way) onto the descendant's version, so it is removed exactly where it was made.
fn strip_rest(
    file: &FileChange,
    part: &RestFile,
    current: Option<&Entry>,
    (original, parts): (Option<&Entry>, Option<&Entry>),
    git: &Git,
    commit: &CommitInfo,
) -> Result<Entry> {
    let again = || {
        fail_hint(
            Code::Conflict,
            format!(
                "{} is changed again by {}; its rest cannot be taken out",
                file.path,
                short_of(commit)
            ),
            "split that commit first, or keep the rest with --rest keep",
        )
    };
    if file.kind != super::model::Kind::Modified {
        return Err(again());
    }
    let ((mode, ours), (_, base), (_, theirs)) = (
        current.ok_or_else(again)?,
        original.ok_or_else(again)?,
        parts.ok_or_else(again)?,
    );
    let blocks: BTreeSet<usize> = match part {
        RestFile::Whole => (0..file.blocks.len()).collect(),
        RestFile::Part { blocks, .. } => blocks.clone(),
    };
    let mode_back = match part {
        RestFile::Whole => file.mode_changed(),
        RestFile::Part { mode, .. } => *mode,
    };
    let Some(merged) = merge3(git, base, ours, theirs)? else {
        let names = file
            .hunks
            .iter()
            .filter(|hunk| hunk.blocks.clone().any(|block| blocks.contains(&block)))
            .map(|hunk| hunk.short.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(fail_hint(
            Code::Conflict,
            format!(
                "hunk {names} ({}) cannot be taken out of {}: it changes the same lines",
                file.path,
                short_of(commit)
            ),
            "split that commit first, or keep the rest with --rest keep",
        ));
    };
    let mode = if mode_back && file.old_mode.is_some() {
        file.old_mode.clone().unwrap_or_else(|| mode.clone())
    } else {
        mode.clone()
    };
    Ok((mode, git.write_blob(&merged, true)?))
}
