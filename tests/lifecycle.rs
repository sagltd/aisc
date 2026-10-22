use serde_json::Value;
use std::fs;
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

    fn start(&self) -> PathBuf {
        let task = self.directory.path().join("task");
        success(
            &self.root,
            &["start", "test", "--path", task.to_str().unwrap()],
        );
        task
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

fn success(root: &Path, args: &[&str]) -> Value {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn task_lifecycle_returns_verified_commit_without_touching_source_checkout() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("second.txt"), "source staged\n").unwrap();
    git(&fixture.root, &["add", "second.txt"]);
    let source_index = git(&fixture.root, &["diff", "--cached"]);
    let source_head = git(&fixture.root, &["rev-parse", "HEAD"]);
    let task = fixture.start();
    fs::write(task.join("first.txt"), "task\n").unwrap();
    success(
        &task,
        &["checkpoint", "-m", "task change", "--", "first.txt"],
    );
    success(
        &task,
        &["verify", "--", "git", "diff", "--exit-code", "HEAD"],
    );
    let result = success(&task, &["finish"]);
    assert_eq!(result["verified"], true);
    assert_eq!(result["branch"], "aisc/test");
    // finish removes the worktree it created; the branch keeps the commit.
    assert!(!task.exists());
    let canonical = fs::canonicalize(fixture.directory.path())
        .unwrap()
        .join("task");
    assert_eq!(result["removed_worktree"], canonical.to_str().unwrap());
    assert_eq!(source_index, git(&fixture.root, &["diff", "--cached"]));
    assert_eq!(source_head, git(&fixture.root, &["rev-parse", "HEAD"]));
}

#[test]
fn checkpoint_preserves_unrelated_staging_and_commits_new_unicode_newline_paths() {
    let fixture = Fixture::new();
    let task = fixture.start();
    fs::write(task.join("second.txt"), "unrelated staged\n").unwrap();
    git(&task, &["add", "second.txt"]);
    fs::write(task.join("second.txt"), "unrelated unstaged\n").unwrap();
    let staged = git(&task, &["show", ":second.txt"]);
    let name = "new 雪\nfile.txt";
    fs::write(task.join(name), "new\n").unwrap();
    success(&task, &["checkpoint", "-m", "new file", "--", name]);
    assert_eq!(staged, git(&task, &["show", ":second.txt"]));
    assert_eq!(git(&task, &["show", "HEAD:second.txt"]), b"base\n");
    assert_eq!(git(&task, &["show", &format!("HEAD:{name}")]), b"new\n");
    let context = success(&task, &["context"]);
    assert_eq!(context["staged"], 1);
    assert_eq!(context["unstaged"], 1);
}

#[cfg(unix)]
#[test]
fn failing_hook_leaves_real_index_and_head_byte_identical() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let task = fixture.start();
    fs::write(task.join("first.txt"), "change\n").unwrap();
    fs::write(task.join("new.txt"), "new\n").unwrap();
    git(&task, &["add", "first.txt"]);
    let index_path = PathBuf::from(
        String::from_utf8(git(
            &task,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        ))
        .unwrap()
        .trim(),
    );
    let before_index = fs::read(&index_path).unwrap();
    let before_head = git(&task, &["rev-parse", "HEAD"]);
    let hooks = fixture.directory.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    fs::write(
        hooks.join("pre-commit"),
        "#!/bin/sh\ngit add second.txt\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &task,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    assert!(
        !run(&task, &["checkpoint", "-m", "fail", "--", "new.txt"])
            .status
            .success()
    );
    assert_eq!(fs::read(&index_path).unwrap(), before_index);
    assert_eq!(git(&task, &["rev-parse", "HEAD"]), before_head);
    assert!(!index_path.with_extension("lock").exists());
}

