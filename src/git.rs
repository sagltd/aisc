use crate::process;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GIT_LIMIT: usize = 16 * 1_024 * 1_024;

pub(crate) struct Repo {
    pub root: PathBuf,
    pub directory: PathBuf,
}

#[derive(Serialize)]
pub(crate) struct Entry {
    pub status: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
}

pub(crate) struct Lock {
    pub path: PathBuf,
    pub file: File,
    pub keep: bool,
}

impl Lock {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "checkout is locked: {}; inspect owner before removing a stale lock",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            })?;
        Ok(Self {
            path,
            file,
            keep: false,
        })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Repo {
    pub fn discover() -> Result<Self> {
        validate_environment()?;
        let cwd = std::env::current_dir()?;
        let root = PathBuf::from(
            text(&run_git(&cwd, &["rev-parse", "--show-toplevel"], None)?)?
                .strip_suffix('\n')
                .context("invalid Git root response")?,
        );
        let directory = PathBuf::from(
            text(&run_git(&root, &["rev-parse", "--absolute-git-dir"], None)?)?
                .strip_suffix('\n')
                .context("invalid Git directory response")?,
        );
        Ok(Self { root, directory })
    }
    pub fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        run_git(&self.root, args, None)
    }
    pub fn common_dir(&self) -> Result<PathBuf> {
        let output = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(
            text(&output)?
                .strip_suffix('\n')
                .context("invalid Git common directory response")?,
        ))
    }
    pub fn git_index(&self, args: &[&str], index: &Path) -> Result<Vec<u8>> {
        run_git(&self.root, args, Some(index))
    }
    pub fn head(&self) -> Result<String> {
        Ok(text(&self.git(&["rev-parse", "--verify", "HEAD"])?)?
            .trim()
            .to_owned())
    }
    pub fn branch(&self) -> Result<Option<String>> {
        let output = self.git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = text(&output)?.trim();
        Ok((branch != "HEAD").then(|| branch.to_owned()))
    }
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let status = self.git(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--renames",
            "--untracked-files=all",
        ])?;
        let mut parts = status
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty());
        let mut entries = Vec::new();
        while let Some(record) = parts.next() {
            if record.len() < 4 || record[2] != b' ' {
                bail!("invalid Git status record");
            }
            let original_path =
                if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
                    Some(text(parts.next().context("rename missing original path")?)?.to_owned())
                } else {
                    None
                };
            entries.push(Entry {
                status: text(&record[..2])?.to_owned(),
                path: text(&record[3..])?.to_owned(),
                original_path,
            });
        }
        Ok(entries)
    }
    pub fn require_clean(&self) -> Result<()> {
        if !self.entries()?.is_empty() {
            bail!(
                "verification and in-place start require a clean committed checkout (including nonignored untracked files)"
            );
        }
        Ok(())
    }
    pub fn writable(&self) -> Result<()> {
        let stage = self.git(&["ls-files", "--stage", "-z"])?;
        text(&stage)?;
        if stage
            .split(|byte| *byte == 0)
            .any(|record| record.starts_with(b"160000 "))
        {
            bail!("submodule checkouts are unsupported for task writes");
        }
        let cone = self.sparse_cone()?;
        let flags = self.git(&["ls-files", "-v", "-z"])?;
        for record in flags.split(|byte| *byte == 0) {
            let Some(flag) = record.first() else {
                continue;
            };
            // Skip-worktree is how a cone checkout hides paths outside the cone; inside it, or
            // without a cone, the flag would hide edits from verification.
            let hidden = flag.is_ascii_lowercase()
                || (*flag == b'S'
                    && match &cone {
                        None => true,
                        Some(cone) => in_cone(text(record.get(2..).unwrap_or_default())?, cone),
                    });
            if hidden {
                bail!(
                    "assume-unchanged and skip-worktree entries are unsupported for task writes and verification"
                );
            }
        }
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
        ] {
            if self.directory.join(marker).exists() {
                bail!("finish the existing Git merge/rebase/sequencer operation first");
            }
        }
        if self
            .entries()?
            .iter()
            .any(|entry| is_conflict(&entry.status))
        {
            bail!("resolve Git conflicts before task writes");
        }
        Ok(())
    }
    fn config_flag(&self, name: &str) -> Result<bool> {
        Ok(text(
            &self
                .git(&["config", "--bool", "--get", name])
                .unwrap_or_default(),
        )?
        .trim()
            == "true")
    }
    /// Directories of a cone-mode sparse checkout; `None` for a full checkout. Non-cone sparse
    /// patterns cannot be checked path by path, so they are refused.
    pub fn sparse_cone(&self) -> Result<Option<Vec<String>>> {
        if !self.config_flag("core.sparseCheckout")? {
            return Ok(None);
        }
        if !self.config_flag("core.sparseCheckoutCone")? {
            bail!("non-cone sparse checkouts are unsupported for task writes");
        }
        let list = self.git(&["sparse-checkout", "list"])?;
        Ok(Some(text(&list)?.lines().map(str::to_owned).collect()))
    }
    pub fn paths(&self, paths: &[String]) -> Result<()> {
        if let Some(cone) = self.sparse_cone()?
            && let Some(outside) = paths.iter().find(|path| !in_cone(path, &cone))
        {
            bail!(
                "path is outside the sparse checkout ({}): {outside:?}",
                cone.join(", ")
            );
        }
        for path in paths {
            if path.is_empty()
                || path.starts_with(['-', ':'])
                || path.contains('\0')
                || path.contains('\\')
            {
                bail!("unsafe literal file path: {path:?}");
            }
            let mut candidate = self.root.clone();
            let components = Path::new(path).components().collect::<Vec<_>>();
            for (position, component) in components.iter().enumerate() {
                let Component::Normal(name) = component else {
                    bail!("paths must be normal relative files: {path:?}");
                };
                if name.to_string_lossy().eq_ignore_ascii_case(".git") {
                    bail!("Git metadata paths are forbidden");
                }
                candidate.push(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata)
                        if position + 1 < components.len()
                            && (!metadata.is_dir() || metadata.file_type().is_symlink()) =>
                    {
                        bail!("path traverses a non-directory or symlink: {path:?}")
                    }
                    Ok(metadata) if position + 1 == components.len() && metadata.is_dir() => {
                        bail!("explicit file paths required; directories are unsupported: {path:?}")
                    }
                    Ok(_) => {}
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && position + 1 == components.len() => {}
                    Err(error) => return Err(error).context("could not validate file path"),
                }
            }
        }
        Ok(())
    }
    pub fn diff(&self, paths: &[String], limit: usize) -> Result<Value> {
        self.paths(paths)?;
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
            "HEAD",
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        let output = self.git(&args)?;
        let end = output.len().min(limit);
        let end = match std::str::from_utf8(&output[..end]) {
            Ok(_) => end,
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => bail!("non-UTF-8 diff content is unsupported"),
        };
        Ok(
            json!({"diff": text(&output[..end])?, "omitted_bytes": output.len() - end, "scope": "tracked working tree versus HEAD; untracked files are listed by context"}),
        )
    }
}

