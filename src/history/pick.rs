//! `aisc pick`: copies commits (author and message kept) onto the current branch, another branch
//! that is not checked out, or a new branch, replaying with `merge-tree`.
use super::plumbing::{Entry, Git};
use super::render;
use super::reorder::{
    LIMIT, Onto, Src, Step, conflict_error, dirty_paths, first_line, recommit, replay, short_ids,
};
use super::rewrite::{IndexChange, Plan, RefMove, Session, WorktreeWrite, stale};
use super::view::{self, View};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::{Repo, text};
use crate::output::escape_line;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;

enum Dest {
    /// The checked-out branch (or detached HEAD); the worktree is carried.
    Head,
    Branch(String),
    New(String),
}

/// Source commit data: from the view's range when possible, else one `git log`.
fn source(git: &Git, views: &[&View], oid: &str) -> Result<Src> {
    for view in views {
        if let Some(commit) = view.commits.iter().find(|commit| commit.oid == oid) {
            let parent = commit.parents.first().cloned();
            if commit.parents.len() > 1 {
                return Err(merge_error(&commit.short));
            }
            let parent_tree = match &parent {
                Some(parent) => git.commit_tree_of(parent)?,
                None => git.empty_tree()?,
            };
            return Ok(Src {
                oid: commit.oid.clone(),
                short: commit.short.clone(),
                tree: commit.tree.clone(),
                parent,
                parent_tree,
                subject: commit.subject.clone(),
            });
        }
    }
    let line = git.line(&[
        "log",
        "-1",
        "--abbrev=4",
        "--format=%H%x1f%h%x1f%T%x1f%P%x1f%s",
        oid,
    ])?;
    let fields: Vec<&str> = line.split('\u{1f}').collect();
    let [full, short, tree, parents, subject] = fields[..] else {
        anyhow::bail!("unexpected git log output for {oid}");
    };
    let parents: Vec<&str> = parents.split_whitespace().collect();
    if parents.len() > 1 {
        return Err(merge_error(short));
    }
    let parent = parents.first().map(|parent| (*parent).to_owned());
    let parent_tree = match &parent {
        Some(parent) => git.commit_tree_of(parent)?,
        None => git.empty_tree()?,
    };
    Ok(Src {
        oid: full.to_owned(),
        short: short.to_owned(),
        tree: tree.to_owned(),
        parent,
        parent_tree,
        subject: subject.to_owned(),
    })
}

fn merge_error(short: &str) -> anyhow::Error {
    fail_hint(
        Code::Unsupported,
        format!("commit {short} is a merge and cannot be picked"),
        "pick its parents' commits instead",
    )
}

/// `:oldmode newmode oldoid newoid status\0path\0` records of `diff-tree -r --raw -z`.
type Raw = (String, Option<Entry>, Option<Entry>);

fn tree_changes(git: &Git, old: &str, new: &str) -> Result<Vec<Raw>> {
    let output = git.run(&[
        "diff-tree",
        "-r",
        "-z",
        "--raw",
        "--no-renames",
        "--no-abbrev",
        old,
        new,
    ])?;
    let mut fields = output.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields.next().context("diff-tree record without a path")?;
        let meta = text(meta)?;
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, old_oid, new_oid, _status] = parts[..] else {
            anyhow::bail!("malformed diff-tree record");
        };
        let entry =
            |mode: &str, oid: &str| (mode != "000000").then(|| (mode.to_owned(), oid.to_owned()));
        changes.push((
            String::from_utf8(path.to_vec())?,
            entry(old_mode, old_oid),
            entry(new_mode, new_oid),
        ));
    }
    Ok(changes)
}

/// Checks that writing `path` cannot fail later: no symlinked or file-shaped parents, no
/// untracked or ignored file or directory in the way.
fn preflight(
    root: &std::path::Path,
    path: &str,
    tracked: bool,
    written: bool,
    removed: &std::collections::BTreeSet<&str>,
    dirty: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let blocked = |why: &str| {
        fail_hint(
            Code::Dirty,
            format!("{path}: {why}"),
            "move it away first, or pick onto another branch with -b NEW",
        )
    };
    let mut current = root.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return Ok(()); // nothing there: parents get created
        };
        let last = index + 1 == parts.len();
        if !last {
            let prefix = parts[..=index].join("/");
            if metadata.file_type().is_symlink() {
                return Err(blocked(&format!("{prefix} is a symlink")));
            }
            if !metadata.is_dir() && written {
                // A tracked file that this very pick deletes may be replaced by a directory.
                let replaced = removed.contains(prefix.as_str()) && !dirty.contains(&prefix);
                if !replaced {
                    return Err(blocked(&format!(
                        "{prefix} is a file where a directory is needed"
                    )));
                }
            }
        } else if written && metadata.is_dir() {
            return Err(blocked(
                "a directory is where the picked commit puts a file",
            ));
        } else if !tracked && written {
            return Err(blocked("an untracked or ignored file is in the way"));
        }
    }
    Ok(())
}