#[test]
fn verification_failure_and_later_edits_cannot_reuse_success() {
    let fixture = Fixture::new();
    let task = fixture.start();
    success(&task, &["verify", "--", "git", "status", "--porcelain"]);
    assert!(
        !run(&task, &["verify", "--", "git", "not-a-command"])
            .status
            .success()
    );
    assert!(!run(&task, &["finish"]).status.success());
    success(&task, &["verify", "--", "git", "status", "--porcelain"]);
    fs::write(task.join("first.txt"), "changed\n").unwrap();
    assert!(!run(&task, &["finish"]).status.success());
    assert!(
        !run(&task, &["verify", "--", "git", "status"])
            .status
            .success()
    );
}

#[test]
fn rejects_unsafe_paths_and_existing_index_lock_without_mutation() {
    let fixture = Fixture::new();
    let task = fixture.start();
    fs::write(task.join("first.txt"), "change\n").unwrap();
    let before = git(&task, &["status", "--porcelain=v1", "-z"]);
    for path in [
        "../first.txt",
        ":(top)first.txt",
        "--all",
        "/tmp/no",
        ".",
        "dir/../first.txt",
    ] {
        assert!(
            !run(&task, &["checkpoint", "-m", "bad", "--", path])
                .status
                .success(),
            "{path}"
        );
    }
    let lock = String::from_utf8(git(
        &task,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    ))
    .unwrap();
    fs::write(lock.trim(), "other writer").unwrap();
    assert!(
        !run(&task, &["checkpoint", "-m", "locked", "--", "first.txt"])
            .status
            .success()
    );
    assert_eq!(before, git(&task, &["status", "--porcelain=v1", "-z"]));
    assert_eq!(fs::read(lock.trim()).unwrap(), b"other writer");
}

#[test]
fn bounded_context_reports_omissions_and_staged_unstaged_stats() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), "new\nline\n").unwrap();
    git(&fixture.root, &["add", "first.txt"]);
    fs::write(fixture.root.join("second.txt"), "new\n").unwrap();
    fs::write(fixture.root.join("untracked.txt"), "new\n").unwrap();
    let context = success(&fixture.root, &["context", "--limit", "1"]);
    assert_eq!(context["staged"], 1);
    assert_eq!(context["unstaged"], 1);
    assert_eq!(context["untracked"], 1);
    assert_eq!(context["omitted"], 2);
    assert_eq!(context["files"].as_array().unwrap().len(), 1);
    assert!(context["diffstat"].is_object());
}

#[test]
fn in_place_task_keeps_branch_and_supports_repeated_lifecycles() {
    let fixture = Fixture::new();
    success(&fixture.root, &["start", "first", "--in-place"]);
    assert!(
        !run(&fixture.root, &["start", "other", "--in-place"])
            .status
            .success()
    );
    assert_eq!(git(&fixture.root, &["branch", "--show-current"]), b"main\n");
    fs::write(fixture.root.join("first.txt"), "in place\n").unwrap();
    success(
        &fixture.root,
        &["checkpoint", "-m", "in place", "--", "first.txt"],
    );
    success(
        &fixture.root,
        &["verify", "--", "git", "diff", "--exit-code", "HEAD"],
    );
    assert_eq!(success(&fixture.root, &["finish"])["branch"], "main");
    success(&fixture.root, &["start", "second", "--in-place"]);
    assert!(!run(&fixture.root, &["finish"]).status.success());
}

#[test]
fn verification_that_changes_source_is_not_evidence() {
    let fixture = Fixture::new();
    success(
        &fixture.root,
        &["verify", "--", "git", "status", "--porcelain"],
    );
    assert!(
        !run(
            &fixture.root,
            &["verify", "--", "sh", "-c", "printf changed > first.txt"]
        )
        .status
        .success()
    );
    assert!(!run(&fixture.root, &["finish"]).status.success());
}

