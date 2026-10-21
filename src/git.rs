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
        let flags = self.git(&["ls-files", "-v", "-z"])?;
        if flags.split(|byte| *byte == 0).any(|record| {
            record
                .first()
                .is_some_and(|flag| flag.is_ascii_lowercase() || *flag == b'S')
        }) {
            bail!(
                "assume-unchanged and skip-worktree entries are unsupported for task writes and verification"
            );
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
        if text(
            &self
                .git(&["config", "--bool", "--get", "core.sparseCheckout"])
                .unwrap_or_default(),
        )?
        .trim()
            == "true"
        {
            bail!("sparse checkouts are unsupported for task writes");
        }
        Ok(())
    }
    pub fn paths(&self, paths: &[String]) -> Result<()> {
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
    pub fn context(&self, limit: usize) -> Result<Value> {
        let head = self.head()?;
        let branch = self.branch()?;
        let entries = self.entries()?;
        let staged = entries
            .iter()
            .filter(|entry| !matches!(entry.status.as_bytes()[0], b' ' | b'?'))
            .count();
        let unstaged = entries
            .iter()
            .filter(|entry| !matches!(entry.status.as_bytes()[1], b' ' | b'?'))
            .count();
        let untracked = entries.iter().filter(|entry| entry.status == "??").count();
        let conflicts = entries
            .iter()
            .filter(|entry| is_conflict(&entry.status))
            .count();
        let staged_stat = self.numstat(true, limit)?;
        let unstaged_stat = self.numstat(false, limit)?;
        if self.head()? != head || self.branch()? != branch {
            bail!("repository changed while reading context; retry");
        }
        Ok(
            json!({"branch": branch, "head": head, "staged": staged, "unstaged": unstaged, "untracked": untracked, "conflicts": conflicts, "files": entries.iter().take(limit).collect::<Vec<_>>(), "omitted": entries.len().saturating_sub(limit), "diffstat": {"staged": staged_stat, "unstaged": unstaged_stat}}),
        )
    }
    fn numstat(&self, cached: bool, limit: usize) -> Result<Value> {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--numstat",
            "-z",
        ];
        if cached {
            args.push("--cached");
        }
        let output = self.git(&args)?;
        let mut records = Vec::new();
        let mut parts_iter = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        while let Some(record) = parts_iter.next() {
            let parts = record.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
            if parts.len() != 3 {
                bail!("invalid Git numstat record");
            }
            if parts[2].is_empty() {
                let original = parts_iter
                    .next()
                    .context("numstat rename missing original path")?;
                let path = parts_iter
                    .next()
                    .context("numstat rename missing destination")?;
                records.push(json!({"path": text(path)?, "original_path": text(original)?, "added": text(parts[0])?, "deleted": text(parts[1])?}));
            } else {
                records.push(json!({"path": text(parts[2])?, "added": text(parts[0])?, "deleted": text(parts[1])?}));
            }
        }
        Ok(
            json!({"files": records.iter().take(limit).collect::<Vec<_>>(), "omitted": records.len().saturating_sub(limit)}),
        )
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

pub(crate) fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).context("non-UTF-8 Git paths/output are unsupported")
}
pub(crate) fn is_conflict(status: &str) -> bool {
    matches!(status, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
}

fn run_git(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = process::execute(&mut command, Duration::from_secs(30), GIT_LIMIT)?;
    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4_096)]);
        bail!(
            "git {} failed (exit {:?}, timeout={}): {}",
            args.first().copied().unwrap_or(""),
            output.code,
            output.timed_out,
            stderr.trim()
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
