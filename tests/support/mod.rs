#![allow(dead_code)] // each test crate uses a different subset
//! Shared fixtures for the history integration tests: a temp repo builder, an `aisc()` runner that
//! counts invocations, Git helpers, state snapshots and a PATH-wrapper that fakes a concurrent actor.
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn ok(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "stdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.stdout))
    }
}

pub struct Fixture {
    pub directory: TempDir,
    pub root: PathBuf,
    calls: AtomicUsize,
}

pub fn real_git() -> String {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2024-01-02T03:04:05+0000")
        .env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05+0000");
    command
}

impl Fixture {
    /// Empty repository with identity configured and no commits.
    pub fn bare() -> Self {
        let directory = tempfile::tempdir().unwrap();
        // Canonical path: temp dirs live under symlinked locations on macOS.
        let root = fs::canonicalize(directory.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            directory,
            root,
            calls: AtomicUsize::new(0),
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Fixture"]);
        fixture.git(&["config", "user.email", "fixture@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture
    }

    /// Repository with files committed as one `base` commit.
    pub fn with_files(files: &[(&str, &str)]) -> Self {
        let fixture = Self::bare();
        for (path, content) in files {
            fixture.write(path, content);
        }
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "base"]);
        fixture
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn write(&self, relative: &str, content: &str) {
        self.write_bytes(relative, content.as_bytes());
    }

    pub fn write_bytes(&self, relative: &str, content: &[u8]) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    pub fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).unwrap()
    }

    pub fn exists(&self, relative: &str) -> bool {
        fs::symlink_metadata(self.path(relative)).is_ok()
    }

    /// Runs git and returns trimmed stdout; panics on failure.
    pub fn git(&self, args: &[&str]) -> String {
        let output = git_command(&self.root, args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    }

    /// Runs git with `input` on stdin and returns trimmed stdout; panics on failure.
    pub fn git_stdin(&self, args: &[&str], input: &[u8]) -> String {
        use std::io::Write;
        let mut child = git_command(&self.root, args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    }

    pub fn git_bytes(&self, args: &[&str]) -> Vec<u8> {
        git_command(&self.root, args).output().unwrap().stdout
    }

    pub fn git_ok(&self, args: &[&str]) -> bool {
        git_command(&self.root, args)
            .output()
            .unwrap()
            .status
            .success()
    }

    pub fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aisc"));
        command
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("AISC_OUTPUT")
            .env_remove("NO_COLOR");
        command
    }

    fn run_of(output: Output) -> Run {
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Runs one aisc invocation and counts it.
    pub fn aisc(&self, args: &[&str]) -> Run {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Self::run_of(self.command(args).output().unwrap())
    }

    /// Runs aisc with extra environment (fake git, locks, ...).
    pub fn aisc_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut command = self.command(args);
        for (key, value) in env {
            command.env(key, value);
        }
        Self::run_of(command.output().unwrap())
    }

    pub fn spawn(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Child {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut command = self.command(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn subjects(&self, range: &str) -> Vec<String> {
        self.git(&["log", "--format=%s", range])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn tree_blob(&self, revision: &str, path: &str) -> Option<String> {
        let spec = format!("{revision}:{path}");
        let output = git_command(&self.root, &["show", &spec]).output().unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn commit_files(&self, revision: &str) -> Vec<String> {
        self.git(&["show", "--name-only", "--format=", revision])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Everything a failed operation must leave byte-identical.
    pub fn snapshot(&self) -> Snapshot {
        let mut files = BTreeMap::new();
        collect(&self.root, &self.root, &mut files);
        Snapshot {
            refs: self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            head: fs::read(self.root.join(".git/HEAD")).unwrap(),
            index: fs::read(self.root.join(".git/index")).ok(),
            files,
        }
    }

    /// Installs a fake `git` first on PATH. When aisc runs git subcommand `trigger` for the first
    /// time, `hook` (a shell snippet; `$REAL_GIT` is the real binary) runs first, then git proceeds.
    pub fn fake_git(&self, trigger: &str, hook: &str) -> Vec<(String, String)> {
        let bin = self.directory.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = r#"#!/bin/sh
sub=""; skip=0
for a in "$@"; do
  if [ $skip = 1 ]; then skip=0; continue; fi
  case "$a" in -c) skip=1;; -*) ;; *) sub="$a"; break;; esac
done
if [ "$sub" = "$AISC_TRIGGER" ] && [ ! -e "$AISC_MARK" ]; then
  : > "$AISC_MARK"
  eval "$AISC_HOOK"
fi
exec "$REAL_GIT" "$@"
"#;
        let path = bin.join("git");
        fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mark = self.directory.path().join("mark");
        let _ = fs::remove_file(&mark);
        let existing = std::env::var("PATH").unwrap_or_default();
        vec![
            ("PATH".into(), format!("{}:{existing}", bin.display())),
            ("AISC_TRIGGER".into(), trigger.into()),
            ("AISC_MARK".into(), mark.display().to_string()),
            ("AISC_HOOK".into(), hook.into()),
            ("REAL_GIT".into(), real_git()),
        ]
    }
}

pub fn env_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub refs: String,
    pub head: Vec<u8>,
    pub index: Option<Vec<u8>>,
    pub files: BTreeMap<String, Vec<u8>>,
}

fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            collect(root, &path, files);
        } else if kind.is_file() {
            files.insert(
                path.strip_prefix(root).unwrap().display().to_string(),
                fs::read(&path).unwrap(),
            );
        }
    }
}

/// Finds the id of the hunk whose hint contains `needle` in `aisc log --json` output.
pub fn hunk_id(log: &Value, path: &str, needle: &str) -> String {
    let file = log["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == path)
        .unwrap_or_else(|| panic!("no change for {path}"));
    file["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hunk| hunk["hint"].as_str().unwrap().contains(needle))
        .unwrap_or_else(|| panic!("no hunk with hint containing {needle:?} in {path}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