#[cfg(unix)]
#[test]
fn timeout_kills_child_group_and_bounded_output_does_not_deadlock() {
    let fixture = Fixture::new();
    let output = success(
        &fixture.root,
        &["verify", "--", "sh", "-c", "yes output | head -c 100000"],
    );
    assert!(output["stdout_omitted_bytes"].as_u64().unwrap() > 0);
    assert!(output["stdout"].as_str().unwrap().len() <= 16_384);
    let begin = std::time::Instant::now();
    let result = run(
        &fixture.root,
        &[
            "verify",
            "--timeout",
            "1",
            "--",
            "sh",
            "-c",
            "(sleep 2; printf leaked > descendant) & wait",
        ],
    );
    assert!(!result.status.success());
    assert!(begin.elapsed() < std::time::Duration::from_secs(3));
    assert!(!run(&fixture.root, &["finish"]).status.success());
    // Give the descendant's scheduled write enough time to expose a leaked process.
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    assert!(!fixture.root.join("descendant").exists());
}

#[test]
fn verification_arguments_are_not_implicitly_evaluated_and_environment_is_bound() {
    let fixture = Fixture::new();
    success(
        &fixture.root,
        &[
            "verify",
            "--",
            "printf",
            "%s",
            "$(touch injected); > injected",
        ],
    );
    assert!(!fixture.root.join("injected").exists());
    let result = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .arg("finish")
        .current_dir(&fixture.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("AISC_DIFFERENT_ENVIRONMENT", "changed")
        .output()
        .unwrap();
    assert!(!result.status.success());
}

#[test]
fn sha256_repository_keeps_native_object_format() {
    let fixture = Fixture::new();
    let root = fixture.directory.path().join("sha256");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "--object-format=sha256", "-b", "main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(root.join("file"), "base\n").unwrap();
    git(&root, &["add", "file"]);
    git(&root, &["commit", "-m", "base"]);
    success(&root, &["start", "sha256", "--in-place"]);
    fs::write(root.join("file"), "change\n").unwrap();
    let result = success(&root, &["checkpoint", "-m", "change", "--", "file"]);
    assert_eq!(result["commit"].as_str().unwrap().len(), 64);
    success(&root, &["verify", "--", "git", "status", "--porcelain"]);
    success(&root, &["finish"]);
}

#[test]
fn conflict_and_submodule_fail_closed_on_writes() {
    let fixture = Fixture::new();
    git(&fixture.root, &["checkout", "-b", "other"]);
    fs::write(fixture.root.join("first.txt"), "other\n").unwrap();
    git(&fixture.root, &["commit", "-am", "other"]);
    git(&fixture.root, &["checkout", "main"]);
    fs::write(fixture.root.join("first.txt"), "main\n").unwrap();
    git(&fixture.root, &["commit", "-am", "main"]);
    let merge = Command::new("git")
        .args(["merge", "other"])
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(!merge.status.success());
    let before = git(&fixture.root, &["ls-files", "--stage", "-z"]);
    assert_eq!(success(&fixture.root, &["context"])["conflicts"], 1);
    assert!(
        !run(
            &fixture.root,
            &["checkpoint", "-m", "bad", "--", "first.txt"]
        )
        .status
        .success()
    );
    assert_eq!(before, git(&fixture.root, &["ls-files", "--stage", "-z"]));

    let other_fixture = Fixture::new();
    let clean = other_fixture.start();
    let head = String::from_utf8(git(&clean, &["rev-parse", "HEAD"])).unwrap();
    git(
        &clean,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},module", head.trim()),
        ],
    );
    assert!(
        !run(&clean, &["checkpoint", "-m", "module", "--", "first.txt"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn symlink_traversal_and_non_utf8_paths_fail_closed() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(fixture.directory.path(), fixture.root.join("outside")).unwrap();
    assert!(
        !run(
            &fixture.root,
            &["checkpoint", "-m", "bad", "--", "outside/file"]
        )
        .status
        .success()
    );
    let invalid = std::ffi::OsString::from_vec(b"invalid-\xff".to_vec());
    let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .arg("checkpoint")
        .arg("-m")
        .arg("bad")
        .arg("--")
        .arg(invalid)
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[cfg(unix)]
#[test]
fn checkpoint_coordinates_native_index_writers_and_other_aisc_mutations() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let hooks = fixture.directory.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let ready = fixture.directory.path().join("ready");
    let release = fixture.directory.path().join("release");
    fs::write(
        hooks.join("pre-commit"),
        format!(
            "#!/bin/sh\nprintf ready > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\n",
            ready.display(),
            release.display()
        ),
    )
    .unwrap();
    fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .args(["checkpoint", "-m", "concurrent", "--", "first.txt"])
        .current_dir(&fixture.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "hook did not signal readiness"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let other = Command::new("git")
        .args(["add", "second.txt"])
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(!other.status.success());
    assert!(
        !run(
            &fixture.root,
            &["checkpoint", "-m", "other", "--", "second.txt"]
        )
        .status
        .success()
    );
    fs::write(release, "go").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(&fixture.root, &["show", "HEAD:first.txt"]),
        b"changed\n"
    );
}

#[test]
fn hidden_index_flags_cannot_produce_false_clean_evidence() {
    let fixture = Fixture::new();
    git(
        &fixture.root,
        &["update-index", "--assume-unchanged", "first.txt"],
    );
    fs::write(fixture.root.join("first.txt"), "hidden dirty\n").unwrap();
    assert!(
        !run(&fixture.root, &["verify", "--", "true"])
            .status
            .success()
    );
    assert!(
        !run(
            &fixture.root,
            &["checkpoint", "-m", "hidden", "--", "first.txt"]
        )
        .status
        .success()
    );
}

#[cfg(unix)]
#[test]
fn evidence_hashes_the_executable_actually_selected_from_path() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let blocked = fixture.directory.path().join("blocked");
    let actual = fixture.directory.path().join("actual");
    fs::create_dir(&blocked).unwrap();
    fs::create_dir(&actual).unwrap();
    fs::write(blocked.join("probe"), "not executable").unwrap();
    fs::write(actual.join("probe"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(actual.join("probe"), fs::Permissions::from_mode(0o755)).unwrap();
    let original = std::env::var_os("PATH").unwrap();
    let path = std::env::join_paths(
        [blocked, actual.clone()]
            .into_iter()
            .chain(std::env::split_paths(&original)),
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(args)
            .current_dir(&fixture.root)
            .env("PATH", &path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap()
    };
    let result = invoke(&["verify", "--", "probe"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::write(actual.join("probe"), "#!/bin/sh\nexit 1\n").unwrap();
    assert!(!invoke(&["finish"]).status.success());
}

#[test]
fn context_preserves_rename_identity_and_zero_line_change_stats() {
    let fixture = Fixture::new();
    git(&fixture.root, &["mv", "first.txt", "renamed 雪\nfile.txt"]);
    let value = success(&fixture.root, &["context"]);
    assert_eq!(value["staged"], 1);
    assert_eq!(value["files"][0]["status"], "R ");
    assert_eq!(value["files"][0]["original_path"], "first.txt");
    assert_eq!(value["files"][0]["path"], "renamed 雪\nfile.txt");
    assert_eq!(value["diffstat"]["staged"]["files"][0]["added"], "0");
    assert_eq!(value["diffstat"]["staged"]["files"][0]["deleted"], "0");
    assert_eq!(
        value["diffstat"]["staged"]["files"][0]["original_path"],
        "first.txt"
    );
}

#[cfg(unix)]
#[test]
fn executable_without_shebang_never_uses_execvp_shell_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("probe"), "touch shell-fallback-ran\n").unwrap();
    fs::set_permissions(
        fixture.root.join("probe"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git(&fixture.root, &["add", "probe"]);
    git(&fixture.root, &["commit", "-m", "bare executable"]);
    assert!(
        !run(&fixture.root, &["verify", "--", "./probe"])
            .status
            .success()
    );
    assert!(!fixture.root.join("shell-fallback-ran").exists());
}

#[test]
fn cargo_rustup_proxy_preserves_requested_argument_zero() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n").unwrap();
    fs::write(
        fixture.root.join("lib.rs"),
        "#[test]\nfn succeeds() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    git(
        &fixture.root,
        &["add", "Cargo.toml", "Cargo.lock", "lib.rs"],
    );
    git(&fixture.root, &["commit", "-m", "rust fixture"]);
    let target = fixture.directory.path().join("target");
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(args)
            .current_dir(&fixture.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("RUSTC_WRAPPER", "")
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap()
    };
    let result = invoke(&[
        "verify",
        "--",
        "cargo",
        "+1.91.1",
        "test",
        "--offline",
        "--locked",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(invoke(&["finish"]).status.success());
}

#[test]
fn codex_tool_call_metadata_does_not_invalidate_separate_invocations() {
    let fixture = Fixture::new();
    let invoke = |args: &[&str], call_id: &str| {
        Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(args)
            .current_dir(&fixture.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("CODEX_TOOL_CALL_ID", call_id)
            .output()
            .unwrap()
    };
    let verified = invoke(&["verify", "--", "true"], "first-call");
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let finished = invoke(&["finish"], "second-call");
    assert!(
        finished.status.success(),
        "{}",
        String::from_utf8_lossy(&finished.stderr)
    );
}

#[test]
fn context_paths_and_stats_are_root_relative_from_subdirectories() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("subdirectory")).unwrap();
    fs::write(fixture.root.join("first.txt"), "changed\n").unwrap();
    git(&fixture.root, &["config", "diff.relative", "true"]);
    let value = success(&fixture.root.join("subdirectory"), &["context"]);
    assert_eq!(value["files"][0]["path"], "first.txt");
    assert_eq!(
        value["diffstat"]["unstaged"]["files"][0]["path"],
        "first.txt"
    );
}

#[cfg(unix)]
#[test]
fn context_rejects_changed_status_even_when_head_and_branch_stay_the_same() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.directory.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let marker = fixture.directory.path().join("marker");
    let capture = fixture.directory.path().join("status");
    let script = "#!/bin/sh\ncase \"$*\" in *' status '*) PATH=\"$AISC_ORIGINAL_PATH\" git \"$@\" > \"$AISC_STATUS_CAPTURE\" || exit $?\nif [ ! -e \"$AISC_RACE_MARKER\" ]; then printf changed > race-untracked; printf marker > \"$AISC_RACE_MARKER\"; fi\ncat \"$AISC_STATUS_CAPTURE\";; *) PATH=\"$AISC_ORIGINAL_PATH\" exec git \"$@\";; esac\n";
    fs::write(bin.join("git"), script).unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let original = std::env::var_os("PATH").unwrap();
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&original))).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .arg("context")
        .current_dir(&fixture.root)
        .env("PATH", path)
        .env("AISC_ORIGINAL_PATH", original)
        .env("AISC_STATUS_CAPTURE", capture)
        .env("AISC_RACE_MARKER", marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("changed"));
}

#[test]
fn alternate_index_preserves_gits_racy_stat_protection_for_same_size_edits() {
    let fixture = Fixture::new();
    git(&fixture.root, &["config", "core.trustctime", "false"]);
    let timestamp = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
    fs::File::open(fixture.root.join("first.txt"))
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    git(&fixture.root, &["add", "first.txt"]);
    fs::File::open(fixture.root.join(".git/index"))
        .unwrap()
        .set_modified(timestamp - std::time::Duration::from_secs(1))
        .unwrap();
    fs::write(fixture.root.join("first.txt"), "task\n").unwrap();
    fs::File::open(fixture.root.join("first.txt"))
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    success(
        &fixture.root,
        &["checkpoint", "-m", "same size", "--", "first.txt"],
    );
    assert_eq!(git(&fixture.root, &["show", "HEAD:first.txt"]), b"task\n");
}
