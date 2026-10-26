//! `aisc amend --into COMMIT SEL...` when COMMIT is on another local branch: the selected changes
//! of this worktree are folded into that commit, the branch is rewritten above it, and the changes
//! leave this worktree and index in the same operation. HEAD never moves, and `aisc undo`
//! restores both the branch and the changes here.
use super::commit::tree_with_selection;
use super::edit::replay_order;
use super::plumbing::Git;
use super::reorder::{
    Finished, LIMIT, Onto, Report, linear_range, outcome, prepare_rewrite, range_sources, recommit,
    require_linear, short_ids,
};
use super::rewrite::{Session, stale};
use super::selector;
use super::view::{self, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::output::escape_line;
use anyhow::Result;
use std::time::SystemTime;

/// Local branches, other than the view's own and its base, whose rewritable range holds `spec`.
pub(crate) fn branches_holding(git: &Git, view: &View, spec: &str) -> Result<Vec<String>> {
    let own = view.branch.as_deref();
    let base = view
        .base
        .as_ref()
        .map(|base| base.label.trim_start_matches("origin/"));
    let mut found = Vec::new();
    for local in &view.locals {
        let name = local.name.as_str();
        if Some(name) == own || Some(name) == base {
            continue;
        }
        let Ok(other) = view::read(git, Some(name), 1_000, false) else {
            continue;
        };
        if let Ok(Some(oid)) = other.resolve_opt(git, spec)
            && other.commits.iter().any(|commit| commit.oid == oid)
        {
            found.push(local.name.clone());
        }
    }
    Ok(found)
}

pub(crate) fn run(
    args: &cli::Amend,
    branch: &str,
    spec: &str,
    tokens: &[String],
    git: &Git,
    session: Session,
) -> Result<Outcome> {
    let head_oid = session.head.oid.clone();
    let head_symbolic = session.head.symbolic.clone();
    // The selection leaves the worktree: any change from here on is concurrent.
    let started = SystemTime::now();
    let (status, changes) = view::read_changes(git)?;
    if status.oid.as_deref() != Some(head_oid.as_str()) {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    if !status.unmerged.is_empty() {
        return Err(fail(Code::Dirty, "resolve the unmerged paths first"));
    }
    let selection = selector::resolve(&git.root, &changes, tokens)?;
    let target = view::read(git, Some(branch), LIMIT, false)?;
    if git.checked_out()?.contains(&format!("refs/heads/{branch}")) {
        return Err(fail_hint(
            Code::State,
            format!("branch '{branch}' is checked out in another worktree"),
            "run the command from that worktree",
        ));
    }
    let oid = target.resolve(git, spec)?;
    let range = linear_range(&target);
    let at = range
        .iter()
        .position(|commit| commit.oid == oid)
        .ok_or_else(|| super::amend::out_of_range(&target, &oid))?;
    require_linear(&range, at)?;
    let sources = range_sources(git, &range)?;
    let src = &sources[at];
    let label = format!("{} on {branch}", src.short);
    let placed = tree_with_selection(
        git,
        &changes,
        &selection,
        (&head_oid, head_symbolic.as_deref()),
        &src.oid,
        &label,
        &format!(
            "commit it on {} first, or switch to {branch} and amend there",
            head_symbolic
                .as_deref()
                .map_or("HEAD", |name| name.trim_start_matches("refs/heads/"))
        ),
    )?;
    if placed.tree == src.tree {
        return Err(fail(
            Code::Selector,
            format!("the selection changes nothing in {label}"),
        ));
    }
    let amended = recommit(git, src, &placed.tree, src.parent.as_ref())?;
    let above: Vec<usize> = (at + 1..sources.len()).collect();
    let onto = Onto {
        oid: Some(amended.clone()),
        tree: placed.tree.clone(),
    };
    let (tip, more) = replay_order(git, &sources, &above, onto, None)?;
    let new_tip = tip.oid.expect("a commit was written");
    let mut rewritten = vec![(src.oid.clone(), amended.clone())];
    rewritten.extend(more);
    let removal = super::discard::removal(
        git,
        &changes,
        &selection,
        &head_oid,
        (started, &placed.hashed),
    )?;
    let mut prepared = prepare_rewrite(
        git,
        &session,
        &target,
        "amend",
        format!("amended {} on {branch}", src.short),
        &new_tip,
        rewritten.clone(),
        Vec::new(),
        None,
    )?;
    prepared.plan.worktree = removal.worktree;
    prepared.plan.index = removal.index;
    super::discard::ensure_unchanged(git, &prepared.plan.worktree)?;
    let applied = session.apply(&prepared.plan)?;
    let finished = Finished {
        applied,
        moved_refs: prepared.moved_refs,
        branch: prepared.branch,
    };
    let new_short = short_ids(git, std::slice::from_ref(&amended))?
        .pop()
        .unwrap_or_default();
    let what = if removal.hunks == 1 {
        "1 hunk".to_owned()
    } else {
        format!("{} hunks", removal.hunks)
    };
    let paths: Vec<String> = removal.paths.iter().map(|path| escape_line(path)).collect();
    let report = Report {
        branch_option: Some(branch),
        quiet: args.opts.quiet,
        verb: "amend",
        line: format!(
            "amended {what} ({}) into {}→{new_short} \"{}\" on {branch}; they left this worktree",
            paths.join(", "),
            src.short,
            escape_line(&src.subject)
        ),
        old: &target.tip,
        new: &new_tip,
        rewritten: &rewritten,
    };
    outcome(git, &report, &finished)
}
