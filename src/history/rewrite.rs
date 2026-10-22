//! Rewrite plan executor: one operation lock, compare-and-swap ref transaction, then HEAD,
//! worktree files and the index. Any failure before the ref transaction changes nothing.
#![allow(dead_code)] // worktree writes, dependents and detached heads are consumed by later slices
use super::oplog::{self, OpRecord};
use super::plumbing::{Entry, Favor, Git, HeadState, RefTx};
use super::{Code, fail, fail_hint};
use crate::git::{Lock, Repo, text};
use crate::output::escape_line;
use crate::signals;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
    /// Mode after the write (`100644`, `100755`, `120000`).
    pub mode: String,
    /// Mode before the write; `None` when the file did not exist.
    pub before_mode: Option<String>,
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
    /// Paths whose index entry should become an intent-to-add placeholder (undo of such a commit).
    pub restore_ita: Vec<String>,
    /// Untracked worktree files (relative paths) the plan made obsolete, such as sync's
    /// `PATH.aisc-conflict` side files: deleted once the references moved, even if a later step
    /// fails.
    pub remove_files: Vec<String>,
    /// Files under the Git directory (relative paths) deleted at the same point, such as a paused
    /// sync's state.
    pub remove_state: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct Applied {
    pub op: String,
    pub op_oid: String,
}

pub(crate) struct Session {
    pub git: Git,
    pub head: HeadState,
    _operation: OperationLock,
    index_lock: Option<Lock>,
    index_snapshot: Option<Vec<u8>>,
    index_mtime: Option<SystemTime>,
}

impl Drop for Session {
    /// Journal first, then index.lock and the temporary files, then (field drop) the operation
    /// lock: a kill anywhere in between leaves only files that recovery attributes to this pid.
    fn drop(&mut self) {
        let _ = fs::remove_file(journal_path(&self.git));
        drop(self.index_lock.take());
        sweep_temporary(&self.git, std::process::id());
    }
}

/// Index bytes, the paths replaced from placeholders, and the placeholders to put back.
type Prepared = (Vec<u8>, Vec<String>, Vec<String>);

