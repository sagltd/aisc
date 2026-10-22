use crate::git::{Lock, Repo, os_bytes, run_git, text};
use crate::{process, worktree};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub name: String,
    pub base: String,
    pub mode: String,
    pub finished: bool,
}

#[derive(Serialize, Deserialize)]
struct Evidence {
    head: String,
    fingerprint: String,
    command: Vec<String>,
    executable: String,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

/// Bytes of command output returned to the caller; the middle is dropped.
const OUTPUT_KEEP: usize = 16_384;
const OUTPUT_HEAD: usize = 4_096;
/// Larger capture so the tail of long output is still available to slice.
const CAPTURE_LIMIT: usize = 2 * 1_024 * 1_024;
/// Variables that describe the calling shell, not the verified candidate.
const VOLATILE_ENVIRONMENT: &[&str] = &["CODEX_TOOL_CALL_ID", "PWD", "OLDPWD", "SHLVL", "_"];

fn head_and_tail(bytes: &[u8], omitted: usize) -> (String, usize) {
    if bytes.len() <= OUTPUT_KEEP {
        return (String::from_utf8_lossy(bytes).into_owned(), omitted);
    }
    let dropped = omitted + bytes.len() - OUTPUT_KEEP;
    let marker = if omitted > 0 {
        format!(
            "\n… {dropped} bytes omitted (only the first {CAPTURE_LIMIT} bytes were captured; the true tail is unavailable) …\n"
        )
    } else {
        format!("\n… {dropped} bytes omitted …\n")
    };
    let tail = OUTPUT_KEEP.saturating_sub(OUTPUT_HEAD + marker.len());
    let mut result = String::from_utf8_lossy(&bytes[..OUTPUT_HEAD]).into_owned();
    result.push_str(&marker);
    result.push_str(&String::from_utf8_lossy(&bytes[bytes.len() - tail..]));
    (
        result,
        dropped + OUTPUT_KEEP.saturating_sub(OUTPUT_HEAD + tail),
    )
}

fn environment_digests() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter(|(name, _)| !VOLATILE_ENVIRONMENT.iter().any(|skip| name == skip))
        .map(|(name, value)| {
            let mut hash = Sha256::new();
            feed(&mut hash, os_bytes(&value));
            (
                name.to_string_lossy().into_owned(),
                format!("{:x}", hash.finalize()),
            )
        })
        .collect()
}

