//! `aisc land` (integrate branches into a target by replay or fast-forward) and `aisc switch`
//! (move HEAD carrying uncommitted edits). Both build one rewrite plan; any refusal changes nothing.
use super::plumbing::Git;
use super::render;
use super::rewrite::{HeadMove, Plan, RefMove, Session, carry};
use super::sync::{ReplayOpts, Resolutions, build_commits, load_picks, replay};
use super::view;
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::json;
use std::collections::BTreeSet;

fn short(oid: &str) -> String {
    oid.chars().take(4).collect()
}

pub(crate) fn run(args: &cli::Land) -> Result<Outcome> {
    if args.opts.branch.is_some() {
        return Err(fail_hint(
            Code::Usage,
            "land does not take --branch",
            "use --into TARGET",
        ));
    }
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let head = git.head_state()?;
    // Target branch: --into, else the base branch (must be local).
    let target = match &args.into {
        Some(name) => name.clone(),
        None => {
            let base = view::read(&git, None, 1, false)?.base;
            match base {
                Some(base) if base.refname.starts_with("refs/heads/") => base.label,
                _ => {
                    return Err(fail_hint(
                        Code::Usage,
                        "no local base branch to land into",
                        "pass --into TARGET",
                    ));
                }
            }
        }
    };
    let target_ref = format!("refs/heads/{target}");
    let here = head.symbolic.as_deref() == Some(target_ref.as_str());
    let carrying = here || args.switch;
    let session = Session::open(&repo, &git, carrying, false)?;
    let old_target = git.ref_oid(&target_ref)?.ok_or_else(|| {
        fail_hint(
            Code::Selector,
            format!("unknown target branch '{target}'"),
            "use a local branch name",
        )
    })?;
    if !here && git.checked_out()?.contains(&target_ref) {
        return Err(fail_hint(
            Code::State,
            format!("'{target}' is checked out in another worktree"),
            "run aisc land from that worktree",
        ));
    }
    let resolutions = Resolutions::new();
    let mut tip = old_target.clone();
    let mut landed = Vec::new();
    let mut rewritten: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut named = BTreeSet::new();
    for name in &args.branches {
        if !named.insert(name) {
            return Err(fail(Code::Usage, format!("branch '{name}' is named twice")));
        }
        if *name == target {
            return Err(fail(Code::Usage, "cannot land a branch into itself"));
        }
        let source = git.ref_oid(&format!("refs/heads/{name}"))?.ok_or_else(|| {
            fail_hint(
                Code::Selector,
                format!("unknown branch '{name}'"),
                "use a local branch name",
            )
        })?;
        sources.push((name.clone(), source.clone()));
        if git
            .probe(&["merge-base", "--is-ancestor", &source, &tip])?
            .is_some()
        {
            landed.push(
                json!({"branch": name, "commits": 0, "fast_forward": false, "already": true}),
            );
            continue;
        }
        let picks = load_picks(&git, &source, &tip)?;
        total += picks.len();
        if git
            .probe(&["merge-base", "--is-ancestor", &tip, &source])?
            .is_some()
        {
            tip = source;
            landed.push(json!({"branch": name, "commits": picks.len(), "fast_forward": true}));
            continue;
        }
        let tip_tree = git.commit_tree_of(&tip)?;
        let opts = ReplayOpts {
            onto_label: &target,
            resolutions: &resolutions,
            prefer_branch: &[],
            prefer_onto: &[],
        };
        let replayed = replay(&git, &tip_tree, &picks, &opts)?;
        if !replayed.pending.is_empty() {
            let mut message =
                format!("landing {name} into {target} conflicts; nothing was changed");
            for conflict in &replayed.pending {
                let pick = &picks[conflict.commit];
                message.push_str(&format!(
                    "\ncommit {} \"{}\": {} conflicts",
                    pick.short,
                    escape_line(&pick.subject),
                    escape_line(&conflict.path)
                ));
            }
            return Err(fail_hint(
                Code::Conflict,
                message,
                format!("aisc sync --branch {name} {target}, resolve there, then land again"),
            ));
        }
        let built = build_commits(&git, &tip, &picks, &replayed.trees)?;
        rewritten.extend(built.rewritten);
        landed.push(
            json!({"branch": name, "commits": picks.len() - built.dropped.len(),
            "fast_forward": false, "dropped": built.dropped.len()}),
        );
        tip = built.tip;
    }
    if tip == old_target && !args.delete {
        let names = args.branches.join(", ");
        return Ok(Outcome::with_text(
            json!({"verb": "land", "status": "up-to-date", "branch": target, "landed": landed}),
            format!("nothing to land: {names} already in {target}"),
        ));
    }
    let mut plan = Plan {
        verb: "land".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    // With nothing new to land, the unchanged target is still compare-and-swapped, so deletions
    // never commit against a target that moved underneath.
    plan.refs.push(RefMove {
        name: target_ref.clone(),
        old: Some(old_target.clone()),
        new: Some(tip.clone()),
    });
    if args.delete {
        // Every landed branch's work is now in the target; the same transaction deletes them, so
        // `aisc undo` brings them back.
        let checked_out = git.checked_out()?;
        let head_moves_away = carrying && !here;
        for (name, oid) in &sources {
            let reference = format!("refs/heads/{name}");
            let on_head = head.symbolic.as_deref() == Some(reference.as_str());
            if (on_head && !head_moves_away) || (!on_head && checked_out.contains(&reference)) {
                return Err(fail_hint(
                    Code::State,
                    format!("'{name}' is checked out, so it cannot be deleted"),
                    "add --switch to move to the target first, or land without --delete",
                ));
            }
            plan.refs.push(RefMove {
                name: reference,
                old: Some(oid.clone()),
                new: None,
            });
        }
    }
    if carrying {
        let from_oid = if here { &old_target } else { &head.oid };
        let from_tree = git.commit_tree_of(from_oid)?;
        let to_tree = git.commit_tree_of(&tip)?;
        let carried = carry(&git, &from_tree, &to_tree, &BTreeSet::new())?;
        plan.worktree = carried.writes;
        plan.index = carried.index;
        if !here {
            plan.head = Some(HeadMove {
                from: head.symbolic.clone(),
                to: target_ref.clone(),
            });
        }
    }
    plan.summary = format!(
        "{} into {target} ({total} commits)",
        args.branches.join(", ")
    );
    let applied = session.apply(&plan)?;
    let after = super::summary::after(&git, None, true)?;
    let deleted = if args.delete {
        format!("; deleted {}", args.branches.join(", "))
    } else {
        String::new()
    };
    let result = format!(
        "landed {} into {target} ({total} commits, {} → {}){deleted}; op {}",
        args.branches.join(", "),
        short(&old_target),
        short(&tip),
        applied.op
    );
    let text = if args.opts.quiet {
        result
    } else {
        super::summary::text(&after, &result)
    };
    Ok(Outcome::with_text(
        json!({"op": applied.op, "verb": "land", "branch": target, "old": old_target,
            "new": tip, "landed": landed,
            "rewritten": rewritten.iter().map(|(o, n)| json!({"old": o, "new": n})).collect::<Vec<_>>(),
            "head_changed_to": plan.head.as_ref().map(|h| h.to.clone()),
            "log": render::log_json(&after, false)}),
        text,
    ))
}

pub(crate) fn switch(args: &cli::Switch) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    git.valid_branch_name(&args.branch)?;
    let reference = format!("refs/heads/{}", args.branch);
    let session = Session::open(&repo, &git, true, false)?;
    let head = &session.head;
    if head.symbolic.as_deref() == Some(reference.as_str()) && !args.create {
        return Ok(Outcome::with_text(
            json!({"verb": "switch", "status": "unchanged", "branch": args.branch}),
            format!("already on {}", args.branch),
        ));
    }
    let existing = git.ref_oid(&reference)?;
    let mut plan = Plan {
        verb: "switch".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    if args.create {
        if existing.is_some() {
            return Err(fail_hint(
                Code::State,
                format!("branch '{}' already exists", args.branch),
                "drop -c to switch to it",
            ));
        }
        plan.refs.push(RefMove {
            name: reference.clone(),
            old: None,
            new: Some(head.oid.clone()),
        });
    } else {
        let target = existing.ok_or_else(|| {
            fail_hint(
                Code::Selector,
                format!("unknown branch '{}'", args.branch),
                "use -c to create it",
            )
        })?;
        if git.checked_out()?.contains(&reference) {
            return Err(fail_hint(
                Code::State,
                format!("'{}' is checked out in another worktree", args.branch),
                "pick another branch",
            ));
        }
        let from_tree = git.commit_tree_of(&head.oid)?;
        let to_tree = git.commit_tree_of(&target)?;
        let carried = carry(&git, &from_tree, &to_tree, &BTreeSet::new())?;
        plan.worktree = carried.writes;
        plan.index = carried.index;
    }
    plan.head = Some(HeadMove {
        from: head.symbolic.clone(),
        to: reference,
    });
    plan.summary = format!("to {}", args.branch);
    let applied = session.apply(&plan)?;
    let after = super::summary::after(&git, None, true)?;
    let result = format!(
        "switched to {}{}; op {}",
        args.branch,
        if args.create { " (new)" } else { "" },
        applied.op
    );
    Ok(Outcome::with_text(
        json!({"op": applied.op, "verb": "switch", "branch": args.branch,
            "log": render::log_json(&after, false)}),
        if args.quiet {
            result
        } else {
            super::summary::text(&after, &result)
        },
    ))
}