/// Cone-mode membership: root files, everything below a cone directory, and the files directly
/// inside each ancestor of a cone directory.
pub(crate) fn in_cone(path: &str, cone: &[String]) -> bool {
    let Some((parent, _)) = path.rsplit_once('/') else {
        return true;
    };
    cone.iter().any(|directory| {
        path.starts_with(&format!("{directory}/")) || directory.starts_with(&format!("{parent}/"))
    })
}

pub(crate) fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).context("non-UTF-8 Git paths/output are unsupported")
}
pub(crate) fn is_conflict(status: &str) -> bool {
    matches!(status, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
}

/// Reads are bounded (30 s). Commands that may run hooks, plus reference transactions, must not be
/// killed halfway, so they get an hour. `AISC_GIT_TIMEOUT` (seconds) overrides both.
pub(crate) fn git_timeout(args: &[&str]) -> Duration {
    if let Some(seconds) = std::env::var("AISC_GIT_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
    {
        return Duration::from_secs(seconds);
    }
    let mut position = 0;
    while position < args.len() {
        if args[position] == "-c" {
            position += 2;
        } else if args[position].starts_with('-') {
            position += 1;
        } else {
            break;
        }
    }
    let subcommand = args.get(position).copied().unwrap_or("");
    let long = matches!(
        subcommand,
        "commit"
            | "merge"
            | "rebase"
            | "am"
            | "cherry-pick"
            | "checkout"
            | "switch"
            | "pull"
            | "push"
            | "gc"
            | "update-ref"
            | "fetch"
    );
    Duration::from_secs(if long { 3_600 } else { 30 })
}

pub(crate) fn run_git_raw(
    root: &Path,
    args: &[&str],
    env: &[(&str, &OsStr)],
    input: Option<Vec<u8>>,
    limit: usize,
) -> Result<process::Output> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    for (key, value) in env {
        command.env(key, value);
    }
    let timeout = git_timeout(args);
    match input {
        Some(input) => process::execute_input(&mut command, input, timeout, limit),
        None => process::execute(&mut command, timeout, limit),
    }
}

pub(crate) fn run_git(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let env: Vec<(&str, &OsStr)> = index
        .map(|index| vec![("GIT_INDEX_FILE", index.as_os_str())])
        .unwrap_or_default();
    let output = run_git_raw(root, args, &env, None, GIT_LIMIT)?;
    if !output.success() {
        let snippet = |bytes: &[u8]| {
            crate::output::escape_block(&String::from_utf8_lossy(&bytes[..bytes.len().min(4_096)]))
        };
        bail!(
            "git {} failed (exit {:?}, timeout={}): {}; stdout: {}",
            args.first().copied().unwrap_or(""),
            output.code,
            output.timed_out,
            snippet(&output.stderr).trim(),
            snippet(&output.stdout).trim()
        );
    }
    if output.stdout_omitted != 0 || output.stderr_omitted != 0 {
        bail!("Git output exceeds the 16 MiB safety limit; narrow the operation");
    }
    Ok(output.stdout)
}

pub(crate) fn os_bytes(value: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes()
}

pub(crate) fn validate_environment() -> Result<()> {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        if std::env::var_os(name).is_some() {
            bail!("{name} overrides are unsupported; unset it before running aisc");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cone_membership_follows_git_cone_mode() {
        let cone = vec!["src/a".to_owned()];
        for inside in ["top.txt", "src/y.txt", "src/a/x.txt", "src/a/deep/z.txt"] {
            assert!(in_cone(inside, &cone), "{inside}");
        }
        for outside in ["docs/d.txt", "src/b/x.txt", "src/ab/x.txt", "srcx/y.txt"] {
            assert!(!in_cone(outside, &cone), "{outside}");
        }
        assert!(!in_cone("docs/d.txt", &[]));
        assert!(in_cone("root.txt", &[]));
    }

    #[test]
    fn hook_running_and_ref_commands_are_not_killed_after_thirty_seconds() {
        if std::env::var_os("AISC_GIT_TIMEOUT").is_some() {
            return;
        }
        assert_eq!(git_timeout(&["status"]), Duration::from_secs(30));
        assert_eq!(
            git_timeout(&["-c", "core.quotePath=true", "diff", "HEAD"]),
            Duration::from_secs(30)
        );
        for args in [
            vec!["commit", "-m", "x"],
            vec!["-c", "a=b", "commit"],
            vec!["update-ref", "--stdin"],
            vec!["merge", "topic"],
        ] {
            assert_eq!(git_timeout(&args), Duration::from_secs(3_600), "{args:?}");
        }
    }
}