pub(crate) fn start(
    repo: &Repo,
    name: &str,
    destination: Option<PathBuf>,
    sparse: &[String],
) -> Result<Value> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        bail!(
            "task name must be 1–80 ASCII letters, digits, hyphens or underscores, starting with a letter or digit"
        );
    }
    let _guard = Lock::acquire(repo.directory.join("aisc-operation.lock"))?;
    repo.writable()?;
    let base = repo.head()?;
    let mut session = Session {
        name: name.to_owned(),
        base: base.clone(),
        mode: "in-place".to_owned(),
        finished: false,
    };
    match destination {
        None => {
            repo.require_clean()?;
            let state = repo.directory.join("aisc-session.json");
            if state.exists() && !read_json::<Session>(&state)?.finished {
                bail!("an active AISC task already exists in this checkout; finish it first");
            }
            clear_evidence(repo)?;
            write_json(&state, &session)?;
            Ok(
                json!({"task": name, "mode": "in-place", "branch": repo.branch()?, "head": base, "path": repo.root}),
            )
        }
        Some(path) => {
            if path.as_os_str().is_empty() || path.exists() || fs::symlink_metadata(&path).is_ok() {
                bail!("worktree destination must not exist");
            }
            let parent = path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent =
                fs::canonicalize(parent).context("worktree destination parent must exist")?;
            let path = parent.join(path.file_name().context("invalid worktree destination")?);
            let path_text = path
                .to_str()
                .context("non-UTF-8 destination is unsupported")?;
            let sparse = sparse
                .iter()
                .map(|directory| sparse_directory(directory))
                .collect::<Result<Vec<_>>>()?;
            let common = repo.common_dir()?;
            if worktree::registry_path(&common, name).exists() {
                bail!(
                    "an AISC worktree named {name} is already registered; finish it, run `aisc gc`, or pick another name"
                );
            }
            let branch = format!("aisc/{name}");
            let mut add = vec!["worktree", "add"];
            if !sparse.is_empty() {
                add.push("--no-checkout");
            }
            add.extend(["-b", &branch, "--", path_text, &base]);
            repo.git(&add)?;
            session.mode = "worktree".to_owned();
            let registration = worktree::Registration {
                name: name.to_owned(),
                path: path_text.to_owned(),
                branch: branch.clone(),
                base: base.clone(),
                base_branch: repo.branch()?,
                created_at: worktree::now(),
                aisc_version: env!("CARGO_PKG_VERSION").to_owned(),
                command: std::env::args().collect(),
                sparse: (!sparse.is_empty()).then(|| sparse.clone()),
            };
            let prepared = (|| -> Result<()> {
                let directory = PathBuf::from(
                    text(&git_at(&path, &["rev-parse", "--absolute-git-dir"])?)?
                        .trim_end_matches('\n'),
                );
                write_json(&directory.join("aisc-session.json"), &session)?;
                worktree::register(&common, &registration)?;
                if !sparse.is_empty() {
                    let mut set = vec!["sparse-checkout", "set", "--cone", "--no-sparse-index"];
                    set.extend(sparse.iter().map(String::as_str));
                    run_git(&path, &set, None)?;
                    run_git(&path, &["checkout"], None)?;
                }
                Ok(())
            })();
            if let Err(error) = prepared {
                // Nothing in the new worktree is the user's yet, so forcing its removal is safe.
                let mut cleanup = Vec::new();
                for args in [
                    &["worktree", "remove", "--force", "--", path_text][..],
                    &["branch", "-D", "--", &branch][..],
                ] {
                    if let Err(failure) = repo.git(args) {
                        cleanup.push(format!("{failure:#}"));
                    }
                }
                if let Err(failure) = worktree::unregister(&common, name) {
                    cleanup.push(format!("{failure:#}"));
                }
                let outcome = if cleanup.is_empty() {
                    "the new worktree and branch were removed again".to_owned()
                } else {
                    format!("cleanup was incomplete: {}", cleanup.join("; "))
                };
                return Err(error.context(format!("could not prepare the worktree; {outcome}")));
            }
            Ok(
                json!({"task": name, "mode": "worktree", "branch": branch, "head": base, "path": path, "sparse": registration.sparse}),
            )
        }
    }
}

