//! `aisc move`: reorders commits of a branch by replaying them with `merge-tree`. Also hosts the
//! replay helpers shared with `squash` and `pick`.
use super::model::ChangeSet;
use super::plumbing::Git;
use super::render;
use super::rewrite::{Applied, Plan, RefMove, Session, dependent_moves, stale};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Enough to cover any realistic branch; the view still stops at the base.
pub(crate) const LIMIT: usize = 100_000;

/// A commit to replay: its identity, tree and the tree of its first parent.
#[derive(Clone, Debug)]
pub(crate) struct Src {
    pub oid: String,
    pub short: String,
    pub tree: String,
    pub parent: Option<String>,
    pub parent_tree: String,
    pub subject: String,
}

/// The commit a replay builds on.
#[derive(Clone, Debug)]
pub(crate) struct Onto {
    pub oid: Option<String>,
    pub tree: String,
}

pub(crate) enum Step {
    /// The commit already has the wanted parent: keep it as it is.
    Reuse,
    /// Clean merge: the new commit gets this tree.
    Merged(String),
    Conflict(Vec<String>),
}

/// Sources for an oldest-first slice of a linear range.
pub(crate) fn range_sources(git: &Git, range: &[CommitInfo]) -> Result<Vec<Src>> {
    let mut sources: Vec<Src> = Vec::new();
    for commit in range {
        let parent = commit.parents.first().cloned();
        let parent_tree = match (sources.last(), &parent) {
            (Some(previous), Some(parent)) if previous.oid == *parent => previous.tree.clone(),
            (_, Some(parent)) => git.commit_tree_of(parent)?,
            (_, None) => git.empty_tree()?,
        };
        sources.push(Src {
            oid: commit.oid.clone(),
            short: commit.short.clone(),
            tree: commit.tree.clone(),
            parent,
            parent_tree,
            subject: commit.subject.clone(),
        });
    }
    Ok(sources)
}

pub(crate) fn replay(git: &Git, src: &Src, onto: &Onto) -> Result<Step> {
    if src.parent == onto.oid {
        return Ok(Step::Reuse);
    }
    let merged = git.merge_trees(&src.parent_tree, &onto.tree, &src.tree)?;
    if merged.conflicts.is_empty() {
        Ok(Step::Merged(merged.tree))
    } else {
        let mut paths: Vec<String> = merged.conflicts.into_iter().map(|c| c.path).collect();
        paths.dedup();
        Ok(Step::Conflict(paths))
    }
}

pub(crate) fn conflict_error(src: &Src, paths: &[String]) -> anyhow::Error {
    let verb = if paths.len() == 1 {
        "conflicts"
    } else {
        "conflict"
    };
    fail_hint(
        Code::Conflict,
        format!(
            "commit {} \"{}\": {} {verb}; nothing was changed",
            src.short,
            escape_line(&src.subject),
            paths
                .iter()
                .map(|path| escape_line(path))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "choose an order or selection that avoids the overlap",
    )
}

/// New commit with `src`'s author and message.
pub(crate) fn recommit(
    git: &Git,
    src: &Src,
    tree: &str,
    parent: Option<&String>,
) -> Result<String> {
    let meta = git.read_commit_meta(&src.oid)?;
    let parents: Vec<String> = parent.cloned().into_iter().collect();
    git.commit_tree(
        tree,
        &parents,
        &meta.message,
        Some(&meta.clone().with_current_committer()),
    )
}

pub(crate) fn short_ids(git: &Git, oids: &[String]) -> Result<Vec<String>> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut arguments = vec![
        "log".to_owned(),
        "--no-walk=unsorted".to_owned(),
        "--abbrev=4".to_owned(),
        "--format=%h".to_owned(),
    ];
    arguments.extend(oids.iter().cloned());
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    Ok(git.line(&arguments)?.lines().map(str::to_owned).collect())
}

pub(crate) fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// The view's range, oldest first.
pub(crate) fn linear_range(view: &View) -> Vec<CommitInfo> {
    view.commits.iter().rev().cloned().collect()
}

/// Refuses merge commits from position `from` upward.
pub(crate) fn require_linear(range: &[CommitInfo], from: usize) -> Result<()> {
    for (index, commit) in range.iter().enumerate().skip(from) {
        if commit.parents.len() > 1 || (commit.parents.is_empty() && index != 0) {
            return Err(fail_hint(
                Code::Unsupported,
                format!("commit {} is a merge; history must be linear", commit.short),
                "only linear history can be rewritten",
            ));
        }
    }
    Ok(())
}

/// Position of `spec` in the oldest-first range.
pub(crate) fn locate(git: &Git, view: &View, range: &[CommitInfo], spec: &str) -> Result<usize> {
    let oid = view.resolve(git, spec)?;
    range
        .iter()
        .position(|commit| commit.oid == oid)
        .ok_or_else(|| {
            let base = view
                .base
                .as_ref()
                .map_or("the base".to_owned(), |base| base.label.clone());
            fail_hint(
                Code::Selector,
                format!(
                    "{} is not in the rewritable range (it is on {base} or another branch)",
                    &oid[..oid.len().min(8)]
                ),
                "published history is never rewritten; run aisc log for the range",
            )
        })
}

