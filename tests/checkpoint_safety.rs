use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Fixture"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("first.txt"), "base\n").unwrap();
        fs::write(root.join("second.txt"), "base\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "base"]);
        Self { directory, root }
    }

    /// Installs a fake `git` ahead of the real one. The script may call `REAL "$@"`.
    fn fake_git(&self, script: &str) -> std::ffi::OsString {
        let bin = self.directory.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let body =
            format!("#!/bin/sh\nREAL() {{ PATH=\"$AISC_ORIGINAL_PATH\" git \"$@\"; }}\n{script}\n");
        fs::write(bin.join("git"), body).unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let original = std::env::var_os("PATH").unwrap();
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&original))).unwrap()
    }

    fn run_with_path(&self, path: &std::ffi::OsStr, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(args)
            .current_dir(&self.root)
            .env("PATH", path)
            .env("AISC_ORIGINAL_PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    }

    fn head(&self) -> String {
        String::from_utf8(git(&self.root, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn status(&self) -> String {
        String::from_utf8(git(&self.root, &["status", "--porcelain"])).unwrap()
    }
}

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aisc"))
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn commit_that_lands_before_git_fails_still_installs_index_and_reports_commit() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("second.txt"), "staged other\n").unwrap();
    git(&fixture.root, &["add", "second.txt"]);
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let before = fixture.head();
    let path = fixture.fake_git(
        "case \" $* \" in *\" commit \"*) REAL \"$@\"; echo 'hook: post-commit exploded' >&2; exit 1;; esac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    let head = fixture.head();
    assert_ne!(head, before);
    let combined = text(&output);
    assert!(combined.contains(&head), "commit id lost: {combined}");
    assert!(combined.contains("hook"), "{combined}");
    // The index must not revert the checkpoint and must keep the unrelated staged entry.
    assert_eq!(fixture.status(), "M  second.txt\n", "{combined}");
    assert!(!fixture.root.join(".git/index.lock").exists());
}

#[test]
fn missing_candidate_index_after_commit_rebuilds_index_from_head() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let path = fixture.fake_git(
        "case \" $* \" in *\" commit \"*) REAL \"$@\"; rc=$?; rm -f \"$GIT_INDEX_FILE\"; exit $rc;; esac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    let combined = text(&output);
    assert!(combined.contains(&fixture.head()), "{combined}");
    assert_eq!(fixture.status(), "");
}

#[test]
fn unrecoverable_index_failure_after_commit_names_commit_and_recovery_command() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let path = fixture.fake_git(
        "case \" $* \" in\n*\" commit \"*) REAL \"$@\"; rc=$?; rm -f \"$GIT_INDEX_FILE\"; exit $rc;;\n*\" reset \"*) echo nope >&2; exit 1;;\nesac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(!output.status.success());
    let combined = text(&output);
    assert!(combined.contains(&fixture.head()), "{combined}");
    assert!(combined.contains("committed"), "{combined}");
    assert!(combined.contains("git reset -- first.txt"), "{combined}");
    assert!(!combined.contains("os error 2"), "{combined}");
}

#[test]
fn commit_that_never_lands_cleans_its_own_locks_and_keeps_index() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let before = fs::read(fixture.root.join(".git/index")).unwrap();
    let path = fixture.fake_git(
        "case \" $* \" in *\" commit \"*) touch \"$GIT_INDEX_FILE.lock\" .git/next-index-$$.lock; exit 1;; esac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), before);
    for name in [
        "aisc-candidate-index.lock",
        "aisc-operation.lock",
        "index.lock",
    ] {
        assert!(!fixture.root.join(".git").join(name).exists(), "{name}");
    }
    // A retry must not be wedged.
    let retry = run(&fixture.root, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(retry.status.success(), "{}", text(&retry));
}

#[test]
fn foreign_locks_are_never_deleted() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    // The lock of a live process (aisc itself, via $PPID) appears during a failed commit.
    let path = fixture.fake_git(
        "case \" $* \" in *\" commit \"*) touch .git/next-index-$PPID.lock; exit 1;; esac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(!output.status.success());
    let foreign = fs::read_dir(fixture.root.join(".git"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("next-index-")
        });
    assert!(foreign, "a live process's lock was deleted");
}

#[test]
fn verify_that_deletes_tracked_file_reports_changed_candidate() {
    let fixture = Fixture::new();
    let output = run(&fixture.root, &["verify", "--", "rm", "first.txt"]);
    assert!(!output.status.success());
    let combined = text(&output);
    assert!(combined.contains("changed"), "{combined}");
    assert!(!combined.contains("os error 2"), "{combined}");
}

#[test]
fn checkpoint_preserves_shared_repository_index_mode() {
    let fixture = Fixture::new();
    let index = fixture.root.join(".git/index");
    fs::set_permissions(&index, fs::Permissions::from_mode(0o664)).unwrap();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let output = run(&fixture.root, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fs::metadata(&index).unwrap().permissions().mode() & 0o777,
        0o664
    );
}

#[test]
fn installed_index_keeps_candidate_timestamp() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let record = fixture.directory.path().join("candidate-mtime");
    let path = fixture.fake_git(&format!(
        "case \" $* \" in *\" commit \"*) REAL \"$@\"; rc=$?; touch -r \"$GIT_INDEX_FILE\" '{}'; sleep 1.1; exit $rc;; esac\nREAL \"$@\"",
        record.display()
    ));
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(output.status.success(), "{}", text(&output));
    let installed = fs::metadata(fixture.root.join(".git/index"))
        .unwrap()
        .modified()
        .unwrap();
    let candidate = fs::metadata(&record).unwrap().modified().unwrap();
    assert_eq!(installed, candidate);
}

fn finish_with_env(cwd: &Path, extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aisc"));
    command
        .arg("finish")
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("PWD", cwd)
        .env("OLDPWD", "/somewhere/else")
        .env("SHLVL", "7")
        .env("_", "/usr/bin/other");
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().unwrap()
}

#[test]
fn finish_from_subdirectory_is_not_stale_and_stale_names_changed_variables() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("sub")).unwrap();
    let verified = run(&fixture.root, &["verify", "--", "true"]);
    assert!(verified.status.success(), "{}", text(&verified));
    let sub = fixture.root.join("sub");
    let ok = finish_with_env(&sub, &[]);
    assert!(ok.status.success(), "{}", text(&ok));
    let stale = finish_with_env(&sub, &[("AISC_CHANGED_VARIABLE", "secret-value")]);
    assert!(!stale.status.success());
    let combined = text(&stale);
    assert!(combined.contains("AISC_CHANGED_VARIABLE"), "{combined}");
    assert!(!combined.contains("secret-value"), "{combined}");
}

#[test]
fn verify_output_keeps_head_and_tail() {
    let fixture = Fixture::new();
    let output = run(
        &fixture.root,
        &[
            "verify",
            "--",
            "sh",
            "-c",
            "echo HEAD_MARKER; yes filler | head -c 100000; echo TAIL_MARKER",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stdout = value["stdout"].as_str().unwrap();
    assert!(stdout.contains("HEAD_MARKER"));
    assert!(stdout.contains("TAIL_MARKER"));
    assert!(stdout.contains("bytes omitted"));
    assert!(stdout.len() <= 16_384 + 64);
    assert!(value["stdout_omitted_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn context_rejects_diffstat_that_changes_between_reads() {
    let fixture = Fixture::new();
    // Already modified, so the later edit changes the line counts but not the status letters.
    fs::write(fixture.root.join("first.txt"), "a\nb\n").unwrap();
    let marker = fixture.directory.path().join("raced");
    let capture = fixture.directory.path().join("capture");
    let path = fixture.fake_git(&format!(
        "case \" $* \" in *--cached*) ;; *\" diff \"*--numstat*) REAL \"$@\" > '{capture}'; rc=$?\nif mkdir '{marker}' 2>/dev/null; then printf 'a\\nb\\nc\\nd\\n' > first.txt; fi\ncat '{capture}'; exit $rc;; esac\nREAL \"$@\"",
        capture = capture.display(),
        marker = marker.display()
    ));
    let output = fixture.run_with_path(&path, &["context"]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("changed"));
}

#[test]
fn rebuilt_index_keeps_original_index_timestamp() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let stamp = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
    fs::File::open(fixture.root.join(".git/index"))
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let record = fixture.directory.path().join("reset-mtime");
    let path = fixture.fake_git(&format!(
        "case \" $* \" in\n*\" commit \"*) REAL \"$@\"; rc=$?; rm -f \"$GIT_INDEX_FILE\"; exit $rc;;\n*\" reset \"*) touch -r \"$GIT_INDEX_FILE\" '{}';; esac\nREAL \"$@\"",
        record.display()
    ));
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(fs::metadata(&record).unwrap().modified().unwrap(), stamp);
}

#[test]
fn candidate_lock_is_removed_when_staging_fails() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let path = fixture.fake_git(
        "case \" $* \" in *\" add \"*) touch \"$GIT_INDEX_FILE.lock\"; exit 1;; esac\nREAL \"$@\"",
    );
    let output = fixture.run_with_path(&path, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(!output.status.success());
    assert!(!fixture.root.join(".git/aisc-candidate-index.lock").exists());
    let retry = run(&fixture.root, &["checkpoint", "-m", "x", "--", "first.txt"]);
    assert!(retry.status.success(), "{}", text(&retry));
}

#[test]
fn oversized_output_marker_says_true_tail_is_unavailable() {
    let fixture = Fixture::new();
    let output = run(
        &fixture.root,
        &["verify", "--", "sh", "-c", "yes filler | head -c 3000000"],
    );
    assert!(output.status.success(), "{}", text(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["stdout"].as_str().unwrap().contains("capture"));
}
