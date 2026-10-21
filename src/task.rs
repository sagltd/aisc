use crate::git::{Lock, Repo, os_bytes, text};
use crate::process;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct Session {
    name: String,
    base: String,
    mode: String,
    finished: bool,
}

#[derive(Serialize, Deserialize)]
struct Evidence {
    head: String,
    fingerprint: String,
    command: Vec<String>,
    executable: String,
}

pub(crate) fn start(repo: &Repo, name: &str, destination: Option<PathBuf>) -> Result<Value> {
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
            let branch = format!("aisc/{name}");
            repo.git(&["worktree", "add", "-b", &branch, "--", path_text, &base])?;
            let directory = PathBuf::from(
                text(&git_at(&path, &["rev-parse", "--absolute-git-dir"])?)?.trim_end_matches('\n'),
            );
            session.mode = "worktree".to_owned();
            write_json(&directory.join("aisc-session.json"), &session).context(
                "worktree exists but task metadata could not be written; no cleanup was attempted",
            )?;
            Ok(
                json!({"task": name, "mode": "worktree", "branch": branch, "head": base, "path": path}),
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
    repo.git_index(&commit, &alternate.path).context("checkpoint failed; original index retained (configured hooks may have their own side effects)")?;
    let committed = repo.head()?;
    if committed == head {
        bail!("Git reported success without advancing HEAD; original index retained");
    }
    if repo.branch()? != branch || fs::read(&index)? != original {
        bail!(
            "concurrent repository change after commit {committed}; original index retained; inspect Git state before proceeding"
        );
    }
    let new_index = fs::read(&alternate.path)?;
    index_lock.file.write_all(&new_index)?;
    index_lock.file.sync_all()?;
    // Match Git's index transaction: the ref advances before atomic index installation.
    if let Err(error) = fs::rename(&index_lock.path, &index) {
        index_lock.keep = true;
        bail!(
            "commit {committed} exists but index installation failed: {error}; index.lock retained for recovery"
        );
    }
    Ok(json!({"commit": committed, "branch": branch, "paths": paths, "verification": "required"}))
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
    child.args(&command[1..]).current_dir(&repo.root);
    process::exact_executable(&mut child, &command[0])?;
    let output = process::execute(&mut child, Duration::from_secs(timeout as u64), 16_384)?;
    let clean = repo.require_clean();
    let after = fingerprint(repo)?;
    if clean.is_err()
        || before != after
        || executable != executable_digest(&resolve_executable(&repo.root, &command[0])?)?
    {
        bail!(
            "verification candidate or environment changed while the command ran; no successful evidence recorded"
        );
    }
    if !output.success() {
        bail!(
            "verification failed (exit {:?}, timeout={}): stdout={} [omitted {} bytes]; stderr={} [omitted {} bytes]",
            output.code,
            output.timed_out,
            String::from_utf8_lossy(&output.stdout),
            output.stdout_omitted,
            String::from_utf8_lossy(&output.stderr),
            output.stderr_omitted
        );
    }
    let head = repo.head()?;
    let evidence = Evidence {
        head: head.clone(),
        fingerprint: before,
        command: command.to_vec(),
        executable,
    };
    write_json(&repo.directory.join("aisc-evidence.json"), &evidence)?;
    Ok(
        json!({"verified": true, "commit": head, "command": command, "stdout": String::from_utf8_lossy(&output.stdout), "stderr": String::from_utf8_lossy(&output.stderr), "stdout_omitted_bytes": output.stdout_omitted, "stderr_omitted_bytes": output.stderr_omitted}),
    )
}

pub(crate) fn finish(repo: &Repo) -> Result<Value> {
    let _guard = Lock::acquire(repo.directory.join("aisc-operation.lock"))?;
    repo.writable()?;
    repo.require_clean()?;
    let evidence: Evidence = read_json(&repo.directory.join("aisc-evidence.json"))
        .context("run successful verification for this exact candidate before finish")?;
    if evidence.command.is_empty()
        || repo.head()? != evidence.head
        || fingerprint(repo)? != evidence.fingerprint
        || executable_digest(&resolve_executable(&repo.root, &evidence.command[0])?)?
            != evidence.executable
    {
        bail!(
            "verification evidence is stale; rerun verify for the current candidate and environment"
        );
    }
    let state = repo.directory.join("aisc-session.json");
    let task = if state.exists() {
        let mut session: Session = read_json(&state)?;
        repo.git(&["merge-base", "--is-ancestor", &session.base, &evidence.head])
            .context("task history diverged from its recorded base")?;
        session.finished = true;
        write_json(&state, &session)?;
        Some(session.name)
    } else {
        None
    };
    let branch = repo.branch()?;
    Ok(
        json!({"task": task, "commit": evidence.head, "branch": branch, "ref": branch.map(|branch| format!("refs/heads/{branch}")), "verified": true, "command": evidence.command}),
    )
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
    let mut environment = std::env::vars_os().collect::<Vec<_>>();
    environment.sort();
    for (name, value) in environment {
        // Codex changes this orchestration identifier on every shell tool call.
        if name == "CODEX_TOOL_CALL_ID" {
            continue;
        }
        feed(&mut hash, os_bytes(&name));
        feed(&mut hash, os_bytes(&value));
    }
    // Status alone does not expose assume-unchanged files; hash every tracked file.
    let paths = repo.git(&["ls-files", "-z"])?;
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path_text = text(path)?;
        feed(&mut hash, path);
        let path = repo.root.join(path_text);
        let metadata = fs::symlink_metadata(&path)?;
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

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.lock");
    let mut lock = Lock::acquire(temporary)?;
    lock.file.write_all(&serde_json::to_vec(value)?)?;
    lock.file.sync_all()?;
    fs::rename(&lock.path, path)?;
    Ok(())
}

fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    let file = OpenOptions::new().read(true).open(path)?;
    if file.metadata()?.len() > 1_048_576 {
        bail!("AISC metadata exceeds safety limit");
    }
    Ok(serde_json::from_reader(file)?)
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