pub(crate) struct Finished {
    pub applied: Applied,
    pub moved_refs: Vec<Value>,
    pub branch: String,
}

/// Moves the target branch (and branches pointing at rewritten commits) to `new_tip`.
pub(crate) fn finish_rewrite(
    git: &Git,
    session: Session,
    view: &View,
    verb: &str,
    summary: String,
    new_tip: &str,
    rewritten: Vec<(String, String)>,
) -> Result<Finished> {
    let checked_out = git.checked_out()?;
    let (reference, branch) = if view.is_head {
        if session.head.oid != view.tip {
            return Err(stale("HEAD changed while aisc was reading"));
        }
        match &session.head.symbolic {
            Some(symbolic) => (
                symbolic.clone(),
                symbolic.trim_start_matches("refs/heads/").to_owned(),
            ),
            None => ("HEAD".to_owned(), "HEAD (detached)".to_owned()),
        }
    } else {
        let name = view.branch.clone().unwrap_or_default();
        let reference = format!("refs/heads/{name}");
        if checked_out.contains(&reference) {
            return Err(fail_hint(
                Code::State,
                format!("branch '{name}' is checked out in another worktree"),
                "run the command from that worktree",
            ));
        }
        (reference, name)
    };
    let mut plan = Plan {
        verb: verb.to_owned(),
        summary,
        argv: std::env::args().skip(1).collect(),
        rewritten: rewritten.clone(),
        ..Plan::default()
    };
    plan.refs.push(RefMove {
        name: reference.clone(),
        old: Some(view.tip.clone()),
        new: Some(new_tip.to_owned()),
    });
    let mut moved_refs = vec![json!({"ref": reference, "old": view.tip, "new": new_tip})];
    let mut skip = vec![branch.as_str()];
    if let Some(name) = &view.branch {
        skip.push(name.as_str());
    }
    for dependent in dependent_moves(&view.locals, &rewritten, &skip) {
        if checked_out.contains(&dependent.name) {
            return Err(fail_hint(
                Code::State,
                format!(
                    "{} points at a rewritten commit and is checked out in a worktree",
                    dependent.name
                ),
                "check out another branch there first",
            ));
        }
        moved_refs.push(json!({"ref": dependent.name, "old": dependent.old, "new": dependent.new}));
        plan.refs.push(dependent);
    }
    let applied = session.apply(&plan)?;
    Ok(Finished {
        applied,
        moved_refs,
        branch,
    })
}

/// Output of a rewrite verb: result line, compact log and JSON.
pub(crate) struct Report<'a> {
    pub branch_option: Option<&'a str>,
    pub quiet: bool,
    pub verb: &'a str,
    pub line: String,
    pub old: &'a str,
    pub new: &'a str,
    pub rewritten: &'a [(String, String)],
}

pub(crate) fn outcome(git: &Git, report: &Report, finished: &Finished) -> Result<Outcome> {
    let after = view::read(git, report.branch_option, 20, true)?;
    let result = format!("{}; op {}", report.line, finished.applied.op);
    let text = if report.quiet {
        result
    } else {
        format!("{result}\n{}", render::compact_text(&after))
    };
    let value = json!({
        "op": finished.applied.op,
        "verb": report.verb,
        "branch": finished.branch,
        "old": report.old,
        "new": report.new,
        "rewritten": report.rewritten.iter()
            .map(|(old, new)| json!({"old": old, "new": new})).collect::<Vec<_>>(),
        "moved_refs": finished.moved_refs,
        "log": render::log_json(&after, false),
    });
    Ok(Outcome::with_text(value, text))
}

/// Uncommitted paths of a change set.
pub(crate) fn dirty_paths(changes: Option<&ChangeSet>) -> BTreeSet<String> {
    changes
        .map(|changes| changes.files.iter().map(|file| file.path.clone()).collect())
        .unwrap_or_default()
}

/// New order of `0..count` after moving `moved` (kept in original relative order) next to `anchor`.
fn placed(count: usize, moved: &BTreeSet<usize>, anchor: usize, after: bool) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::with_capacity(count);
    for index in 0..count {
        if moved.contains(&index) {
            continue;
        }
        if index == anchor && !after {
            order.extend(moved.iter().copied());
        }
        order.push(index);
        if index == anchor && after {
            order.extend(moved.iter().copied());
        }
    }
    order
}