pub(crate) fn checkpoint(repo: &Repo, message: &str, paths: &[String]) -> Result<Value> {
    if message.trim().is_empty() || message.len() > 4_096 || message.contains('\0') {
        bail!("commit message must contain 1–4096 bytes without NUL");
    }
    let _guard = Lock::acquire(repo.directory.join("aisc-operation.lock"))?;
    clear_evidence(repo)?;
    repo.writable()?;
    repo.paths(paths)?;
    let head = repo.head()?;
    let branch = repo.branch()?;
    let index = repo.directory.join("index");
    if fs::symlink_metadata(&index)?.file_type().is_symlink() {
        bail!("symlink Git indexes are unsupported");
    }
    let mut index_lock = Lock::acquire(repo.directory.join("index.lock"))?;
    let original = fs::read(&index)?;
    let original_modified = fs::metadata(&index)?.modified()?;
    let alternate = Lock::acquire(repo.directory.join("aisc-candidate-index"))?;
    // Git's own lock beside our alternate index must not outlive any exit path.
    let _candidate_lock = RemoveOnDrop(candidate_lock_path(&alternate.path));
    fs::write(&alternate.path, &original)?;
    // Git's racy-stat check depends on index mtime; copying must preserve it.
    alternate.file.set_modified(original_modified)?;
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    repo.git_index(&add, &alternate.path)?;
    if repo.head()? != head || repo.branch()? != branch || fs::read(&index)? != original {
        bail!("repository changed during staging; no commit attempted");
    }
    let mut commit = vec!["commit", "--only", "-m", message, "--"];
    commit.extend(paths.iter().map(String::as_str));
    let stray_before = next_index_locks(&repo.directory);
    let outcome = repo.git_index(&commit, &alternate.path);
    // Whatever Git reported, HEAD is the truth about whether the commit landed.
    let committed = match repo.head() {
        Ok(committed) => committed,
        Err(error) => bail!(
            "could not determine whether the checkpoint commit landed ({error}); original index retained; inspect `git log -1` and `git status`{}",
            outcome
                .err()
                .map_or_else(String::new, |cause| format!(" (commit reported: {cause})"))
        ),
    };
    if outcome.is_err() {
        remove_dead_locks(repo, &stray_before);
    }
    if committed == head {
        return match outcome {
            Err(error) => Err(error).context("checkpoint failed; original index retained (configured hooks may have their own side effects)"),
            Ok(_) => bail!("Git reported success without advancing HEAD; original index retained"),
        };
    }
    let warning = outcome.err().map(|error| {
        format!(
            "Git reported a failure after the commit landed (hook or timeout); the commit exists and the index was updated: {error}"
        )
    });
    let recovery = format!(
        "git reset -- {}",
        paths
            .iter()
            .map(|path| quote(path))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let after_commit = |cause: &dyn std::fmt::Display| {
        anyhow::anyhow!(
            "committed {committed} but could not update index: {cause}; run `{recovery}`"
        )
    };
    let branch_now = repo.branch().map_err(|error| after_commit(&error))?;
    let index_now = fs::read(&index).map_err(|error| after_commit(&error))?;
    if branch_now != branch || index_now != original {
        bail!(
            "concurrent repository change after commit {committed}; original index retained; inspect Git state before proceeding"
        );
    }
    let new_index = match fs::read(&alternate.path) {
        Ok(bytes) => bytes,
        Err(_) => rebuild_index(repo, &original, original_modified, &alternate.path, paths)
            .map_err(|error| after_commit(&error))?,
    };
    let candidate_modified = fs::metadata(&alternate.path)
        .and_then(|data| data.modified())
        .ok();
    {
        use std::os::unix::fs::PermissionsExt;
        // Keep a shared repository's group bits and Git's timestamp for racy-entry detection.
        let mode = fs::metadata(&index)
            .map_err(|error| after_commit(&error))?
            .permissions()
            .mode();
        index_lock
            .file
            .write_all(&new_index)
            .map_err(|error| after_commit(&error))?;
        index_lock
            .file
            .set_permissions(fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|error| after_commit(&error))?;
        if let Some(modified) = candidate_modified {
            index_lock
                .file
                .set_modified(modified)
                .map_err(|error| after_commit(&error))?;
        }
        index_lock
            .file
            .sync_all()
            .map_err(|error| after_commit(&error))?;
    }
    // Match Git's index transaction: the ref advances before atomic index installation.
    if let Err(error) = fs::rename(&index_lock.path, &index) {
        index_lock.keep = true;
        bail!(
            "commit {committed} exists but index installation failed: {error}; index.lock retained for recovery"
        );
    }
    let mut result =
        json!({"commit": committed, "branch": branch, "paths": paths, "verification": "required"});
    if let Some(warning) = warning {
        result["warning"] = Value::String(warning);
    }
    Ok(result)
}

fn quote(path: &str) -> String {
    if !path.is_empty()
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-./".contains(&byte))
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

fn next_index_locks(directory: &Path) -> Vec<std::ffi::OsString> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with("next-index-") && name.ends_with(".lock")
        })
        .collect()
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn candidate_lock_path(alternate: &Path) -> PathBuf {
    let mut own = alternate.as_os_str().to_owned();
    own.push(".lock");
    PathBuf::from(own)
}