/// Worktree and index edits that carry the checked-out branch from `old` to `new` tree.
fn carry(git: &Git, view: &View, old: &str, new: &str, plan: &mut Plan) -> Result<()> {
    let status = view.status.as_ref().context("missing status")?;
    if !status.unmerged.is_empty() {
        return Err(fail(Code::Dirty, "resolve the unmerged paths first"));
    }
    let dirty = dirty_paths(view.changes.as_ref());
    let staged: std::collections::BTreeSet<String> = git
        .run(&["diff", "--cached", "--name-only", "-z", "--no-renames"])?
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    let changes = tree_changes(git, old, new)?;
    let removed: std::collections::BTreeSet<&str> = changes
        .iter()
        .filter(|(_, _, after)| after.is_none())
        .map(|(path, _, _)| path.as_str())
        .collect();
    let mut blobs: Vec<String> = Vec::new();
    for (path, before, after) in &changes {
        for entry in [before, after].into_iter().flatten() {
            if entry.0 == "160000" {
                return Err(fail(
                    Code::Unsupported,
                    format!("{path}: submodules are not supported"),
                ));
            }
            blobs.push(entry.1.clone());
        }
        let hint = "commit or discard them first, or pick onto another branch with -b NEW";
        if dirty.contains(path) {
            return Err(fail_hint(
                Code::Dirty,
                format!("{path} has uncommitted changes that overlap the picked commits"),
                hint,
            ));
        }
        if staged.contains(path) {
            return Err(fail_hint(
                Code::Dirty,
                format!("{path} has staged changes that overlap the picked commits"),
                hint,
            ));
        }
        preflight(
            &git.root,
            path,
            before.is_some(),
            after.is_some(),
            &removed,
            &dirty,
        )?;
    }
    blobs.sort();
    blobs.dedup();
    let contents = git.read_blobs(&blobs)?;
    let by_oid: BTreeMap<&String, &Vec<u8>> = blobs.iter().zip(&contents).collect();
    for (path, before, after) in changes {
        plan.index.push(IndexChange {
            path: path.clone(),
            from: before.clone(),
            to: after.clone(),
        });
        let mode = after
            .as_ref()
            .or(before.as_ref())
            .map(|entry| entry.0.clone())
            .unwrap_or_default();
        plan.worktree.push(WorktreeWrite {
            path,
            before: before.map(|entry| by_oid[&entry.1].clone()),
            after: after.map(|entry| by_oid[&entry.1].clone()),
            mode,
        });
    }
    Ok(())
}

