//! `aisc reword`, `aisc drop` and `aisc move --to`: edit one commit's message, remove commits,
//! or move commits to another branch, each in one call. Built on the replay helpers of `move`.
use super::plumbing::Git;
use super::reorder::{
    LIMIT, Onto, Report, Src, Step, conflict_error, finish_rewrite, finish_rewrite_with,
    linear_range, locate, outcome, range_sources, recommit, replay, require_linear,
};
use super::rewrite::{RefMove, Session};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use std::collections::BTreeSet;

/// Replays `sources[position]` for each position in `order`, starting on `parent`. Returns the
/// new tip and tree, and every (old, new) commit pair that changed. With `onto_branch`, a commit
/// whose replay changes nothing there is refused: that branch already contains its change, and
/// moving it would only add an empty duplicate.
pub(crate) fn replay_order(
    git: &Git,
    sources: &[Src],
    order: &[usize],
    mut parent: Onto,
    onto_branch: Option<&str>,
) -> Result<(Onto, Vec<(String, String)>)> {
    let mut rewritten = Vec::new();
    for position in order {
        let src = &sources[*position];
        let step = replay(git, src, &parent)?;
        if let (Some(branch), Step::Merged(tree)) = (onto_branch, &step)
            && *tree == parent.tree
        {
            return Err(fail_hint(
                Code::State,
                format!(
                    "'{branch}' already contains \"{}\"; nothing was changed",
                    escape_line(&src.subject)
                ),
                "to remove it from this branch use aisc drop COMMIT...",
            ));
        }
        let (oid, tree) = match step {
            Step::Conflict(paths) => return Err(conflict_error(src, &paths)),
            Step::Reuse => (src.oid.clone(), src.tree.clone()),
            Step::Merged(tree) => (recommit(git, src, &tree, parent.oid.as_ref())?, tree),
        };
        if oid != src.oid {
            rewritten.push((src.oid.clone(), oid.clone()));
        }
        parent = Onto {
            oid: Some(oid),
            tree,
        };
    }
    Ok((parent, rewritten))
}

/// The commit below position `first` of the range: where a rewrite from `first` starts.
fn below(sources: &[Src], first: usize) -> Onto {
    if first > 0 {
        Onto {
            oid: Some(sources[first - 1].oid.clone()),
            tree: sources[first - 1].tree.clone(),
        }
    } else {
        Onto {
            oid: sources[0].parent.clone(),
            tree: sources[0].parent_tree.clone(),
        }
    }
}

/// Distinct positions of `specs` in the range, refusing repeats.
fn positions(
    git: &Git,
    view: &View,
    range: &[CommitInfo],
    specs: &[String],
) -> Result<BTreeSet<usize>> {
    let mut found = BTreeSet::new();
    for spec in specs {
        let position = locate(git, view, range, spec)?;
        if !found.insert(position) {
            return Err(fail(
                Code::Usage,
                format!("{} is named twice", range[position].short),
            ));
        }
    }
    Ok(found)
}

pub(crate) fn reword(args: &cli::Reword) -> Result<Outcome> {
    let message = args.message.trim_end();
    if message.trim().is_empty() {
        return Err(fail(Code::Usage, "the new message is empty"));
    }
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, false, false)?;
    let branch_option = args.opts.branch.as_deref();
    let view = view::read(&git, branch_option, LIMIT, false)?;
    let range = linear_range(&view);
    let target = locate(&git, &view, &range, &args.commit)?;
    require_linear(&range, target)?;
    let sources = range_sources(&git, &range)?;
    let src = &sources[target];
    let mut meta = git.read_commit_meta(&src.oid)?.with_current_committer();
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(b'\n');
    meta.message = bytes.clone();
    let parents: Vec<String> = src.parent.clone().into_iter().collect();
    let reworded = git.commit_tree(&src.tree, &parents, &bytes, Some(&meta))?;
    let mut rewritten = vec![(src.oid.clone(), reworded.clone())];
    let start = Onto {
        oid: Some(reworded),
        tree: src.tree.clone(),
    };
    let rest: Vec<usize> = (target + 1..range.len()).collect();
    let (tip, more) = replay_order(&git, &sources, &rest, start, None)?;
    rewritten.extend(more);
    let new_tip = tip.oid.expect("a commit was written");
    let old_subject = escape_line(&range[target].subject);
    let finished = finish_rewrite(
        &git,
        session,
        &view,
        "reword",
        format!("reworded \"{old_subject}\""),
        &new_tip,
        rewritten.clone(),
    )?;
    let new_subject = escape_line(message.lines().next().unwrap_or(""));
    let report = Report {
        branch_option,
        quiet: args.opts.quiet,
        verb: "reword",
        line: format!("reworded \"{old_subject}\" to \"{new_subject}\""),
        old: &view.tip,
        new: &new_tip,
        rewritten: &rewritten,
    };
    outcome(&git, &report, &finished)
}