/// Removes `next-index-<pid>.lock` files that appeared during a failed commit and whose
/// owning process no longer exists, i.e. the child we killed. Locks of live processes stay.
fn remove_dead_locks(repo: &Repo, before: &[std::ffi::OsString]) {
    for name in next_index_locks(&repo.directory) {
        if before.contains(&name) {
            continue;
        }
        let text = name.to_string_lossy();
        let Some(pid) = text
            .strip_prefix("next-index-")
            .and_then(|rest| rest.strip_suffix(".lock"))
            .and_then(|pid| pid.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        // SAFETY: signal 0 only probes for existence of the process.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
        if !alive {
            let _ = fs::remove_file(repo.directory.join(&name));
        }
    }
}

/// Rebuilds the post-commit index from the pre-commit index: checkpointed paths take their
/// new HEAD state while unrelated staged entries are untouched.
fn rebuild_index(
    repo: &Repo,
    original: &[u8],
    original_modified: std::time::SystemTime,
    alternate: &Path,
    paths: &[String],
) -> Result<Vec<u8>> {
    fs::write(alternate, original)?;
    // Keep the original timestamp so Git's racy-entry detection stays valid.
    File::open(alternate)?.set_modified(original_modified)?;
    let mut reset = vec!["reset", "-q", "--"];
    reset.extend(paths.iter().map(String::as_str));
    repo.git_index(&reset, alternate)?;
    Ok(fs::read(alternate)?)
}

pub(crate) fn verify(repo: &Repo, command: &[String], timeout: usize) -> Result<Value> {
    let _guard = Lock::acquire(repo.directory.join("aisc-operation.lock"))?;
    clear_evidence(repo)?;
    repo.writable()?;
    repo.require_clean()?;
    let before = fingerprint(repo)?;
    let resolved = resolve_executable(&repo.root, &command[0])?;
    let executable = executable_digest(&resolved)?;
    let mut child = Command::new(&resolved);
    child
        .args(&command[1..])
        .current_dir(&repo.root)
        .env("PWD", &repo.root);
    process::exact_executable(&mut child, &command[0])?;
    let output = process::execute(
        &mut child,
        Duration::from_secs(timeout as u64),
        CAPTURE_LIMIT,
    )?;
    let clean = repo.require_clean();
    // A command that deletes or replaces tracked files makes fingerprinting itself fail.
    let after = fingerprint(repo);
    if clean.is_err()
        || after.as_ref().map_or(true, |after| *after != before)
        || executable != executable_digest(&resolve_executable(&repo.root, &command[0])?)?
    {
        bail!(
            "verification candidate or environment changed while the command ran; no successful evidence recorded"
        );
    }
    let (stdout, stdout_omitted) = head_and_tail(&output.stdout, output.stdout_omitted);
    let (stderr, stderr_omitted) = head_and_tail(&output.stderr, output.stderr_omitted);
    if !output.success() {
        bail!(
            "verification failed (exit {:?}, timeout={}): stdout={} [omitted {} bytes]; stderr={} [omitted {} bytes]",
            output.code,
            output.timed_out,
            stdout,
            stdout_omitted,
            stderr,
            stderr_omitted
        );
    }
    let head = repo.head()?;
    let evidence = Evidence {
        head: head.clone(),
        fingerprint: before,
        command: command.to_vec(),
        executable,
        environment: environment_digests(),
    };
    write_json(&repo.directory.join("aisc-evidence.json"), &evidence)?;
    Ok(
        json!({"verified": true, "commit": head, "command": command, "stdout": stdout, "stderr": stderr, "stdout_omitted_bytes": stdout_omitted, "stderr_omitted_bytes": stderr_omitted}),
    )
}

pub(crate) fn finish(repo: &Repo, keep: bool) -> Result<Value> {
    let _guard = Lock::acquire(repo.directory.join("aisc-operation.lock"))?;
    repo.writable()?;
    repo.require_clean()?;
    let evidence: Evidence = read_json(&repo.directory.join("aisc-evidence.json"))
        .context("run successful verification for this exact candidate before finish")?;
    let mut changed = Vec::new();
    if repo.head()? != evidence.head {
        changed.push("HEAD".to_owned());
    }
    if fingerprint(repo)? != evidence.fingerprint {
        changed.push("tracked files, index, branch or Git configuration".to_owned());
    }
    if evidence.command.is_empty()
        || executable_digest(&resolve_executable(&repo.root, &evidence.command[0])?)?
            != evidence.executable
    {
        changed.push("verification executable".to_owned());
    }
    let current = environment_digests();
    let mut names = evidence
        .environment
        .keys()
        .chain(current.keys())
        .filter(|name| evidence.environment.get(*name) != current.get(*name))
        .cloned()
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    if !names.is_empty() {
        changed.push(format!("environment variables {}", names.join(", ")));
    }
    if !changed.is_empty() {
        bail!(
            "verification evidence is stale; changed since verify: {}; rerun verify for the current candidate and environment",
            changed.join("; ")
        );
    }
    let state = repo.directory.join("aisc-session.json");
    let session = if state.exists() {
        let mut session: Session = read_json(&state)?;
        repo.git(&["merge-base", "--is-ancestor", &session.base, &evidence.head])
            .context("task history diverged from its recorded base")?;
        session.finished = true;
        write_json(&state, &session)?;
        Some(session)
    } else {
        None
    };
    let branch = repo.branch()?;
    let mut result = json!({"task": session.as_ref().map(|session| &session.name), "commit": evidence.head, "branch": branch, "ref": branch.map(|branch| format!("refs/heads/{branch}")), "verified": true, "command": evidence.command});
    if let Some(session) = session.filter(|session| session.mode == "worktree") {
        let cleanup = if keep {
            serde_json::Map::from_iter([("removed_worktree".to_owned(), Value::Null)])
        } else {
            worktree::remove_after_finish(repo, &session.name)
        };
        result
            .as_object_mut()
            .context("finish result is an object")?
            .extend(cleanup);
    }
    Ok(result)
}

fn clear_evidence(repo: &Repo) -> Result<()> {
    match fs::remove_file(repo.directory.join("aisc-evidence.json")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("could not invalidate old verification evidence"),
    }
}

fn fingerprint(repo: &Repo) -> Result<String> {
    let mut hash = Sha256::new();
    feed(&mut hash, os_bytes(repo.root.as_os_str()));
    feed(&mut hash, repo.head()?.as_bytes());
    feed(&mut hash, repo.branch()?.unwrap_or_default().as_bytes());
    feed(&mut hash, &fs::read(repo.directory.join("index"))?);
    feed(
        &mut hash,
        &repo.git(&[
            "config",
            "--null",
            "--list",
            "--show-origin",
            "--show-scope",
        ])?,
    );
    // Status alone does not expose assume-unchanged files; hash every tracked file. Entries
    // outside a sparse cone (`S`, vetted by `writable`) are absent on disk and covered by the index.
    let records = repo.git(&["ls-files", "-v", "-z"])?;
    for record in records
        .split(|byte| *byte == 0)
        .filter(|record| record.len() > 2 && !record.starts_with(b"S "))
    {
        let path = &record[2..];
        let path_text = text(path)?;
        feed(&mut hash, path);
        let path = repo.root.join(path_text);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("tracked file {path_text} is missing or unreadable"))?;
        use std::os::unix::fs::PermissionsExt;
        feed(&mut hash, &metadata.permissions().mode().to_le_bytes());
        if metadata.file_type().is_symlink() {
            feed(&mut hash, os_bytes(fs::read_link(path)?.as_os_str()));
        } else if metadata.is_file() {
            hash_file(&mut hash, &path)?;
        } else {
            bail!("unsupported tracked file type");
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn resolve_executable(root: &Path, program: &str) -> Result<PathBuf> {
    let path = if program.contains('/') {
        PathBuf::from(program)
    } else {
        let path = std::env::var_os("PATH").context("PATH is unset")?;
        std::env::split_paths(&path)
            .map(|directory| root.join(directory).join(program))
            .find(|path| is_executable(path))
            .context("verification executable not found in PATH")?
    };
    let path = root.join(path);
    if !is_executable(&path) {
        bail!("verification program is not an executable regular file");
    }
    fs::canonicalize(path).context("could not resolve verification executable")
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(os_bytes(path.as_os_str())) else {
        return false;
    };
    // SAFETY: access reads this live NUL-terminated path only during the call.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

fn executable_digest(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    feed(&mut hash, os_bytes(path.as_os_str()));
    hash_file(&mut hash, path)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn hash_file(hash: &mut Sha256, path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    hash.update(size.to_le_bytes());
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(())
}

fn feed(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.lock");
    let mut lock = Lock::acquire(temporary)?;
    lock.file.write_all(&serde_json::to_vec(value)?)?;
    lock.file.sync_all()?;
    fs::rename(&lock.path, path)?;
    Ok(())
}

pub(crate) fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    let file = OpenOptions::new().read(true).open(path)?;
    if file.metadata()?.len() > 1_048_576 {
        bail!("AISC metadata exceeds safety limit");
    }
    Ok(serde_json::from_reader(file)?)
}

/// A cone directory: normal relative components only, joined with `/`.
fn sparse_directory(directory: &str) -> Result<String> {
    if directory.contains(['\\', '\0']) {
        bail!("sparse directories must be plain relative directories: {directory:?}");
    }
    let mut parts = Vec::new();
    for component in Path::new(directory).components() {
        let std::path::Component::Normal(part) = component else {
            bail!("sparse directories must be plain relative directories: {directory:?}");
        };
        let part = part
            .to_str()
            .context("non-UTF-8 sparse directory is unsupported")?;
        if part.eq_ignore_ascii_case(".git") || part.starts_with('-') {
            bail!("sparse directories must be plain relative directories: {directory:?}");
        }
        parts.push(part);
    }
    if parts.is_empty() {
        bail!("sparse directories must be plain relative directories: {directory:?}");
    }
    Ok(parts.join("/"))
}

fn git_at(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(path)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = process::execute(&mut command, Duration::from_secs(30), 16_384)?;
    if !output.success() {
        bail!("could not inspect created Git worktree");
    }
    Ok(output.stdout)
}