pub(crate) fn run(args: &cli::Pick) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    if args.opts.branch.is_some() && args.new_branch.is_some() {
        return Err(fail(
            Code::Usage,
            "use -b NEW to create a branch, or --branch/--to to pick onto an existing one, not both",
        ));
    }
    if args.opts.branch.is_some() && args.to.is_some() {
        return Err(fail(Code::Usage, "use --to or --branch, not both"));
    }
    let session = Session::open(&repo, &git, true, false)?;
    let head_view = view::read(&git, None, LIMIT, true)?;
    let from_view = match &args.from {
        Some(name) if head_view.branch.as_deref() != Some(name.as_str()) => {
            Some(view::read(&git, Some(name), LIMIT, false)?)
        }
        _ => None,
    };
    let resolving = from_view.as_ref().unwrap_or(&head_view);

    // Destination.
    let to = args.to.clone().or_else(|| args.opts.branch.clone());
    let checked_out = git.checked_out()?;
    let (dest, tip) = match (&to, &args.new_branch) {
        (Some(name), _) if head_view.branch.as_deref() == Some(name.as_str()) => {
            (Dest::Head, head_view.tip.clone())
        }
        (Some(name), _) => {
            let reference = format!("refs/heads/{name}");
            let tip = git.ref_oid(&reference)?.ok_or_else(|| {
                fail_hint(
                    Code::Selector,
                    format!("unknown branch '{name}'"),
                    "use a local branch name, or -b NEW to create one",
                )
            })?;
            if checked_out.contains(&reference) {
                return Err(fail_hint(
                    Code::State,
                    format!("branch '{name}' is checked out in another worktree"),
                    "pick from that worktree, or use -b NEW",
                ));
            }
            (Dest::Branch(name.clone()), tip)
        }
        (None, Some(name)) => {
            git.valid_branch_name(name)?;
            if git.ref_oid(&format!("refs/heads/{name}"))?.is_some() {
                return Err(fail_hint(
                    Code::State,
                    format!("branch '{name}' already exists"),
                    "pick another name, or use --to",
                ));
            }
            let start = match &args.onto {
                Some(spec) => head_view.resolve(&git, spec)?,
                None => head_view
                    .base
                    .as_ref()
                    .or(resolving.base.as_ref())
                    .map(|base| base.oid.clone())
                    .ok_or_else(|| {
                        fail_hint(Code::Usage, "there is no base branch", "pass --onto BASE")
                    })?,
            };
            (Dest::New(name.clone()), start)
        }
        (None, None) => (Dest::Head, head_view.tip.clone()),
    };

    // Commits, oldest first when they all come from the viewed range.
    let mut oids: Vec<String> = Vec::new();
    for spec in &args.commits {
        let oid = resolving.resolve(&git, spec)?;
        if oids.contains(&oid) {
            return Err(fail(Code::Usage, format!("{spec} is named twice")));
        }
        oids.push(oid);
    }
    let position = |oid: &String| resolving.commits.iter().position(|c| c.oid == *oid);
    if oids.iter().all(|oid| position(oid).is_some()) {
        oids.sort_by_key(|oid| std::cmp::Reverse(position(oid)));
    }
    let sources = oids
        .iter()
        .map(|oid| source(&git, &[resolving, &head_view], oid))
        .collect::<Result<Vec<_>>>()?;

    // Replay.
    let tip_tree = git.commit_tree_of(&tip)?;
    let mut onto = Onto {
        oid: Some(tip.clone()),
        tree: tip_tree.clone(),
    };
    let mut created: Vec<(String, String)> = Vec::new(); // (new oid, subject)
    let mut skipped: Vec<String> = Vec::new();
    for src in &sources {
        match replay(&git, src, &onto)? {
            Step::Conflict(paths) => return Err(conflict_error(src, &paths)),
            Step::Reuse => {
                // Already the child of the destination tip: nothing to copy.
                onto = Onto {
                    oid: Some(src.oid.clone()),
                    tree: src.tree.clone(),
                };
                created.push((src.oid.clone(), src.subject.clone()));
            }
            Step::Merged(tree) if tree == onto.tree => skipped.push(src.short.clone()),
            Step::Merged(tree) => {
                let commit = recommit(&git, src, &tree, onto.oid.as_ref())?;
                onto = Onto {
                    oid: Some(commit.clone()),
                    tree,
                };
                created.push((commit, src.subject.clone()));
            }
        }
    }
    if created.is_empty() {
        return Err(fail_hint(
            Code::State,
            format!("nothing to pick: {} already applied", skipped.join(", ")),
            "the destination already contains these changes",
        ));
    }
    let new_tip = onto.oid.clone().expect("a pick produced a commit");

    // Plan.
    let mut plan = Plan {
        verb: "pick".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    let (target_ref, target_name, new_branch) = match &dest {
        Dest::Head => {
            if session.head.oid != head_view.tip {
                return Err(stale("HEAD changed while aisc was reading"));
            }
            carry(&git, &head_view, &tip_tree, &onto.tree, &mut plan)?;
            match &session.head.symbolic {
                Some(symbolic) => (
                    symbolic.clone(),
                    symbolic.trim_start_matches("refs/heads/").to_owned(),
                    false,
                ),
                None => ("HEAD".to_owned(), "HEAD (detached)".to_owned(), false),
            }
        }
        Dest::Branch(name) => (format!("refs/heads/{name}"), name.clone(), false),
        Dest::New(name) => (format!("refs/heads/{name}"), name.clone(), true),
    };
    plan.refs.push(RefMove {
        name: target_ref.clone(),
        old: (!new_branch).then(|| tip.clone()),
        new: Some(new_tip.clone()),
    });
    let ids = short_ids(
        &git,
        &created
            .iter()
            .map(|(oid, _)| oid.clone())
            .collect::<Vec<_>>(),
    )?;
    plan.summary = created
        .iter()
        .zip(&ids)
        .map(|((_, subject), id)| format!("{id} {}", first_line(subject)))
        .collect::<Vec<_>>()
        .join(", ");
    let applied = session.apply(&plan)?;

    // Report.
    let described: Vec<String> = created
        .iter()
        .zip(&ids)
        .map(|((_, subject), id)| format!("{id} \"{}\"", escape_line(first_line(subject))))
        .collect();
    let on = if new_branch {
        format!("new branch {target_name}")
    } else {
        target_name.clone()
    };
    let mut line = format!(
        "picked {} onto {on}: {}",
        if created.len() == 1 {
            "1 commit".to_owned()
        } else {
            format!("{} commits", created.len())
        },
        described.join(", ")
    );
    if !skipped.is_empty() {
        line.push_str(&format!(" (already applied: {})", skipped.join(", ")));
    }
    line.push_str(&format!("; op {}", applied.op));
    let shown = match &dest {
        Dest::Head => view::read(&git, None, 20, true)?,
        Dest::Branch(name) | Dest::New(name) => view::read(&git, Some(name), 20, false)?,
    };
    let result_text = if args.opts.quiet {
        line
    } else {
        format!("{line}\n{}", render::compact_text(&shown))
    };
    let value = json!({
        "op": applied.op,
        "verb": "pick",
        "branch": target_name,
        "old": (!new_branch).then_some(tip),
        "new": new_tip,
        "created": created.iter().zip(&ids).map(|((oid, subject), id)|
            json!({"id": id, "oid": oid, "subject": first_line(subject)})).collect::<Vec<_>>(),
        "skipped": skipped,
        "moved_refs": [{"ref": target_ref, "old": plan.refs[0].old, "new": new_tip}],
        "log": render::log_json(&shown, false),
    });
    Ok(Outcome::with_text(value, result_text))
}