fn full_order(
    git: &Git,
    view: &View,
    range: &[CommitInfo],
    specs: &[String],
) -> Result<Vec<usize>> {
    let mut order = Vec::new();
    for spec in specs {
        let position = locate(git, view, range, spec)?;
        if order.contains(&position) {
            return Err(fail(
                Code::Usage,
                format!("--order lists {} twice", range[position].short),
            ));
        }
        order.push(position);
    }
    if order.len() != range.len() {
        let missing: Vec<String> = (0..range.len())
            .filter(|index| !order.contains(index))
            .map(|index| {
                format!(
                    "{} \"{}\"",
                    range[index].short,
                    escape_line(&range[index].subject)
                )
            })
            .collect();
        return Err(fail_hint(
            Code::Usage,
            format!(
                "--order must list all {} commits of the range, oldest first; missing: {}",
                range.len(),
                missing.join(", ")
            ),
            "run aisc log for the range",
        ));
    }
    Ok(order)
}

pub(crate) fn run(args: &cli::Move) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, false, false)?;
    let branch_option = args.opts.branch.as_deref();
    let view = view::read(&git, branch_option, LIMIT, false)?;
    let range = linear_range(&view);
    if range.is_empty() {
        return Err(fail(Code::State, "there are no commits to move"));
    }

    let order: Vec<usize> = if args.order {
        full_order(&git, &view, &range, &args.commits)?
    } else {
        let mut moved = BTreeSet::new();
        for spec in &args.commits {
            let position = locate(&git, &view, &range, spec)?;
            if !moved.insert(position) {
                return Err(fail(
                    Code::Usage,
                    format!("{} is named twice", range[position].short),
                ));
            }
        }
        let (anchor_spec, after) = match (&args.before, &args.after) {
            (Some(anchor), None) => (anchor, false),
            (None, Some(anchor)) => (anchor, true),
            _ => {
                return Err(fail(
                    Code::Usage,
                    "use exactly one of --before, --after, --order",
                ));
            }
        };
        let anchor = locate(&git, &view, &range, anchor_spec)?;
        if moved.contains(&anchor) {
            return Err(fail(
                Code::Usage,
                format!("{} cannot be moved relative to itself", range[anchor].short),
            ));
        }
        placed(range.len(), &moved, anchor, after)
    };
    let first = order
        .iter()
        .enumerate()
        .position(|(index, position)| index != *position)
        .ok_or_else(|| {
            fail(
                Code::State,
                "the commits are already in that order; nothing changed",
            )
        })?;
    require_linear(&range, first)?;

    let sources = range_sources(&git, &range)?;
    let mut parent = if first > 0 {
        Onto {
            oid: Some(sources[first - 1].oid.clone()),
            tree: sources[first - 1].tree.clone(),
        }
    } else {
        Onto {
            oid: sources[0].parent.clone(),
            tree: sources[0].parent_tree.clone(),
        }
    };
    let mut rewritten = Vec::new();
    for position in &order[first..] {
        let src = &sources[*position];
        let (oid, tree) = match replay(&git, src, &parent)? {
            Step::Conflict(paths) => return Err(conflict_error(src, &paths)),
            Step::Reuse => (src.oid.clone(), src.tree.clone()),
            Step::Merged(tree) => {
                let created = recommit(&git, src, &tree, parent.oid.as_ref())?;
                (created, tree)
            }
        };
        if oid != src.oid {
            rewritten.push((src.oid.clone(), oid.clone()));
        }
        parent = Onto {
            oid: Some(oid),
            tree,
        };
    }
    let new_tip = parent.oid.clone().expect("a replay produced a commit");
    if view.is_head && parent.tree != range[range.len() - 1].tree {
        return Err(fail_hint(
            Code::Unsupported,
            "the new order changes the branch's final tree, which would rewrite the worktree",
            "reorder with --branch on a branch that is not checked out, or move independent commits only",
        ));
    }
    let subjects: Vec<String> = order
        .iter()
        .map(|position| format!("\"{}\"", escape_line(&range[*position].subject)))
        .collect();
    let finished = finish_rewrite(
        &git,
        session,
        &view,
        "move",
        format!("reordered {} commits", range.len()),
        &new_tip,
        rewritten.clone(),
    )?;
    let line = format!(
        "moved {} commit{}; order now {}",
        rewritten.len(),
        if rewritten.len() == 1 { "" } else { "s" },
        subjects.join(" > ")
    );
    let report = Report {
        branch_option,
        quiet: args.opts.quiet,
        verb: "move",
        line,
        old: &view.tip,
        new: &new_tip,
        rewritten: &rewritten,
    };
    outcome(&git, &report, &finished)
}

#[cfg(test)]
mod tests {
    use super::placed;
    use std::collections::BTreeSet;

    #[test]
    fn places_moved_commits_before_and_after_the_anchor() {
        let moved: BTreeSet<usize> = [1, 2].into_iter().collect();
        assert_eq!(placed(6, &moved, 4, true), [0, 3, 4, 1, 2, 5]);
        assert_eq!(placed(6, &moved, 4, false), [0, 3, 1, 2, 4, 5]);
        assert_eq!(placed(4, &moved, 0, false), [1, 2, 0, 3]);
    }
}