/// Puts `git add -N` placeholders back on a copy of the new index. A file that is gone has
/// nothing to restore and `-f` covers paths that are ignored now; Git refusing either is
/// expected, any other failure is reported.
fn restore_placeholders(git: &Git, bytes: &[u8], paths: &[String]) -> Result<Vec<u8>> {
    let directory = temporary_dir(git);
    fs::create_dir_all(&directory)?;
    let alternate = directory.join(format!("index-ita-{}", std::process::id()));
    let result = (|| -> Result<Vec<u8>> {
        fs::write(&alternate, bytes)?;
        let env = [("GIT_INDEX_FILE", alternate.as_os_str())];
        for path in paths {
            if fs::symlink_metadata(git.root.join(path)).is_err() {
                continue;
            }
            let args = ["add", "-N", "-f", "--", path.as_str()];
            let output = git.raw(&args, &env, None)?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            let expected =
                stderr.contains("did not match any files") || stderr.contains("paths are ignored");
            if !output.success() && !expected {
                return Err(Git::failure(&args, &output));
            }
        }
        Ok(fs::read(&alternate)?)
    })();
    let _ = fs::remove_file(&alternate);
    result
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
    /// A lock or journal left by a killed aisc process is recovered first: rolled back silently
    /// when nothing had changed, or completed and reported as stale (exit 4) when it had.
    pub fn open(repo: &Repo, git: &Git, touches_index: bool, allow_paused: bool) -> Result<Self> {
        let (operation, dead_owner) = OperationLock::acquire(git)?;
        if let Some(finished) = recover(git, dead_owner)? {
            return Err(finished.into_error());
        }
        if !allow_paused && git.dir.join("aisc").join("sync.json").exists() {
            return Err(fail_hint(
                Code::State,
                "a sync is paused",
                "run aisc sync --continue or aisc sync --abort first",
            ));
        }
        repo.writable()?;
        let head = git.head_state()?;
        let index_lock = if touches_index {
            Some(acquire_index_lock(git)?)
        } else {
            None
        };
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

    /// HEAD's symbolic ref and commit as the plan will leave them.
    fn head_after(&self, plan: &Plan) -> Result<super::oplog::HeadAfter> {
        let symbolic = match &plan.head {
            Some(head) => Some(head.to.clone()),
            None => self.head.symbolic.clone(),
        };
        let name = symbolic.clone().unwrap_or_else(|| "HEAD".to_owned());
        let moved = plan
            .refs
            .iter()
            .find(|movement| movement.name == name)
            .and_then(|movement| movement.new.clone());
        let oid = match (moved, &symbolic) {
            (Some(oid), _) => oid,
            (None, Some(reference)) => self
                .git
                .ref_oid(reference)?
                .unwrap_or_else(|| self.head.oid.clone()),
            (None, None) => self.head.oid.clone(),
        };
        Ok(super::oplog::HeadAfter { symbolic, oid })
    }

    /// Paths among `changes` whose index entry is an intent-to-add placeholder (`git add -N`):
    /// porcelain v2 reports them as index-unchanged (`.`) and worktree-added (`A`).
    fn intent_to_add(&self, changes: &[IndexChange]) -> Result<std::collections::BTreeSet<String>> {
        let mut found = std::collections::BTreeSet::new();
        for chunk in changes.chunks(400) {
            let mut args = vec![
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=no",
                "--no-renames",
                "--",
            ];
            args.extend(chunk.iter().map(|change| change.path.as_str()));
            let output = self.git.run(&args)?;
            let mut records = output.split(|byte| *byte == 0).filter(|r| !r.is_empty());
            while let Some(record) = records.next() {
                let text = String::from_utf8_lossy(record);
                if text.starts_with("2 ") {
                    records.next(); // rename: original path follows
                } else if text.starts_with("1 .A ")
                    && let Some(path) = text.splitn(9, ' ').nth(8)
                {
                    found.insert(path.to_owned());
                }
            }
        }
        Ok(found)
    }

    /// Index bytes with the rule applied: an entry equal to `from` becomes `to`; others are kept.
    fn prepare_index(&self, plan: &Plan) -> Result<Option<Prepared>> {
        let changes = &plan.index;
        if changes.is_empty() {
            return Ok(None);
        }
        let git = &self.git;
        let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
        let (current, intent_to_add) = match git.index_state(&paths) {
            Some(found) => found,
            None => {
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
                (current, self.intent_to_add(changes)?)
            }
        };
        let zero = git.zero_oid(&self.head.oid);
        let mut input = Vec::new();
        let mut applied: Vec<&str> = Vec::new();
        let mut replaced_ita = Vec::new();
        for change in changes {
            let entries = current.get(&change.path).map_or(&[][..], Vec::as_slice);
            if entries.iter().any(|(stage, _)| *stage != 0) {
                return Err(fail(
                    Code::Dirty,
                    format!("{}: unresolved conflict in the index", change.path),
                ));
            }
            // `git add -N` placeholders are not user staging: they count as absent.
            let present = if intent_to_add.contains(&change.path) {
                None
            } else {
                entries.first().map(|(_, entry)| entry)
            };
            if present != change.from.as_ref() {
                continue; // staged differently by the user: preserved
            }
            applied.push(&change.path);
            if intent_to_add.contains(&change.path) {
                replaced_ita.push(change.path.clone());
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
        let restore = plan
            .restore_ita
            .iter()
            .filter(|path| applied.contains(&path.as_str()))
            .cloned()
            .collect();
        result.map(|bytes| Some((bytes, replaced_ita, restore)))
    }

    /// Executes the plan. Nothing changes unless every precondition holds up to the ref transaction.
    pub fn apply(mut self, plan: &Plan) -> Result<Applied> {
        self.ensure_unchanged()?;
        let git = self.git.clone();
        preflight_worktree(&git.root, &plan.worktree)?;
        let prepared = self.prepare_index(plan)?;
        let mut record = OpRecord::from_plan(plan);
        record.data.head_after = Some(self.head_after(plan)?);
        record.data.index_ita = prepared
            .as_ref()
            .map(|(_, ita, _)| ita.clone())
            .unwrap_or_default();
        let (op_oid, previous) = oplog::build(&git, &record)?;
        let log_ref = oplog::log_ref(&git)?;
        self.ensure_unchanged()?;
        let op = oplog::short(&op_oid);
        // From here on a killed process is finished by the next command (see `recover`).
        let obsolete = obsolete_digests(&git, &plan.remove_files);
        self.write_journal(plan, &op_oid, &log_ref, prepared.as_ref(), &obsolete)?;
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
        match &previous {
            Some(previous) => transaction.update(&log_ref, &op_oid, previous),
            None => transaction.create(&log_ref, &op_oid),
        }
        transaction.commit(&git, &format!("aisc {}", plan.verb))?;
        // A side file edited meanwhile is kept: the user's bytes are never discarded.
        remove_obsolete(&git, &plan.remove_files, &obsolete, &plan.remove_state);
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
                .head_symbolic()
                .map_err(|e| partial("HEAD unreadable", &e))?;
            if current != head.from {
                return Err(partial("HEAD moved", &"concurrent change"));
            }
            git.run(&["symbolic-ref", "HEAD", &head.to])
                .map_err(|e| partial("could not switch HEAD", &e))?;
        }
        for write in write_order(&plan.worktree) {
            let side =
                preserve_if_changed(&git.root, write).map_err(|e| partial(&write.path, &e))?;
            if let Some(side) = side {
                eprintln!(
                    "aisc: {} changed while aisc was working; your version is in {}",
                    escape_line(&write.path),
                    escape_line(&side)
                );
            }
            write_file(&git.root, write).map_err(|e| partial(&write.path, &e))?;
        }
        if let Some((mut bytes, _, restore)) = prepared {
            if !restore.is_empty() {
                // Undo of a commit made from `git add -N`: the files exist again only now.
                bytes = restore_placeholders(&git, &bytes, &restore)
                    .map_err(|e| partial("could not restore intent-to-add entries", &e))?;
            }
            self.install_index(&bytes)
                .map_err(|e| partial("could not install the index", &e))?;
        }
        Ok(Applied { op, op_oid })
    }

    /// Records what the next command needs to finish this operation should the process die
    /// after the reference transaction: the op, the prepared index and the cleanup lists.
    fn write_journal(
        &self,
        plan: &Plan,
        op_oid: &str,
        log_ref: &str,
        prepared: Option<&Prepared>,
        obsolete: &[Option<String>],
    ) -> Result<()> {
        let pid = std::process::id();
        let index = match prepared {
            Some((bytes, _, _)) => {
                write_new(&prepared_path(&self.git, pid), bytes)?;
                Some(JournalIndex {
                    snapshot: digest(self.index_snapshot.as_deref()),
                    mtime: self.index_mtime.and_then(|time| {
                        let since = time.duration_since(std::time::UNIX_EPOCH).ok()?;
                        Some((since.as_secs(), since.subsec_nanos()))
                    }),
                })
            }
            None => None,
        };
        let record = Journal {
            v: 1,
            pid,
            phase: "prepared".to_owned(),
            op: op_oid.to_owned(),
            log_ref: log_ref.to_owned(),
            verb: plan.verb.clone(),
            index,
            restore_ita: prepared
                .map(|(_, _, restore)| restore.clone())
                .unwrap_or_default(),
            remove_files: plan.remove_files.clone(),
            remove_digests: obsolete.to_vec(),
            remove_state: plan.remove_state.clone(),
        };
        let staged = temporary_dir(&self.git).join(format!("journal-{pid}"));
        write_new(&staged, &serde_json::to_vec(&record)?)?;
        fs::rename(&staged, journal_path(&self.git))?;
        Ok(())
    }

    fn install_index(&mut self, bytes: &[u8]) -> Result<()> {
        let lock = self.index_lock.as_mut().context("index lock not held")?;
        if fingerprint(&self.git).0 != self.index_snapshot {
            anyhow::bail!("the index changed concurrently");
        }
        fill_index_lock(&lock.file, bytes, self.index_mtime)?;
        lock.keep = true;
        let real = self.git.dir.join("index");
        if let Err(error) = fs::rename(&lock.path, &real) {
            lock.keep = false;
            return Err(error.into());
        }
        Ok(())
    }
}

/// Writes the new index into the held `index.lock`, keeping the original mtime: Git's racy-stat
/// check depends on it (PR 5 rule).
fn fill_index_lock(file: &fs::File, bytes: &[u8], mtime: Option<SystemTime>) -> Result<()> {
    file.set_len(0)?;
    (&*file).write_all(bytes)?;
    file.sync_all()?;
    if let Some(modified) = mtime {
        file.set_modified(modified)?;
    }
    Ok(())
}

/// Creates (or replaces) a private temporary file with `bytes`.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn is_relative(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// `digest` of each obsolete worktree file as the plan saw it.
fn obsolete_digests(git: &Git, files: &[String]) -> Vec<Option<String>> {
    files
        .iter()
        .map(|path| digest(fs::read(git.root.join(path)).ok().as_deref()))
        .collect()
}

/// Deletes the files a plan made obsolete (they hold nothing history lacks). A worktree file is
/// deleted only while it still holds what the plan saw; the others are kept and returned.
fn remove_obsolete(
    git: &Git,
    files: &[String],
    digests: &[Option<String>],
    state: &[String],
) -> Vec<String> {
    let mut kept = Vec::new();
    for (position, path) in files
        .iter()
        .enumerate()
        .filter(|(_, path)| is_relative(path))
    {
        let full = git.root.join(path);
        let current = digest(fs::read(&full).ok().as_deref());
        if current.is_none() {
            continue; // already gone
        }
        if digests.get(position) == Some(&current) {
            let _ = fs::remove_file(full);
        } else {
            kept.push(path.clone());
        }
    }
    for path in state.iter().filter(|path| is_relative(path)) {
        let _ = fs::remove_file(git.dir.join(path));
    }
    kept
}

pub(crate) fn stale(message: &str) -> anyhow::Error {
    fail_hint(
        Code::Stale,
        format!("{message}; nothing was changed"),
        "run aisc log and retry",
    )
}

/// The order writes run in: deletions first, so a file can replace a directory it empties and a
/// directory can replace a deleted file (and a case-only rename frees the old name first).
pub(crate) fn write_order(writes: &[WorktreeWrite]) -> Vec<&WorktreeWrite> {
    let mut ordered: Vec<&WorktreeWrite> = writes.iter().collect();
    ordered.sort_by_key(|write| write.after.is_some());
    ordered
}

fn refuse_symlinked_parents(root: &Path, path: &str) -> Result<()> {
    let mut parent = root.to_path_buf();
    let components: Vec<_> = Path::new(path).components().collect();
    for component in &components[..components.len().saturating_sub(1)] {
        parent.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&parent)
            && metadata.file_type().is_symlink()
        {
            anyhow::bail!("{path}: parent is a symlink");
        }
    }
    Ok(())
}

/// Compare-before-write: a file that no longer holds the bytes the plan was built from (edited
/// while aisc worked) is moved aside to `PATH.aisc-conflict` (or `PATH.aisc-conflict-N`) before
/// the write, so a write or delete never destroys unsaved bytes. Returns the side file.
fn preserve_if_changed(root: &Path, write: &WorktreeWrite) -> Result<Option<String>> {
    refuse_symlinked_parents(root, &write.path)?;
    let Ok(work) = read_work(root, &write.path) else {
        return Ok(None); // a directory: `write_file` replaces only an emptied one
    };
    // Absent, as planned, or already the target (a resolved sync file): nothing to lose.
    let bytes = work.bytes();
    if work.is_absent() || bytes == write.before || bytes == write.after {
        return Ok(None);
    }
    let mut side = format!("{}.aisc-conflict", write.path);
    let mut counter = 0;
    while super::plumbing::path_exists(&root.join(&side)) {
        counter += 1;
        side = format!("{}.aisc-conflict-{counter}", write.path);
    }
    fs::rename(root.join(&write.path), root.join(&side))?;
    Ok(Some(side))
}

/// Removes `directory` when it holds nothing but (nested) empty directories.
fn remove_empty_tree(directory: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_empty_tree(&entry.path())?;
        }
    }
    fs::remove_dir(directory)
}

