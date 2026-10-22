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
    let plan = reverse_plan(&git, &op_oid, &data)?;
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

/// The plan that reverses a recorded operation (refs, HEAD, index entries, files and modes).
pub(crate) fn reverse_plan(git: &Git, op_oid: &str, data: &OpData) -> Result<Plan> {
    let mut plan = Plan {
        verb: "undo".to_owned(),
        summary: format!("undid {} ({})", oplog::short(op_oid), data.verb),
        argv: std::env::args().skip(1).collect(),
        undo_of: Some(op_oid.to_owned()),
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
    plan.restore_ita = data.index_ita.clone();
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
            // Restores exec bits (and mode-only changes): the old mode comes back.
            mode: file
                .before_mode
                .clone()
                .unwrap_or_else(|| file.mode.clone()),
            before_mode: Some(file.mode.clone()),
        });
    }
    Ok(plan)
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
    // Records without a HEAD state (version 1) fail closed: HEAD must be on a branch the
    // operation moved, so index changes are never staged on an unrelated branch.
    let legacy_ok = data.head_after.is_none()
        && session
            .head
            .symbolic
            .as_ref()
            .is_some_and(|current| data.refs.iter().any(|movement| &movement.name == current));
    if data.head_after.is_none() && !legacy_ok {
        return Err(fail_hint(
            Code::Stale,
            "HEAD is not on a branch this operation moved; nothing was changed",
            "switch back to the branch the operation ran on, then run aisc undo",
        ));
    }
    if let Some(after) = &data.head_after
        && (session.head.symbolic != after.symbolic || session.head.oid != after.oid)
    {
        let now = session
            .head
            .symbolic
            .as_deref()
            .map_or("a detached HEAD", |name| {
                name.trim_start_matches("refs/heads/")
            });
        return Err(fail_hint(
            Code::Stale,
            format!("HEAD is on {now}, not where that operation left it; nothing was changed"),
            "switch back to the branch the operation ran on, then run aisc undo",
        ));
    }
    for file in &data.worktree {
        let path = git.root.join(&file.path);
        let current = std::fs::read(&path).ok();
        let expected = match &file.after {
            Some(oid) => Some(git.read_blobs(std::slice::from_ref(oid))?.remove(0)),
            None => None,
        };
        let mode_differs = expected.is_some()
            && file.mode != "120000"
            && std::fs::metadata(&path).is_ok_and(|metadata| {
                use std::os::unix::fs::PermissionsExt;
                (metadata.permissions().mode() & 0o111 != 0) != (file.mode == "100755")
            });
        if current != expected || mode_differs {
            return Err(fail_hint(
                Code::Dirty,
                format!("{} changed after that operation", file.path),
                "revert or save your edits, then retry",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::rewrite::WorktreeWrite;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn undo_restores_exec_bits_for_mode_only_and_content_changes() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@example.invalid"]);
        std::fs::write(root.join("s.sh"), "echo\n").unwrap();
        std::fs::write(root.join("t.sh"), "old\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        let repo = Repo {
            root: root.clone(),
            directory: root.join(".git"),
        };
        let git = Git::from_repo(&repo);
        let exec = |name: &str| {
            std::fs::metadata(root.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o111
                != 0
        };
        let forward = Plan {
            verb: "test".to_owned(),
            summary: "chmod".to_owned(),
            worktree: vec![
                WorktreeWrite {
                    path: "s.sh".to_owned(),
                    before: Some(b"echo\n".to_vec()),
                    after: Some(b"echo\n".to_vec()),
                    mode: "100755".to_owned(),
                    before_mode: Some("100644".to_owned()),
                },
                WorktreeWrite {
                    path: "t.sh".to_owned(),
                    before: Some(b"old\n".to_vec()),
                    after: Some(b"new\n".to_vec()),
                    mode: "100755".to_owned(),
                    before_mode: Some("100644".to_owned()),
                },
            ],
            ..Plan::default()
        };
        Session::open(&repo, &git, false, false)
            .unwrap()
            .apply(&forward)
            .unwrap();
        assert!(exec("s.sh") && exec("t.sh"));
        let (op, data) = oplog::load(&git, None).unwrap();
        assert_eq!(data.worktree[0].before_mode.as_deref(), Some("100644"));
        let session = Session::open(&repo, &git, false, false).unwrap();
        check_current(&git, &session, &data).unwrap();
        session
            .apply(&reverse_plan(&git, &op, &data).unwrap())
            .unwrap();
        assert!(!exec("s.sh") && !exec("t.sh"), "exec bits restored");
        assert_eq!(std::fs::read_to_string(root.join("t.sh")).unwrap(), "old\n");
        // A mode drifting after the op blocks undo (dirty) instead of a silent overwrite.
        let (_, data) = oplog::load(&git, None).unwrap();
        std::fs::set_permissions(root.join("s.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let session = Session::open(&repo, &git, false, false).unwrap();
        assert!(check_current(&git, &session, &data).is_err());
    }
}
