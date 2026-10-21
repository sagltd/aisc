//! `aisc undo`: reverses a recorded operation; undoing an undo redoes it.
use super::oplog::{self, OpData};
use super::plumbing::Git;
use super::render;
use super::rewrite::{HeadMove, IndexChange, Plan, RefMove, Session, WorktreeWrite, stale};
use super::{Code, Outcome, fail_hint, view};
use crate::git::Repo;
use anyhow::Result;
use serde_json::json;

pub(crate) fn run(spec: Option<&str>) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, true, false)?;
    let (op_oid, data) = oplog::load(&git, spec)?;
    check_current(&git, &session, &data)?;

    let mut plan = Plan {
        verb: "undo".to_owned(),
        summary: format!("undid {} ({})", oplog::short(&op_oid), data.verb),
        argv: std::env::args().skip(1).collect(),
        undo_of: Some(op_oid.clone()),
        ..Plan::default()
    };
    for movement in &data.refs {
        plan.refs.push(RefMove {
            name: movement.name.clone(),
            old: movement.new.clone(),
            new: movement.old.clone(),
        });
    }
    plan.head = data.head.as_ref().map(|head| HeadMove {
        from: Some(head.to.clone()),
        to: head.from.clone().unwrap_or_default(),
    });
    if let Some(head) = &data.head
        && head.from.is_none()
    {
        // The op attached a detached HEAD; undo detaches it again at the old commit.
        return Err(fail_hint(
            Code::Unsupported,
            "this operation attached a detached HEAD",
            "switch back with git switch --detach",
        ));
    }
    plan.index = data
        .index
        .iter()
        .map(|change| IndexChange {
            path: change.path.clone(),
            from: change.to.clone(),
            to: change.from.clone(),
        })
        .collect();
    for file in &data.worktree {
        let before = match file.before {
            Some(position) => {
                Some(git.run(&["cat-file", "blob", &format!("{op_oid}:wt/{position}")])?)
            }
            None => None,
        };
        let after = match &file.after {
            Some(oid) => Some(git.read_blobs(std::slice::from_ref(oid))?.remove(0)),
            None => None,
        };
        plan.worktree.push(WorktreeWrite {
            path: file.path.clone(),
            before: after,
            after: before,
            mode: file.mode.clone(),
        });
    }
    let applied = session.apply(&plan)?;
    let after = view::read(&git, None, 20, true)?;
    let result = format!(
        "undid {} ({}); op {}",
        oplog::short(&op_oid),
        data.verb,
        applied.op
    );
    Ok(Outcome::with_text(
        json!({"op": applied.op, "verb": "undo", "undid": oplog::short(&op_oid),
            "log": render::log_json(&after, false)}),
        format!("{result}\n{}", render::compact_text(&after)),
    ))
}

/// Every reference must still be where the op left it; files must still hold what it wrote.
fn check_current(git: &Git, session: &Session, data: &OpData) -> Result<()> {
    for movement in &data.refs {
        let current = if movement.name == "HEAD" {
            Some(session.head.oid.clone())
        } else {
            git.ref_oid(&movement.name)?
        };
        if current != movement.new {
            return Err(stale(&format!(
                "{} moved after that operation",
                movement.name.trim_start_matches("refs/heads/")
            )));
        }
    }
    if let Some(head) = &data.head
        && session.head.symbolic.as_deref() != Some(head.to.as_str())
    {
        return Err(stale("HEAD moved after that operation"));
    }
    for file in &data.worktree {
        let path = git.root.join(&file.path);
        let current = std::fs::read(&path).ok();
        let expected = match &file.after {
            Some(oid) => Some(git.read_blobs(std::slice::from_ref(oid))?.remove(0)),
            None => None,
        };
        if current != expected {
            return Err(fail_hint(
                Code::Dirty,
                format!("{} changed after that operation", file.path),
                "revert or save your edits, then retry",
            ));
        }
    }
    Ok(())
}