/// Writes (or removes) a worktree file atomically, never following symlinked parents. A removal
/// keeps its directories (they may predate the operation, and undo must be exact); a file that
/// replaces a directory removes it once the plan's deletions have emptied it.
pub(crate) fn write_file(root: &Path, write: &WorktreeWrite) -> Result<()> {
    let target = root.join(&write.path);
    refuse_symlinked_parents(root, &write.path)?;
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
            if fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.is_dir()) {
                remove_empty_tree(&target)
                    .with_context(|| format!("{}: a directory is in the way", write.path))?;
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

fn dirty(path: &str, why: &str) -> anyhow::Error {
    fail_hint(
        Code::Dirty,
        format!("{path}: {why}"),
        "move or remove the conflicting file or directory, then retry",
    )
}

/// What the earlier writes of a plan leave on disk, in execution order (relative paths).
#[derive(Default)]
struct Planned {
    removed: BTreeSet<PathBuf>,
    files: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
}

impl Planned {
    fn record(&mut self, write: &WorktreeWrite) {
        let path = PathBuf::from(&write.path);
        if write.after.is_none() {
            self.files.remove(&path);
            self.removed.insert(path);
            return;
        }
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|directory| !directory.as_os_str().is_empty()) {
            self.directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
        self.removed.remove(&path);
        self.files.insert(path);
    }

    /// Whether earlier removals leave `directory` empty, so that `write_file` can replace it.
    fn empties(&self, root: &Path, directory: &Path) -> bool {
        let Ok(entries) = fs::read_dir(root.join(directory)) else {
            return false;
        };
        let mut any = false;
        for entry in entries {
            let Ok(entry) = entry else { return false };
            let relative = directory.join(entry.file_name());
            let emptied = match entry.file_type() {
                Ok(kind) if kind.is_dir() => self.empties(root, &relative),
                Ok(_) => self.removed.contains(&relative),
                Err(_) => false,
            };
            if !emptied {
                return false;
            }
            any = true;
        }
        any // a directory that is empty already was never ours to prune
    }
}

/// Validates every planned worktree write or delete before any reference moves, so a write that
/// cannot succeed refuses the whole operation with nothing changed (code `dirty`). Writes are
/// checked in `write_order`, against the disk as the plan's earlier writes leave it.
pub(crate) fn preflight_worktree(root: &Path, writes: &[WorktreeWrite]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut planned = Planned::default();
    for write in write_order(writes) {
        let components: Vec<_> = Path::new(&write.path).components().collect();
        let mut relative = PathBuf::new();
        let mut missing_from = None;
        for (position, component) in components.iter().enumerate() {
            relative.push(component);
            let last = position + 1 == components.len();
            if !last && planned.files.contains(&relative) {
                return Err(dirty(
                    &write.path,
                    "the operation writes a file where a directory is needed",
                ));
            }
            if planned.directories.contains(&relative) {
                if last {
                    return Err(dirty(&write.path, "a directory is where a file is needed"));
                }
                continue; // created by an earlier write
            }
            if last && planned.files.contains(&relative) {
                break;
            }
            let gone = planned.removed.contains(&relative);
            match fs::symlink_metadata(root.join(&relative)) {
                Ok(_) if gone => {
                    missing_from.get_or_insert(position);
                    break;
                }
                Ok(metadata) if !last => {
                    if metadata.file_type().is_symlink() {
                        return Err(dirty(&write.path, "a parent directory is a symlink"));
                    }
                    if !metadata.is_dir() {
                        return Err(dirty(
                            &write.path,
                            "a file (untracked or ignored) is where a directory is needed",
                        ));
                    }
                }
                Ok(metadata) => {
                    if metadata.is_dir() {
                        if write.after.is_none() || !planned.empties(root, &relative) {
                            return Err(dirty(
                                &write.path,
                                "a directory is where a file is needed",
                            ));
                        }
                        missing_from.get_or_insert(position);
                    } else if write.after.is_some() && write.before.is_none() {
                        return Err(dirty(
                            &write.path,
                            "an untracked or ignored file is in the way",
                        ));
                    }
                }
                // ENOTDIR: a parent is a file an earlier write deletes.
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ENOTDIR) =>
                {
                    missing_from.get_or_insert(position);
                    break;
                }
                Err(error) => return Err(dirty(&write.path, &error.to_string())),
            }
        }
        planned.record(write);
        if write.after.is_none() {
            continue;
        }
        // The nearest directory that exists when the file is written must allow new entries.
        let mut existing = match missing_from {
            Some(position) => components[..position]
                .iter()
                .fold(root.to_path_buf(), |path, component| path.join(component)),
            None => root
                .join(&write.path)
                .parent()
                .unwrap_or(root)
                .to_path_buf(),
        };
        while existing != root && !existing.is_dir() {
            existing.pop();
        }
        let c_path = std::ffi::CString::new(existing.as_os_str().as_bytes())
            .map_err(|_| dirty(&write.path, "unusable path"))?;
        // SAFETY: access() only reads the NUL-terminated path.
        if unsafe { libc::access(c_path.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
            return Err(dirty(&write.path, "the parent directory is not writable"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Operation lock, crash journal and recovery
// ---------------------------------------------------------------------------------------------
//
// A rewrite can be killed (SIGKILL, power button, OOM) at any step. Every file it leaves behind
// is attributable to its pid, and `$GIT_DIR/aisc/journal.json`, written just before the
// reference transaction, says how to finish. The next `Session::open` recovers:
// - the operation lock carries `aisc pid N` and an exclusive flock that dies with its owner, so a
//   live owner (flock held) or a foreign lock (no tag) is always refused;
// - `index.lock` is a hard link of `aisc/tmp/index-token-PID`, so it is removed only when it is
//   provably the dead process's (same inode), never a lock Git or anyone else took;
// - with no journal, or one whose op never reached the oplog ref, nothing had changed: the locks
//   and temporary files go and the command proceeds;
// - when the oplog ref holds the journal's op, HEAD, the worktree and the prepared index are
//   brought forward, and the command stops with exit 4 so the caller re-reads the new state.

const LOCK_TAG: &str = "aisc pid ";
const GROUP_TAG: &str = "pgid ";

fn temporary_dir(git: &Git) -> PathBuf {
    git.dir.join("aisc").join("tmp")
}

fn journal_path(git: &Git) -> PathBuf {
    git.dir.join("aisc").join("journal.json")
}

fn token_path(git: &Git, pid: u32) -> PathBuf {
    temporary_dir(git).join(format!("index-token-{pid}"))
}

fn prepared_path(git: &Git, pid: u32) -> PathBuf {
    temporary_dir(git).join(format!("index-prepared-{pid}"))
}

/// Removes the temporary files process `pid` created under `aisc/tmp` (and Git's locks on them).
fn sweep_temporary(git: &Git, pid: u32) {
    let Ok(entries) = fs::read_dir(temporary_dir(git)) else {
        return;
    };
    let owned = [
        "index-ita-",
        "index-new-",
        "index-token-",
        "index-prepared-",
        "journal-",
        "oplock-",
    ]
    .map(|prefix| format!("{prefix}{pid}"));
    let tree_index = format!("index-{pid}-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let name = name.strip_suffix(".lock").unwrap_or(&name);
        if owned.iter().any(|owned| owned == name) || name.starts_with(&tree_index) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn locked(name: &str, why: &str) -> anyhow::Error {
    fail(Code::State, format!("checkout is locked: {name}; {why}"))
}

fn same_file(first: &Path, second: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(first), fs::symlink_metadata(second)) {
        (Ok(first), Ok(second)) => first.dev() == second.dev() && first.ino() == second.ino(),
        _ => false,
    }
}

/// `flock(LOCK_EX | LOCK_NB)`: `false` when another open file description holds it.
fn try_flock(file: &fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock only acts on the descriptor, which `file` keeps open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Links `staged` to `path` (fails if `path` exists), falling back to an exclusive create on
/// filesystems without hard links. `Ok(None)` means `path` already exists.
fn link_into_place(staged: &Path, path: &Path) -> Result<Option<Option<fs::File>>> {
    match fs::hard_link(staged, path) {
        Ok(()) => Ok(Some(None)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(_) => match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => Ok(Some(Some(file))),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(error.into()),
        },
    }
}

/// `aisc-operation.lock`, holding `aisc pid N` and an exclusive flock for as long as its owner
/// lives. The kernel drops the flock when the owner dies, so a stale lock is told apart from a
/// live one without trusting the pid, which may have been reused.
pub(crate) struct OperationLock {
    path: PathBuf,
    _file: fs::File,
}

impl OperationLock {
    /// Records (or, with `None`, clears) the process group of a command this lock's owner runs,
    /// with its leader's start time, so the lock still counts as held while that leader lives
    /// after the owner died, and a reused pgid is never mistaken for it.
    pub(crate) fn record_group(&self, group: Option<u32>) -> Result<()> {
        use std::os::unix::fs::FileExt;
        let mut tag = format!("{LOCK_TAG}{}\n", std::process::id());
        if let Some(group) = group
            && let Some((start, _)) = crate::process::process_identity(group)
        {
            tag.push_str(&format!("{GROUP_TAG}{group} start {start}\n"));
        }
        // Grow before shrinking, so a concurrent reader never sees an empty file.
        self._file.write_all_at(tag.as_bytes(), 0)?;
        self._file.set_len(tag.len() as u64)?;
        Ok(())
    }
}

/// A verified command's process group, as recorded in the lock: pgid and its leader's start.
#[derive(Clone, Copy)]
struct Group {
    id: u32,
    start: u64,
}

/// The owner pid and, while a verified command runs, its process group.
fn parse_tag(content: &str) -> Option<(u32, Option<Group>)> {
    let mut lines = content.lines();
    let owner = lines.next()?.strip_prefix(LOCK_TAG)?.trim().parse().ok()?;
    let group = lines.next().and_then(|line| {
        let (id, start) = line.strip_prefix(GROUP_TAG)?.split_once(" start ")?;
        Some(Group {
            id: id.trim().parse().ok()?,
            start: start.trim().parse().ok()?,
        })
    });
    Some((owner, group))
}

/// Whether the recorded group is still the verified command's: its leader (pid = pgid) lives,
/// started at the recorded time, and still leads that group. A reused pgid has another start
/// time. pgid 0 and 1, and this process's own group or session, can never be a verified
/// command. Members left after the leader died are not tracked: verify waits for the leader,
/// so once it is gone the command has finished as far as the lock is concerned.
fn group_alive(group: Group) -> bool {
    let Ok(id) = libc::pid_t::try_from(group.id) else {
        return false;
    };
    // SAFETY: getpgrp and getsid(0) only read this process's own ids.
    let (own_group, own_session) = unsafe { (libc::getpgrp(), libc::getsid(0)) };
    if id <= 1 || id == own_group || id == own_session {
        return false;
    }
    crate::process::process_identity(group.id)
        .is_some_and(|(start, leads)| start == group.start && leads == group.id)
}

fn group_refusal(owner: u32, group: Group, lock: &Path) -> String {
    format!(
        "process group {} recorded by aisc pid {owner} still has members; if that is not your verify command, remove {}",
        group.id,
        lock.display()
    )
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl OperationLock {
    /// Takes the lock; also returns the pid of a dead aisc owner whose lock it took over.
    fn acquire(git: &Git) -> Result<(Self, Option<u32>)> {
        use std::io::{Read, Seek};
        let path = git.dir.join("aisc-operation.lock");
        let pid = std::process::id();
        let tag = format!("{LOCK_TAG}{pid}\n");
        fs::create_dir_all(temporary_dir(git))?;
        for _ in 0..3 {
            // Complete (tag and flock) under a private name before it appears under the real one.
            let staged = temporary_dir(git).join(format!("oplock-{pid}"));
            let _ = fs::remove_file(&staged);
            let mut file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&staged)?;
            try_flock(&file)?;
            file.write_all(tag.as_bytes())?;
            let placed = link_into_place(&staged, &path);
            let _ = fs::remove_file(&staged);
            match placed? {
                Some(None) => return Ok((Self { path, _file: file }, None)),
                Some(Some(mut direct)) => {
                    try_flock(&direct)?;
                    direct.write_all(tag.as_bytes())?;
                    return Ok((
                        Self {
                            path,
                            _file: direct,
                        },
                        None,
                    ));
                }
                None => {}
            }
            drop(file);
            let mut held = match fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(held) => held,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let mut content = String::new();
            let _ = (&held).take(64).read_to_string(&mut content);
            let parsed = parse_tag(&content);
            let owner = parsed.map(|(owner, _)| owner);
            // An empty lock past the grace period was abandoned before any tag was written
            // (an older task verb killed mid-way, or `: > lock`); the flock still decides.
            let abandoned = owner.is_none()
                && content.is_empty()
                && held
                    .metadata()?
                    .modified()?
                    .elapsed()
                    .is_ok_and(|age| age >= JOURNAL_GRACE);
            if owner.is_none() && !abandoned {
                return Err(locked(
                    "aisc-operation.lock",
                    "inspect owner before removing a stale lock",
                ));
            }
            if !try_flock(&held)? {
                let holder = owner.map_or_else(
                    || "another process holds it".to_owned(),
                    |owner| format!("aisc process {owner} is running"),
                );
                return Err(locked("aisc-operation.lock", &holder));
            }
            if let Some((owner, Some(group))) = parsed
                && group_alive(group)
            {
                return Err(locked(
                    "aisc-operation.lock",
                    &group_refusal(owner, group, &path),
                ));
            }
            // The owner is dead; take over only if this is still the file at `path`.
            let current = held.metadata()?;
            let same = fs::symlink_metadata(&path).is_ok_and(|metadata| {
                use std::os::unix::fs::MetadataExt;
                metadata.dev() == current.dev() && metadata.ino() == current.ino()
            });
            if !same {
                continue;
            }
            held.set_len(0)?;
            held.seek(std::io::SeekFrom::Start(0))?;
            held.write_all(tag.as_bytes())?;
            return Ok((Self { path, _file: held }, owner));
        }
        Err(locked(
            "aisc-operation.lock",
            "another aisc process is starting",
        ))
    }
}

/// Takes `index.lock` as a hard link of this process's token file, so that recovery can prove
/// (same inode) that a stale `index.lock` is one aisc created before removing it.
fn acquire_index_lock(git: &Git) -> Result<Lock> {
    let token = token_path(git, std::process::id());
    let _ = fs::remove_file(&token);
    fs::create_dir_all(temporary_dir(git))?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&token)?;
    let path = git.dir.join("index.lock");
    let placed = link_into_place(&token, &path);
    let file = match placed {
        Ok(Some(None)) => file,
        Ok(Some(Some(direct))) => {
            let _ = fs::remove_file(&token); // no hard links: no proof, never taken over
            direct
        }
        Ok(None) => {
            let _ = fs::remove_file(&token);
            return Err(locked(
                "index.lock",
                "inspect owner before removing a stale lock",
            ));
        }
        Err(error) => {
            let _ = fs::remove_file(&token);
            return Err(error);
        }
    };
    Ok(Lock {
        path,
        file,
        keep: false,
    })
}

#[derive(Serialize, Deserialize)]
struct Journal {
    v: u32,
    pid: u32,
    /// Always `prepared`: written once the op commit exists, before the reference transaction.
    phase: String,
    op: String,
    log_ref: String,
    verb: String,
    index: Option<JournalIndex>,
    restore_ita: Vec<String>,
    remove_files: Vec<String>,
    /// `digest` of each `remove_files` entry as the plan saw it; a file is removed only while it
    /// still matches (journals without it keep every file).
    #[serde(default)]
    remove_digests: Vec<Option<String>>,
    remove_state: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct JournalIndex {
    /// `digest` of the index the new one was prepared from; installed only over that one.
    snapshot: Option<String>,
    /// Original index mtime as (seconds, nanoseconds) since the epoch.
    mtime: Option<(u64, u32)>,
}

/// Length and FNV-1a hash of the index bytes: a change detector, not a security check.
fn digest(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| {
        let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
        format!("{}:{hash:016x}", bytes.len())
    })
}

/// An interrupted operation that recovery completed.
struct Finished {
    op: String,
    verb: String,
    /// What could not be brought forward because it changed after the crash.
    skipped: Vec<String>,
}

impl Finished {
    fn into_error(self) -> anyhow::Error {
        let short = oplog::short(&self.op);
        let mut message = format!(
            "finished an interrupted aisc {} (op {short}) first; this command did not run",
            self.verb
        );
        if !self.skipped.is_empty() {
            let shown: Vec<String> = self.skipped.iter().map(|path| escape_line(path)).collect();
            message.push_str(&format!(
                "; left as found (changed after the interruption): {}",
                shown.join(", ")
            ));
        }
        fail_hint(
            Code::Stale,
            message,
            format!("run aisc log and retry; aisc undo {short} reverts that operation"),
        )
    }
}

/// Cleans up after a killed aisc process (see the section comment). Only files the dead process
/// provably created are removed; an error keeps the journal so the next command retries.
fn recover(git: &Git, dead_owner: Option<u32>) -> Result<Option<Finished>> {
    let path = journal_path(git);
    let journal: Option<Journal> = match fs::read(&path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|error| {
            fail_hint(
                Code::State,
                format!("unreadable operation journal: {error}"),
                "inspect .git/aisc/journal.json, then remove it",
            )
        })?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let Some(pid) = journal.as_ref().map(|journal| journal.pid).or(dead_owner) else {
        return Ok(None);
    };
    if dead_owner.is_none()
        && let Some(journal) = &journal
    {
        // No dead owner's lock vouches for this journal (someone removed the lock by hand):
        // touch nothing unless its owner is surely gone.
        let age = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        let alive = pid_alive(journal.pid);
        if alive || age.is_none_or(|age| age < JOURNAL_GRACE) {
            return Err(fail_hint(
                Code::State,
                format!(
                    "an operation journal of aisc process {} exists without its lock",
                    journal.pid
                ),
                if alive {
                    "that process may still be running: wait for it to finish, then retry"
                } else {
                    "retry in a few seconds"
                },
            ));
        }
    }
    let _critical = signals::Critical::enter();
    let mut finished = None;
    if let Some(journal) = &journal {
        if git.ref_oid(&journal.log_ref)?.as_deref() == Some(journal.op.as_str()) {
            finished = Some(roll_forward(git, journal).map_err(|error| {
                fail_hint(
                    Code::State,
                    format!(
                        "could not finish the interrupted aisc {} (op {}): {error:#}",
                        journal.verb,
                        oplog::short(&journal.op)
                    ),
                    "fix the cause and retry; to give up, remove .git/aisc/journal.json",
                )
            })?);
        } else if super::plumbing::path_exists(
            &git.common_dir()?.join(format!("{}.lock", journal.log_ref)),
        ) {
            return Err(fail_hint(
                Code::State,
                "an interrupted aisc operation may still be updating references",
                format!(
                    "retry in a moment; if no git process is running, remove the {}.lock file",
                    journal.log_ref
                ),
            ));
        }
    }
    let index_lock = git.dir.join("index.lock");
    if same_file(&token_path(git, pid), &index_lock) {
        fs::remove_file(&index_lock)?;
    }
    sweep_temporary(git, pid);
    sweep_temporary(git, std::process::id()); // a token `install_prepared` may have taken
    if journal.is_some() {
        fs::remove_file(&path)?;
    }
    Ok(finished)
}

/// Completes an operation whose reference transaction landed: HEAD, worktree files and the
/// index, each only from the state the operation expected (anything else is reported).
fn roll_forward(git: &Git, journal: &Journal) -> Result<Finished> {
    let op = &journal.op;
    let data = oplog::load_oid(git, op)?;
    let mut skipped = remove_obsolete(
        git,
        &journal.remove_files,
        &journal.remove_digests,
        &journal.remove_state,
    );
    if let Some(head) = &data.head {
        let current = git.probe(&["symbolic-ref", "-q", "HEAD"])?;
        if current == head.from {
            git.run(&["symbolic-ref", "HEAD", &head.to])?;
        } else if current.as_deref() != Some(head.to.as_str()) {
            skipped.push("HEAD".to_owned());
        }
    }
    let writes = oplog::recorded_writes(git, op, &data)?;
    for write in write_order(&writes) {
        let target = git.root.join(&write.path);
        if let Some(name) = target.file_name() {
            let leftover = format!(".aisc-write-{}-{}", journal.pid, name.to_string_lossy());
            let _ = fs::remove_file(target.with_file_name(leftover));
        }
        let work = read_work(&git.root, &write.path).ok();
        let bytes = work.as_ref().map(Work::bytes);
        let mode = work.as_ref().and_then(Work::mode);
        let written = bytes.as_ref() == Some(&write.after);
        if written && (write.after.is_none() || mode.as_deref() == Some(write.mode.as_str())) {
            continue;
        }
        if bytes.as_ref() == Some(&write.before) {
            if write_file(&git.root, write).is_err() {
                skipped.push(write.path.clone());
            }
        } else if !written {
            skipped.push(write.path.clone());
        }
    }
    if let Some(index) = &journal.index
        && !same_file(&token_path(git, journal.pid), &git.dir.join("index"))
        && let Some(reason) = install_prepared(git, journal, index)?
    {
        skipped.push(reason);
    }
    Ok(Finished {
        op: op.clone(),
        verb: journal.verb.clone(),
        skipped,
    })
}

/// Installs the dead process's prepared index (which it had not installed) under an `index.lock`
/// aisc holds: the dead process's own, proved by its token, or a fresh one when the user removed
/// it. The decision rests on the index content: it must still be the snapshot the new index was
/// prepared from. Returns why the index was left alone, if it was.
fn install_prepared(git: &Git, journal: &Journal, index: &JournalIndex) -> Result<Option<String>> {
    let path = git.dir.join("index.lock");
    let mut lock = if same_file(&token_path(git, journal.pid), &path) {
        let file = fs::OpenOptions::new().write(true).open(&path)?;
        Lock {
            path,
            file,
            keep: false,
        }
    } else {
        match acquire_index_lock(git) {
            Ok(lock) => lock,
            Err(_) => {
                return Ok(Some(
                    "the index (index.lock is held by another process)".into(),
                ));
            }
        }
    };
    let real = git.dir.join("index");
    if digest(fs::read(&real).ok().as_deref()) != index.snapshot {
        return Ok(Some("the index".to_owned())); // dropping `lock` releases it
    }
    let mut bytes = fs::read(prepared_path(git, journal.pid))?;
    if !journal.restore_ita.is_empty() {
        bytes = restore_placeholders(git, &bytes, &journal.restore_ita)?;
    }
    let mtime = index
        .mtime
        .map(|(seconds, nanos)| std::time::UNIX_EPOCH + Duration::new(seconds, nanos));
    fill_index_lock(&lock.file, &bytes, mtime)?;
    lock.keep = true;
    if let Err(error) = fs::rename(&lock.path, &real) {
        lock.keep = false;
        return Err(error.into());
    }
    Ok(None)
}

/// Whether process `pid` may be alive (`kill(pid, 0)` does not report ESRCH).
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true; // unrepresentable: assume the worst
    };
    // SAFETY: signal 0 only checks for existence and permission.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// A journal younger than this, found without its operation lock, may belong to a process that
/// is still starting or finishing; it is never touched.
const JOURNAL_GRACE: Duration = Duration::from_secs(5);

/// Recovers whatever a killed aisc process left behind, without running an operation: the
/// entry point for commands that do not open a `Session` (history reads, and task verbs once
/// they adopt it). A live or foreign lock is left alone. Returns a note when an interrupted
/// operation was completed.
/// The operation lock for task verbs (start, checkpoint, verify, finish): the same flock-tagged
/// lock history verbs take, with the same recovery first, so a killed history verb never leaves
/// task verbs refusing. A completed interrupted operation is reported as stale (exit 4).
pub(crate) fn task_lock(git: &Git) -> Result<OperationLock> {
    let (operation, dead_owner) = OperationLock::acquire(git)?;
    if let Some(finished) = recover(git, dead_owner)? {
        return Err(finished.into_error());
    }
    Ok(operation)
}

/// Whether `aisc-operation.lock` in `git.dir` is really held, without taking it: a live flock,
/// a recorded process group that still has members (a verified command outliving a killed
/// owner), a fresh empty lock (possibly still being written) or a lock with foreign content. A
/// dead owner's tagged lock, or an empty one past the grace period, is not; `task_lock` takes it
/// over.
pub(crate) fn operation_lock_held(git: &Git) -> bool {
    use std::io::Read;
    let path = git.dir.join("aisc-operation.lock");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    let mut content = String::new();
    let _ = (&file).take(64).read_to_string(&mut content);
    let parsed = parse_tag(&content);
    let tagged = parsed.is_some();
    if let Some((_, Some(group))) = parsed
        && group_alive(group)
    {
        return true;
    }
    let abandoned = !tagged
        && content.is_empty()
        && file
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= JOURNAL_GRACE);
    if !tagged && !abandoned {
        return true;
    }
    match try_flock(&file) {
        Ok(true) => {
            // A probe only: release at once, never write or take the lock over.
            use std::os::fd::AsRawFd;
            // SAFETY: unlocks the flock this probe just took on its own open descriptor.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            false
        }
        _ => true,
    }
}

pub(crate) fn recover_if_needed(git: &Git) -> Result<Option<String>> {
    let lock = git.dir.join("aisc-operation.lock");
    if !super::plumbing::path_exists(&lock) && !super::plumbing::path_exists(&journal_path(git)) {
        return Ok(None);
    }
    let Ok((operation, dead_owner)) = OperationLock::acquire(git) else {
        return Ok(None); // live or foreign: nothing to recover
    };
    let finished = recover(git, dead_owner)?;
    drop(operation);
    Ok(finished.map(|finished| {
        let mut note = format!(
            "aisc: finished an interrupted aisc {} (op {})",
            finished.verb,
            oplog::short(&finished.op)
        );
        if !finished.skipped.is_empty() {
            let shown: Vec<String> = finished
                .skipped
                .iter()
                .map(|path| escape_line(path))
                .collect();
            note.push_str(&format!("; left as found: {}", shown.join(", ")));
        }
        note
    }))
}

/// `recover_if_needed` for read-only commands: a read never fails because of recovery; what it
/// did, or why it could not, goes to stderr.
pub(crate) fn recover_before_read(git: &Git) {
    match recover_if_needed(git) {
        Ok(None) => {}
        Ok(Some(note)) => eprintln!("{note}"),
        Err(error) => {
            let hint = error
                .downcast_ref::<super::HistoryError>()
                .and_then(|error| error.hint.clone())
                .map(|hint| format!(" ({hint})"))
                .unwrap_or_default();
            eprintln!("aisc: could not recover an interrupted operation: {error:#}{hint}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worktree carry (shared with sync, land and switch)
// ---------------------------------------------------------------------------------------------

pub(crate) struct Carry {
    pub writes: Vec<WorktreeWrite>,
    pub index: Vec<IndexChange>,
}

struct TreeChange {
    path: String,
    from: Option<Entry>,
    to: Option<Entry>,
}

fn tree_changes(git: &Git, from_tree: &str, to_tree: &str) -> Result<Vec<TreeChange>> {
    if from_tree == to_tree {
        return Ok(Vec::new());
    }
    if let Some(changed) = git.tree_changes(from_tree, to_tree) {
        let mut changes = Vec::with_capacity(changed.len());
        for (path, from, to) in changed {
            if [&from, &to]
                .into_iter()
                .flatten()
                .any(|(mode, _)| mode == "160000")
            {
                return Err(fail(
                    Code::Unsupported,
                    format!("{path}: submodules are unsupported"),
                ));
            }
            changes.push(TreeChange { path, from, to });
        }
        return Ok(changes);
    }
    let output = git.run(&[
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--no-abbrev",
        from_tree,
        to_tree,
    ])?;
    let mut fields = output.split(|byte| *byte == 0).filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields.next().context("diff-tree record without a path")?;
        let path = String::from_utf8(path.to_vec())
            .map_err(|_| fail(Code::Unsupported, "non-UTF-8 paths are unsupported"))?;
        let meta = String::from_utf8_lossy(meta).into_owned();
        let mut parts = meta.trim_start_matches(':').split(' ');
        let (old_mode, new_mode, old_oid, new_oid) = (
            parts.next().context("diff-tree mode")?,
            parts.next().context("diff-tree mode")?,
            parts.next().context("diff-tree oid")?,
            parts.next().context("diff-tree oid")?,
        );
        if old_mode == "160000" || new_mode == "160000" {
            return Err(fail(
                Code::Unsupported,
                format!("{path}: submodules are unsupported"),
            ));
        }
        let entry = |mode: &str, oid: &str| {
            (!oid.bytes().all(|byte| byte == b'0')).then(|| (mode.to_owned(), oid.to_owned()))
        };
        changes.push(TreeChange {
            path,
            from: entry(old_mode, old_oid),
            to: entry(new_mode, new_oid),
        });
    }
    Ok(changes)
}

pub(crate) enum Work {
    Absent,
    File { bytes: Vec<u8>, exec: bool },
    Link { target: Vec<u8> },
}

impl Work {
    pub fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Absent => None,
            Self::File { bytes, .. } => Some(bytes.clone()),
            Self::Link { target } => Some(target.clone()),
        }
    }
    pub fn mode(&self) -> Option<String> {
        match self {
            Self::Absent => None,
            Self::File { exec, .. } => Some(if *exec { "100755" } else { "100644" }.to_owned()),
            Self::Link { .. } => Some("120000".to_owned()),
        }
    }
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

pub(crate) fn read_work(root: &std::path::Path, path: &str) -> Result<Work> {
    let full = root.join(path);
    let Ok(metadata) = fs::symlink_metadata(&full) else {
        return Ok(Work::Absent);
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(&full)?;
        return Ok(Work::Link {
            target: target.as_os_str().as_encoded_bytes().to_vec(),
        });
    }
    if metadata.is_file() {
        return Ok(Work::File {
            bytes: fs::read(&full)?,
            exec: metadata.permissions().mode() & 0o111 != 0,
        });
    }
    // A directory (or special file) where a file is expected never matches anything.
    Err(fail(
        Code::Dirty,
        format!("{path}: a directory or special file is in the way"),
    ))
}

pub(crate) fn work_matches(work: &Work, hash: Option<&String>, entry: Option<&Entry>) -> bool {
    match (work, entry) {
        (Work::Absent, None) => true,
        (Work::Absent, Some(_)) | (_, None) => false,
        (Work::Link { .. }, Some((mode, oid))) => mode == "120000" && hash == Some(oid),
        (Work::File { exec, .. }, Some((mode, oid))) => {
            ((mode == "100644" && !exec) || (mode == "100755" && *exec)) && hash == Some(oid)
        }
    }
}

/// Moves the worktree from `from_tree` to `to_tree`: per path, a file equal to the old tree takes
/// the new content, a file already equal to the new tree is kept, anything else is merged
/// three-way. A real conflict (or an untracked collision) refuses with `dirty` before any change.
/// Paths in `skip` get index changes only. Also returns the index rule changes for every path.
pub(crate) fn carry(
    git: &Git,
    from_tree: &str,
    to_tree: &str,
    skip: &BTreeSet<String>,
) -> Result<Carry> {
    let changes = tree_changes(git, from_tree, to_tree)?;
    let index = changes
        .iter()
        .map(|change| IndexChange {
            path: change.path.clone(),
            from: change.from.clone(),
            to: change.to.clone(),
        })
        .collect();
    let active: Vec<&TreeChange> = changes.iter().filter(|c| !skip.contains(&c.path)).collect();
    let mut works = Vec::new();
    for change in &active {
        let path = git.root.join(&change.path);
        let directory = fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        if directory && change.from.is_none() {
            // A directory where the new tree adds a file: fine if this carry empties it, which
            // the plan's preflight decides before anything moves.
            works.push(Work::Absent);
            continue;
        }
        works.push(read_work(&git.root, &change.path)?);
    }
    let present: Vec<String> = active
        .iter()
        .zip(&works)
        .filter(|(_, work)| !work.is_absent())
        .map(|(change, _)| change.path.clone())
        .collect();
    let mut hashes = git.hash_worktree(&present)?.into_iter();
    let mut hashed = Vec::new();
    for work in &works {
        hashed.push(if work.is_absent() {
            None
        } else {
            hashes.next()
        });
    }

    enum Action {
        Write(Option<Entry>),
        Merged(Vec<u8>, String),
        Skip,
    }
    // On a case-insensitive filesystem a case-only rename adds a path that already "exists".
    let ignorecase = crate::runner::config(&git.root)?.is_true("core.ignorecase");
    let removed_lower: BTreeSet<String> = active
        .iter()
        .filter(|change| change.to.is_none())
        .map(|change| change.path.to_lowercase())
        .collect();
    let mut actions = Vec::new();
    let mut conflicts = Vec::new();
    for ((change, work), hash) in active.iter().zip(&works).zip(&hashed) {
        let case_rename = ignorecase
            && change.from.is_none()
            && change.to.is_some()
            && !work.is_absent()
            && removed_lower.contains(&change.path.to_lowercase());
        let action = if case_rename {
            // The old name is deleted first (below), then the new name is written fresh.
            Action::Write(change.to.clone())
        } else if work_matches(work, hash.as_ref(), change.from.as_ref()) {
            Action::Write(change.to.clone())
        } else if work_matches(work, hash.as_ref(), change.to.as_ref()) {
            Action::Skip
        } else if let (Some(from), Some(to), true, Some(hash)) =
            (&change.from, &change.to, work.is_file(), hash)
            && regular(&from.0)
            && regular(&to.0)
        {
            match git.merge_file(
                &from.1,
                hash,
                &to.1,
                ["current", "base", "incoming"],
                Favor::None,
                false,
            ) {
                Ok((bytes, 0)) => Action::Merged(bytes, to.0.clone()),
                _ => {
                    conflicts.push(change.path.clone());
                    Action::Skip
                }
            }
        } else {
            conflicts.push(change.path.clone());
            Action::Skip
        };
        actions.push(action);
    }
    if !conflicts.is_empty() {
        let shown: Vec<String> = conflicts.iter().take(5).map(|p| escape_line(p)).collect();
        let more = conflicts.len().saturating_sub(5);
        return Err(fail_hint(
            Code::Dirty,
            format!(
                "uncommitted changes in {}{} conflict with incoming changes; nothing was changed",
                shown.join(", "),
                if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                }
            ),
            "commit or discard those edits (aisc commit, aisc discard), then retry",
        ));
    }
    let oids: Vec<String> = actions
        .iter()
        .filter_map(|action| match action {
            Action::Write(Some((_, oid))) => Some(oid.clone()),
            _ => None,
        })
        .collect();
    let mut blobs = git.read_blobs(&oids)?.into_iter();
    let mut writes = Vec::new();
    for ((change, work), action) in active.iter().zip(&works).zip(actions) {
        match action {
            Action::Skip => {}
            Action::Write(Some((mode, _))) => writes.push(WorktreeWrite {
                path: change.path.clone(),
                before: work.bytes(),
                before_mode: work.mode(),
                after: blobs.next(),
                mode,
            }),
            Action::Write(None) => writes.push(WorktreeWrite {
                path: change.path.clone(),
                before: work.bytes(),
                before_mode: work.mode(),
                after: None,
                mode: change
                    .from
                    .as_ref()
                    .map_or_else(|| "100644".to_owned(), |e| e.0.clone()),
            }),
            Action::Merged(bytes, mode) => writes.push(WorktreeWrite {
                path: change.path.clone(),
                before: work.bytes(),
                before_mode: work.mode(),
                after: Some(bytes),
                mode,
            }),
        }
    }
    // Deletions first: a case-only rename must remove the old name before writing the new one.
    writes.sort_by_key(|write| write.after.is_some());
    Ok(Carry { writes, index })
}

pub(crate) fn regular(mode: &str) -> bool {
    matches!(mode, "100644" | "100755")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_group_session_and_init_are_never_a_verified_command() {
        // SAFETY: getpgrp and getsid(0) only read this process's own ids.
        let (group, session) = unsafe { (libc::getpgrp(), libc::getsid(0)) };
        for id in [0, 1, group as u32, session as u32] {
            let start = crate::process::process_identity(id).map_or(0, |(start, _)| start);
            assert!(!group_alive(Group { id, start }), "{id}");
        }
        assert!(parse_tag("aisc pid 7\npgid 9\n").unwrap().1.is_none());
        let (owner, group) = parse_tag("aisc pid 7\npgid 9 start 11\n").unwrap();
        assert_eq!((owner, group.unwrap().id, group.unwrap().start), (7, 9, 11));
    }
    use crate::history::HistoryError;

    fn write(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> WorktreeWrite {
        WorktreeWrite {
            path: path.to_owned(),
            before: before.map(<[u8]>::to_vec),
            after: after.map(<[u8]>::to_vec),
            mode: "100644".to_owned(),
            before_mode: before.map(|_| "100644".to_owned()),
        }
    }

    fn refused(root: &Path, writes: &[WorktreeWrite]) -> bool {
        preflight_worktree(root, writes)
            .unwrap_err()
            .downcast_ref::<HistoryError>()
            .is_some_and(|error| error.code == Code::Dirty)
    }

    #[test]
    fn preflight_rejects_collisions_symlinks_and_unwritable_parents() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::write(root.join("a"), "untracked").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        fs::write(root.join("present"), "x").unwrap();
        assert!(refused(&root, &[write("a/b", None, Some(b"x"))]));
        assert!(refused(&root, &[write("dir", None, Some(b"x"))]));
        assert!(refused(&root, &[write("dir", Some(b"x"), None)]));
        assert!(refused(&root, &[write("link/f", None, Some(b"x"))]));
        assert!(refused(&root, &[write("present", None, Some(b"x"))]));
        fs::set_permissions(root.join("real"), fs::Permissions::from_mode(0o500)).unwrap();
        let unwritable = preflight_worktree(&root, &[write("real/new/f", None, Some(b"x"))]);
        // SAFETY: geteuid has no preconditions. Root can write anywhere.
        if unsafe { libc::geteuid() } != 0 {
            assert!(unwritable.is_err());
        }
        fs::set_permissions(root.join("real"), fs::Permissions::from_mode(0o700)).unwrap();
        let fine = [
            write("present", Some(b"x"), Some(b"y")),
            write("present", Some(b"x"), None),
            write("missing", Some(b"x"), None),
            write("new/deep/file", None, Some(b"x")),
        ];
        preflight_worktree(&root, &fine).unwrap();
    }

    #[test]
    fn preflight_accounts_for_earlier_writes_of_the_same_plan() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::write(root.join("a"), "tracked").unwrap();
        fs::create_dir_all(root.join("d/e")).unwrap();
        fs::write(root.join("d/x"), "tracked").unwrap();
        fs::write(root.join("d/e/y"), "tracked").unwrap();
        // file -> dir, in either input order: the deletion runs first.
        let file_to_dir = [
            write("a/b", None, Some(b"x")),
            write("a", Some(b"tracked"), None),
        ];
        preflight_worktree(&root, &file_to_dir).unwrap();
        // dir -> file: the deletions empty `d` (nested directories included).
        let dir_to_file = [
            write("d", None, Some(b"x")),
            write("d/x", Some(b"tracked"), None),
            write("d/e/y", Some(b"tracked"), None),
        ];
        preflight_worktree(&root, &dir_to_file).unwrap();
        // ... but not when something else stays in it, or the plan contradicts itself.
        assert!(refused(&root, &dir_to_file[..2]));
        assert!(refused(
            &root,
            &[write("q", None, Some(b"x")), write("q/r", None, Some(b"x"))]
        ));
        fs::create_dir(root.join("empty")).unwrap();
        assert!(refused(&root, &[write("empty", None, Some(b"x"))]));
    }

    #[test]
    fn deletions_keep_directories_unless_a_file_replaces_them() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::create_dir_all(root.join("d/e")).unwrap();
        fs::write(root.join("d/e/y"), "tracked").unwrap();
        fs::create_dir(root.join("logs")).unwrap();
        fs::write(root.join("logs/x"), "tracked").unwrap();
        let writes = [
            write("d", None, Some(b"file now")),
            write("d/e/y", Some(b"tracked"), None),
            write("logs/x", Some(b"tracked"), None),
        ];
        for each in write_order(&writes) {
            write_file(&root, each).unwrap();
        }
        assert_eq!(fs::read_to_string(root.join("d")).unwrap(), "file now");
        assert!(root.join("logs").is_dir(), "an emptied directory is kept");
    }

    #[test]
    fn index_digest_detects_any_change() {
        assert_eq!(digest(None), None);
        assert_ne!(digest(Some(b"ab")), digest(Some(b"ba")));
        assert_eq!(digest(Some(b"")), Some("0:cbf29ce484222325".to_owned()));
    }

    #[test]
    fn failed_preflight_leaves_refs_oplog_and_files_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@example.invalid"]);
        fs::write(root.join("x.txt"), "x\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        fs::write(root.join("a"), "untracked\n").unwrap();
        let repo = Repo {
            root: root.clone(),
            directory: root.join(".git"),
        };
        let git = Git::from_repo(&repo);
        let head = git.head_state().unwrap();
        let tree = git.commit_tree_of(&head.oid).unwrap();
        let next = git
            .commit_tree(&tree, std::slice::from_ref(&head.oid), b"next", None)
            .unwrap();
        let before_refs = run(&["for-each-ref"]);
        let session = Session::open(&repo, &git, true, false).unwrap();
        let plan = Plan {
            verb: "test".to_owned(),
            summary: "collide".to_owned(),
            refs: vec![RefMove {
                name: "refs/heads/main".to_owned(),
                old: Some(head.oid.clone()),
                new: Some(next),
            }],
            worktree: vec![write("a/b", None, Some(b"new"))],
            ..Plan::default()
        };
        let error = session.apply(&plan).unwrap_err();
        let error = error.downcast_ref::<HistoryError>().unwrap();
        assert_eq!(error.code, Code::Dirty);
        assert_eq!(
            before_refs,
            run(&["for-each-ref"]),
            "no ref or oplog change"
        );
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "untracked\n");
        assert!(!root.join(".git/index.lock").exists());
        assert!(!root.join(".git/aisc-operation.lock").exists());
    }
}
