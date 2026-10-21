//! Rewrite plan executor: one operation lock, compare-and-swap ref transaction, then HEAD,
//! worktree files and the index. Any failure before the ref transaction changes nothing.
#![allow(dead_code)] // worktree writes, dependents and detached heads are consumed by later slices
use super::oplog::{self, OpRecord};
use super::plumbing::{Entry, Git, HeadState, RefTx};
use super::{Code, fail, fail_hint};
use crate::git::{Lock, Repo, text};
use crate::signals;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub(crate) struct RefMove {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Where HEAD points before and after: `None` means detached.
#[derive(Clone, Debug)]
pub(crate) struct HeadMove {
    pub from: Option<String>,
    pub to: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexChange {
    pub path: String,
    pub from: Option<Entry>,
    pub to: Option<Entry>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorktreeWrite {
    pub path: String,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
    pub mode: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub verb: String,
    pub summary: String,
    pub argv: Vec<String>,
    pub refs: Vec<RefMove>,
    pub head: Option<HeadMove>,
    pub rewritten: Vec<(String, String)>,
    pub index: Vec<IndexChange>,
    pub worktree: Vec<WorktreeWrite>,
    pub undo_of: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Applied {
    pub op: String,
    pub op_oid: String,
}

pub(crate) struct Session {
    pub git: Git,
    pub head: HeadState,
    _operation: Lock,
    index_lock: Option<Lock>,
    index_snapshot: Option<Vec<u8>>,
    index_mtime: Option<SystemTime>,
}

fn fingerprint(git: &Git) -> (Option<Vec<u8>>, Option<SystemTime>) {
    let path = git.dir.join("index");
    (
        fs::read(&path).ok(),
        fs::metadata(&path).and_then(|m| m.modified()).ok(),
    )
}

impl Session {
    /// Takes the operation lock (and `index.lock` when the index will change) and snapshots state.
    pub fn open(repo: &Repo, git: &Git, touches_index: bool, allow_paused: bool) -> Result<Self> {
        let operation = Lock::acquire(git.dir.join("aisc-operation.lock"))
            .map_err(|error| fail(Code::State, format!("{error:#}")))?;
        if !allow_paused && git.dir.join("aisc").join("sync.json").exists() {
            return Err(fail_hint(
                Code::State,
                "a sync is paused",
                "run aisc sync --continue or aisc sync --abort first",
            ));
        }
        repo.writable()?;
        let index_lock = if touches_index {
            Some(
                Lock::acquire(git.dir.join("index.lock"))
                    .map_err(|error| fail(Code::State, format!("{error:#}")))?,
            )
        } else {
            None
        };
        let head = git.head_state()?;
        let (index_snapshot, index_mtime) = fingerprint(git);
        Ok(Self {
            git: git.clone(),
            head,
            _operation: operation,
            index_lock,
            index_snapshot,
            index_mtime,
        })
    }

    fn ensure_unchanged(&self) -> Result<()> {
        let head = self.git.head_state()?;
        if head.oid != self.head.oid || head.symbolic != self.head.symbolic {
            return Err(stale("HEAD changed while aisc was working"));
        }
        if self.index_lock.is_some() && fingerprint(&self.git).0 != self.index_snapshot {
            return Err(stale("the index changed while aisc was working"));
        }
        Ok(())
    }

    /// Index bytes with the rule applied: an entry equal to `from` becomes `to`; others are kept.
    fn prepare_index(&self, changes: &[IndexChange]) -> Result<Option<Vec<u8>>> {
        if changes.is_empty() {
            return Ok(None);
        }
        let git = &self.git;
        let mut current: BTreeMap<String, Vec<(u8, Entry)>> = BTreeMap::new();
        for chunk in changes.chunks(400) {
            let mut args = vec!["ls-files", "-s", "-z", "--"];
            args.extend(chunk.iter().map(|change| change.path.as_str()));
            let output = git.run(&args)?;
            for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
                let tab = record
                    .iter()
                    .position(|byte| *byte == b'\t')
                    .context("malformed ls-files record")?;
                let meta = text(&record[..tab])?;
                let path = String::from_utf8(record[tab + 1..].to_vec())?;
                let mut fields = meta.split(' ');
                let mode = fields.next().context("ls-files mode")?.to_owned();
                let oid = fields.next().context("ls-files oid")?.to_owned();
                let stage: u8 = fields.next().context("ls-files stage")?.parse()?;
                current.entry(path).or_default().push((stage, (mode, oid)));
            }
        }
        let zero = git.zero_oid(&self.head.oid);
        let mut input = Vec::new();
        for change in changes {
            let entries = current.get(&change.path).map_or(&[][..], Vec::as_slice);
            if entries.iter().any(|(stage, _)| *stage != 0) {
                return Err(fail(
                    Code::Dirty,
                    format!("{}: unresolved conflict in the index", change.path),
                ));
            }
            let present = entries.first().map(|(_, entry)| entry);
            if present != change.from.as_ref() {
                continue; // staged differently by the user: preserved
            }
            match &change.to {
                Some((mode, oid)) => {
                    input.extend_from_slice(format!("{mode} {oid}\t{}", change.path).as_bytes());
                }
                None => input.extend_from_slice(format!("0 {zero}\t{}", change.path).as_bytes()),
            }
            input.push(0);
        }
        if input.is_empty() {
            return Ok(None);
        }
        let directory = git.dir.join("aisc").join("tmp");
        fs::create_dir_all(&directory)?;
        let alternate = directory.join(format!("index-new-{}", std::process::id()));
        let _ = fs::remove_file(&alternate);
        let result = (|| -> Result<Vec<u8>> {
            let real = git.dir.join("index");
            if real.exists() {
                fs::copy(&real, &alternate)?;
            }
            let env = [("GIT_INDEX_FILE", alternate.as_os_str())];
            git.run_with(&["update-index", "-z", "--index-info"], &env, Some(input))?;
            Ok(fs::read(&alternate)?)
        })();
        let _ = fs::remove_file(&alternate);
        result.map(Some)
    }

    /// Executes the plan. Nothing changes unless every precondition holds up to the ref transaction.
    pub fn apply(mut self, plan: &Plan) -> Result<Applied> {
        self.ensure_unchanged()?;
        let git = self.git.clone();
        let index_bytes = self.prepare_index(&plan.index)?;
        let record = OpRecord::from_plan(plan);
        let (op_oid, previous) = oplog::build(&git, &record)?;
        self.ensure_unchanged()?;
        let op = oplog::short(&op_oid);
        let _critical = signals::Critical::enter();
        // Reference transaction: the point of no return.
        let mut transaction = RefTx::default();
        for movement in &plan.refs {
            match (&movement.old, &movement.new) {
                (Some(old), Some(new)) if movement.name == "HEAD" => {
                    transaction.update_no_deref(&movement.name, new, old);
                }
                (Some(old), Some(new)) => transaction.update(&movement.name, new, old),
                (None, Some(new)) => transaction.create(&movement.name, new),
                (Some(old), None) => transaction.delete(&movement.name, old),
                (None, None) => {}
            }
        }
        let log_ref = oplog::log_ref(&git)?;
        match &previous {
            Some(previous) => transaction.update(&log_ref, &op_oid, previous),
            None => transaction.create(&log_ref, &op_oid),
        }
        transaction.commit(&git, &format!("aisc {}", plan.verb))?;
        let partial = |what: &str, error: &dyn std::fmt::Display| {
            fail_hint(
                Code::State,
                format!("partially applied: {what}: {error}"),
                format!("references were updated; run aisc undo {op} to revert"),
            )
        };
        if let Some(head) = &plan.head {
            // HEAD may dangle here (its branch was just deleted), so read only the symref.
            let current = git
                .probe(&["symbolic-ref", "-q", "HEAD"])
                .map_err(|e| partial("HEAD unreadable", &e))?;
            if current != head.from {
                return Err(partial("HEAD moved", &"concurrent change"));
            }
            git.run(&["symbolic-ref", "HEAD", &head.to])
                .map_err(|e| partial("could not switch HEAD", &e))?;
        }
        for write in &plan.worktree {
            write_file(&git.root, write).map_err(|e| partial(&write.path, &e))?;
        }
        if let Some(bytes) = index_bytes {
            self.install_index(&bytes)
                .map_err(|e| partial("could not install the index", &e))?;
        }
        Ok(Applied { op, op_oid })
    }

    fn install_index(&mut self, bytes: &[u8]) -> Result<()> {
        let lock = self.index_lock.as_mut().context("index lock not held")?;
        if fingerprint(&self.git).0 != self.index_snapshot {
            anyhow::bail!("the index changed concurrently");
        }
        lock.file.set_len(0)?;
        lock.file.write_all(bytes)?;
        lock.file.sync_all()?;
        // Git's racy-stat check depends on the index mtime; keep the original (PR 5 rule).
        if let Some(modified) = self.index_mtime {
            lock.file.set_modified(modified)?;
        }
        lock.keep = true;
        let real = self.git.dir.join("index");
        if let Err(error) = fs::rename(&lock.path, &real) {
            lock.keep = false;
            return Err(error.into());
        }
        Ok(())
    }
}

pub(crate) fn stale(message: &str) -> anyhow::Error {
    fail_hint(
        Code::Stale,
        format!("{message}; nothing was changed"),
        "run aisc log and retry",
    )
}

/// Writes (or removes) a worktree file atomically, never following symlinked parents.
pub(crate) fn write_file(root: &Path, write: &WorktreeWrite) -> Result<()> {
    let target = root.join(&write.path);
    let mut parent = root.to_path_buf();
    let components: Vec<_> = Path::new(&write.path).components().collect();
    for component in &components[..components.len().saturating_sub(1)] {
        parent.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&parent)
            && metadata.file_type().is_symlink()
        {
            anyhow::bail!("{}: parent is a symlink", write.path);
        }
    }
    match &write.after {
        None => {
            if fs::symlink_metadata(&target).is_ok() {
                fs::remove_file(&target)?;
            }
            Ok(())
        }
        Some(bytes) => {
            if let Some(directory) = target.parent() {
                fs::create_dir_all(directory)?;
            }
            let temporary = target.with_file_name(format!(
                ".aisc-write-{}-{}",
                std::process::id(),
                target
                    .file_name()
                    .unwrap_or(OsStr::new("file"))
                    .to_string_lossy()
            ));
            if write.mode == "120000" {
                let _ = fs::remove_file(&temporary);
                std::os::unix::fs::symlink(
                    OsStr::new(&String::from_utf8_lossy(bytes).into_owned()),
                    &temporary,
                )?;
            } else {
                fs::write(&temporary, bytes)?;
                let bits = if write.mode == "100755" { 0o755 } else { 0o644 };
                fs::set_permissions(&temporary, fs::Permissions::from_mode(bits))?;
            }
            fs::rename(&temporary, &target)?;
            Ok(())
        }
    }
}

/// Local branches that point at a rewritten commit move with it (design decision D5).
pub(crate) fn dependent_moves(
    locals: &[super::view::BranchRef],
    rewritten: &[(String, String)],
    skip: &[&str],
) -> Vec<RefMove> {
    locals
        .iter()
        .filter(|branch| !skip.contains(&branch.name.as_str()))
        .filter_map(|branch| {
            rewritten
                .iter()
                .find(|(old, _)| *old == branch.oid)
                .map(|(old, new)| RefMove {
                    name: format!("refs/heads/{}", branch.name),
                    old: Some(old.clone()),
                    new: Some(new.clone()),
                })
        })
        .collect()
}