pub(crate) fn drop(args: &cli::Drop) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let branch_option = args.opts.branch.as_deref();
    // The worktree is carried only when the branch is checked out, and which branch that is
    // is read after the lock: like split and amend, take the index lock up front, every time.
    let session = Session::open(&repo, &git, true, false)?;
    let view = view::read(&git, branch_option, LIMIT, false)?;
    let range = linear_range(&view);
    let dropped = positions(&git, &view, &range, &args.commits)?;
    let first = *dropped.iter().next().expect("at least one commit");
    require_linear(&range, first)?;
    let sources = range_sources(&git, &range)?;
    let order: Vec<usize> = (first..range.len())
        .filter(|p| !dropped.contains(p))
        .collect();
    let (tip, rewritten) = replay_order(&git, &sources, &order, below(&sources, first), None)?;
    let new_tip = tip.oid.clone().ok_or_else(|| {
        fail(
            Code::Unsupported,
            "dropping every commit down to the root is not supported",
        )
    })?;
    let names: Vec<String> = dropped
        .iter()
        .map(|p| format!("\"{}\"", escape_line(&range[*p].subject)))
        .collect();
    let old_tree = range[range.len() - 1].tree.clone();
    let finished = finish_rewrite_with(
        &git,
        session,
        &view,
        "drop",
        format!("dropped {}", names.join(", ")),
        &new_tip,
        rewritten.clone(),
        Vec::new(),
        Some((&old_tree, &tip.tree)),
    )?;
    let report = Report {
        branch_option,
        quiet: args.opts.quiet,
        verb: "drop",
        line: format!("dropped {}", names.join(", ")),
        old: &view.tip,
        new: &new_tip,
        rewritten: &rewritten,
    };
    outcome(&git, &report, &finished)
}

/// `aisc move COMMIT... --to BRANCH`: the commits are replayed on top of BRANCH and removed
/// from this branch, in one transaction.
pub(crate) fn move_to(args: &cli::Move, target: &str) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    git.valid_branch_name(target)?;
    let branch_option = args.opts.branch.as_deref();
    // Same lock rule as `drop`: the index lock is taken before the branch is read.
    let session = Session::open(&repo, &git, true, false)?;
    let view = view::read(&git, branch_option, LIMIT, false)?;
    if view.branch.as_deref() == Some(target) {
        return Err(fail(
            Code::Usage,
            "--to names the branch the commits are already on",
        ));
    }
    let target_ref = format!("refs/heads/{target}");
    let existing = git.ref_oid(&target_ref)?;
    let target_tip = match (&existing, args.onto.as_deref()) {
        (Some(tip), None) => tip.clone(),
        (Some(_), Some(_)) => {
            return Err(fail_hint(
                Code::Usage,
                format!("branch '{target}' already exists"),
                format!("to move onto it, drop --onto: aisc move COMMIT... --to {target}"),
            ));
        }
        (None, Some(base)) => {
            if let Some(clash) = super::commit::conflicting_branch(&git, target)? {
                return Err(fail_hint(
                    Code::State,
                    format!("branch '{target}' conflicts with existing branch {clash}"),
                    "a branch cannot be both a name and a directory of names; pick another name",
                ));
            }
            git.resolve_commit(base)?.ok_or_else(|| {
                fail_hint(
                    Code::Selector,
                    format!("unknown revision '{base}'"),
                    "--onto names the branch or commit the new branch starts from",
                )
            })?
        }
        (None, None) => {
            return Err(fail_hint(
                Code::Selector,
                format!("unknown branch '{target}'"),
                format!(
                    "use an existing local branch, or start one there: aisc move COMMIT... --to {target} --onto BASE"
                ),
            ));
        }
    };
    if session.head.symbolic.as_deref() == Some(target_ref.as_str()) {
        return Err(fail_hint(
            Code::State,
            format!("'{target}' is checked out here"),
            "switch to another branch first, or use aisc pick --from BRANCH COMMIT... here and then aisc drop --branch BRANCH COMMIT...",
        ));
    }
    if git.checked_out()?.contains(&target_ref) {
        return Err(fail_hint(
            Code::State,
            format!("'{target}' is checked out in another worktree"),
            "run the command from that worktree",
        ));
    }
    let range = linear_range(&view);
    let moved = positions(&git, &view, &range, &args.commits)?;
    let first = *moved.iter().next().expect("at least one commit");
    require_linear(&range, first)?;
    let sources = range_sources(&git, &range)?;
    // Onto the target branch first: refuse before anything changes if they do not apply there.
    let target_tree = git.commit_tree_of(&target_tip)?;
    let onto_target = Onto {
        oid: Some(target_tip.clone()),
        tree: target_tree,
    };
    let picked: Vec<usize> = moved.iter().copied().collect();
    let (target_new, target_rewritten) =
        replay_order(&git, &sources, &picked, onto_target, Some(target))?;
    let target_new = target_new.oid.expect("a commit was written");
    // Then this branch without them.
    let keep: Vec<usize> = (first..range.len())
        .filter(|p| !moved.contains(p))
        .collect();
    let (tip, rewritten) = replay_order(&git, &sources, &keep, below(&sources, first), None)?;
    let new_tip = tip.oid.clone().ok_or_else(|| {
        fail(
            Code::Unsupported,
            "moving every commit down to the root is not supported",
        )
    })?;
    let names: Vec<String> = moved
        .iter()
        .map(|p| format!("\"{}\"", escape_line(&range[*p].subject)))
        .collect();
    let old_tree = range[range.len() - 1].tree.clone();
    let extra = vec![RefMove {
        name: target_ref,
        old: existing.clone(),
        new: Some(target_new.clone()),
    }];
    let mut all = rewritten.clone();
    all.extend(target_rewritten);
    let finished = finish_rewrite_with(
        &git,
        session,
        &view,
        "move",
        format!("moved {} to {target}", names.join(", ")),
        &new_tip,
        rewritten,
        extra,
        Some((&old_tree, &tip.tree)),
    )?;
    let report = Report {
        branch_option,
        quiet: args.opts.quiet,
        verb: "move",
        line: format!(
            "moved {} to {target} ({})",
            names.join(", "),
            &target_new[..7]
        ),
        old: &view.tip,
        new: &new_tip,
        rewritten: &all,
    };
    outcome(&git, &report, &finished)
}
