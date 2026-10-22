//! `start --path` registration, `finish` worktree removal and `start --sparse`.
mod support;

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{Fixture, Run, env_refs};

fn aisc_at(directory: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AISC_OUTPUT")
        .output()
        .unwrap();
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn git_at(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned()
}

fn repository() -> Fixture {
    Fixture::with_files(&[
        ("top.txt", "top\n"),
        ("src/lib.txt", "lib\n"),
        ("src/deep/inner.txt", "inner\n"),
        ("docs/guide.txt", "guide\n"),
        ("other/file.txt", "other\n"),
        (
            ".gitignore",
            ".env\nbuild/\n*.key\nsecret/\nlocal/\nnode_modules/\n",
        ),
    ])
}

/// Starts task `name` in a sibling worktree and returns its path.
fn start(fixture: &Fixture, name: &str, extra: &[&str]) -> PathBuf {
    let path = fixture.root.parent().unwrap().join(name);
    let mut args = vec!["start", name, "--path", path.to_str().unwrap()];
    args.extend_from_slice(extra);
    fixture.aisc(&args).ok();
    path
}

/// Commits one edit in the worktree and records verification evidence.
fn checkpoint_and_verify(path: &Path, file: &str) {
    fs::write(path.join(file), "task edit\n").unwrap();
    aisc_at(path, &["checkpoint", "-m", "task edit", "--", file]).ok();
    aisc_at(path, &["verify", "--", "true"]).ok();
}

fn registry(fixture: &Fixture, name: &str) -> PathBuf {
    fixture
        .root
        .join(".git/aisc/worktrees")
        .join(format!("{name}.json"))
}

fn listed(fixture: &Fixture, path: &Path) -> bool {
    fixture
        .git(&["worktree", "list", "--porcelain"])
        .lines()
        .any(|line| line == format!("worktree {}", path.display()))
}

#[test]
fn start_with_path_registers_the_worktree_under_the_common_dir() {
    let fixture = repository();
    let base = fixture.git(&["rev-parse", "HEAD"]);
    let path = start(&fixture, "reg", &[]);
    let entry: Value =
        serde_json::from_slice(&fs::read(registry(&fixture, "reg")).unwrap()).unwrap();
    assert_eq!(entry["name"], "reg");
    assert_eq!(entry["path"], path.to_str().unwrap());
    assert_eq!(entry["branch"], "aisc/reg");
    assert_eq!(entry["base"], base.as_str());
    assert_eq!(entry["base_branch"], "main");
    assert!(entry["created_at"].as_u64().unwrap() > 0);
    assert!(entry["sparse"].is_null());
    let command = entry["command"].as_array().unwrap();
    assert!(command.iter().any(|part| part == "start"), "{entry}");
    // In-place sessions create no worktree and no registry entry.
    fixture.aisc(&["start", "here", "--in-place"]).ok();
    assert!(!registry(&fixture, "here").exists());
}

#[test]
fn finish_removes_the_worktree_and_keeps_the_branch() {
    let fixture = repository();
    let path = start(&fixture, "done", &[]);
    checkpoint_and_verify(&path, "top.txt");
    let tip = git_at(&path, &["rev-parse", "HEAD"]);
    let result = aisc_at(&path, &["finish"]);
    result.ok();
    let value = result.json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap());
    assert!(value["freed_bytes"].as_u64().unwrap() > 0, "{value}");
    assert_eq!(value["main_checkout"], fixture.root.to_str().unwrap());
    // The caller's shell was inside the removed worktree, so it is told where to go.
    assert_eq!(value["cd"], fixture.root.to_str().unwrap());
    assert!(!path.exists());
    assert!(!listed(&fixture, &path));
    assert!(!registry(&fixture, "done").exists());
    assert_eq!(fixture.git(&["rev-parse", "refs/heads/aisc/done"]), tip);
    assert_eq!(
        fixture.tree_blob("aisc/done", "top.txt").unwrap(),
        "task edit\n"
    );
}

#[test]
fn finish_keep_leaves_the_worktree_and_its_registration() {
    let fixture = repository();
    let path = start(&fixture, "kept", &[]);
    checkpoint_and_verify(&path, "top.txt");
    let value = aisc_at(&path, &["finish", "--keep"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    assert!(path.join("top.txt").exists());
    assert!(listed(&fixture, &path));
    assert!(registry(&fixture, "kept").exists());
}

#[test]
fn dirty_or_untracked_worktree_fails_finish_and_nothing_is_removed() {
    let fixture = repository();
    let path = start(&fixture, "dirty", &[]);
    checkpoint_and_verify(&path, "top.txt");
    fs::write(path.join("scratch.txt"), "untracked\n").unwrap();
    assert_ne!(aisc_at(&path, &["finish"]).code, 0);
    assert!(path.join("scratch.txt").exists());
    fs::remove_file(path.join("scratch.txt")).unwrap();
    fs::write(path.join("src/lib.txt"), "modified\n").unwrap();
    assert_ne!(aisc_at(&path, &["finish"]).code, 0);
    assert_eq!(
        fs::read_to_string(path.join("src/lib.txt")).unwrap(),
        "modified\n"
    );
    assert!(listed(&fixture, &path));
    assert!(registry(&fixture, "dirty").exists());
}

#[test]
fn finish_keeps_a_worktree_that_aisc_did_not_register() {
    let fixture = repository();
    let path = start(&fixture, "unknown", &[]);
    fs::remove_file(registry(&fixture, "unknown")).unwrap();
    checkpoint_and_verify(&path, "top.txt");
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    assert!(
        value["kept_reason"]
            .as_str()
            .unwrap()
            .contains("not registered"),
        "{value}"
    );
    assert!(path.join("top.txt").exists());
}

#[test]
fn finish_keeps_a_locked_worktree() {
    let fixture = repository();
    let path = start(&fixture, "locked", &[]);
    checkpoint_and_verify(&path, "top.txt");
    fixture.git(&["worktree", "lock", path.to_str().unwrap()]);
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    assert!(
        value["kept_reason"].as_str().unwrap().contains("locked"),
        "{value}"
    );
    assert!(path.join("top.txt").exists());
}

#[cfg(unix)]
#[test]
fn failed_removal_keeps_branch_registration_and_reports_the_leftover() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = repository();
    let path = start(&fixture, "stuck", &[]);
    checkpoint_and_verify(&path, "top.txt");
    // A read-only directory makes Git's recursive delete fail partway through.
    let blocked = path.join("src/deep");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();
    let result = aisc_at(&path, &["finish"]);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
    // The finish itself succeeded; only the cleanup failed and is reported.
    let value = result.ok().json();
    assert_eq!(value["verified"], true);
    assert!(value["removed_worktree"].is_null(), "{value}");
    let warning = value["warning"].as_str().unwrap();
    assert!(warning.contains("partially removed"), "{warning}");
    assert!(warning.contains(path.to_str().unwrap()), "{warning}");
    assert!(warning.contains("delete by hand"), "{warning}");
    assert!(!warning.contains("it is kept"), "{warning}");
    assert_eq!(value["partially_removed"], path.to_str().unwrap());
    // Git drops its metadata even when the delete fails, so the registry entry is what keeps
    // the leftover directory visible; the branch keeps every commit.
    assert!(registry(&fixture, "stuck").exists());
    assert!(path.join("src/deep/inner.txt").exists());
    assert_eq!(
        fixture.tree_blob("aisc/stuck", "top.txt").unwrap(),
        "task edit\n"
    );
    let report = fixture
        .aisc(&["gc", "--json", "--min-age", "0", "--apply"])
        .ok()
        .json();
    let entry = &report["worktrees"][0];
    assert_eq!(entry["name"], "stuck");
    assert_eq!(entry["state"], "leftover", "{report}");
    assert_eq!(entry["action"], "keep", "{report}");
    assert!(path.join("src/deep/inner.txt").exists());
}

#[test]
fn sparse_start_materializes_only_the_cone() {
    let fixture = repository();
    let path = start(&fixture, "narrow", &["--sparse", "src"]);
    assert!(path.join("top.txt").exists());
    assert!(path.join("src/lib.txt").exists());
    assert!(path.join("src/deep/inner.txt").exists());
    assert!(!path.join("docs").exists());
    assert!(!path.join("other").exists());
    let entry: Value =
        serde_json::from_slice(&fs::read(registry(&fixture, "narrow")).unwrap()).unwrap();
    assert_eq!(entry["sparse"], serde_json::json!(["src"]));
    // The main checkout stays a full checkout.
    assert!(fixture.path("docs/guide.txt").exists());
    assert_ne!(
        fixture.git_bytes(&["config", "--get", "core.sparseCheckout"]),
        b"true\n"
    );
}

#[test]
fn sparse_worktree_refuses_paths_outside_the_cone_and_supports_the_lifecycle() {
    let fixture = repository();
    let path = start(&fixture, "cone", &["--sparse", "src"]);
    fs::create_dir_all(path.join("docs")).unwrap();
    fs::write(path.join("docs/guide.txt"), "outside\n").unwrap();
    let refused = aisc_at(&path, &["checkpoint", "-m", "x", "--", "docs/guide.txt"]);
    assert_ne!(refused.code, 0);
    assert!(refused.stderr.contains("sparse"), "{}", refused.stderr);
    fs::remove_dir_all(path.join("docs")).unwrap();
    fs::write(path.join("src/lib.txt"), "inside\n").unwrap();
    aisc_at(&path, &["checkpoint", "-m", "inside", "--", "src/lib.txt"]).ok();
    aisc_at(&path, &["verify", "--", "true"]).ok();
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap());
    assert_eq!(
        fixture.tree_blob("aisc/cone", "src/lib.txt").unwrap(),
        "inside\n"
    );
    assert_eq!(
        fixture.tree_blob("aisc/cone", "docs/guide.txt").unwrap(),
        "guide\n"
    );
}

#[test]
fn sparse_worktree_still_refuses_hidden_flags_inside_the_cone() {
    let fixture = repository();
    let path = start(&fixture, "hidden", &["--sparse", "src"]);
    git_at(&path, &["update-index", "--skip-worktree", "src/lib.txt"]);
    fs::write(path.join("src/lib.txt"), "hidden dirty\n").unwrap();
    assert_ne!(aisc_at(&path, &["verify", "--", "true"]).code, 0);
}

#[test]
fn sparse_start_rejects_unsafe_directories_without_creating_anything() {
    let fixture = repository();
    for directory in ["../outside", ".git", "/abs", "-flag", ""] {
        let path = fixture.root.parent().unwrap().join("bad");
        let run = fixture.aisc(&[
            "start",
            "bad",
            "--path",
            path.to_str().unwrap(),
            "--sparse",
            directory,
        ]);
        assert_ne!(run.code, 0, "{directory:?}");
        assert!(!path.exists(), "{directory:?}");
        assert!(!fixture.git_ok(&["rev-parse", "--verify", "refs/heads/aisc/bad"]));
        assert!(!registry(&fixture, "bad").exists());
    }
}

#[test]
fn failed_sparse_setup_rolls_back_the_new_worktree() {
    let fixture = repository();
    let env = fixture.fake_git("sparse-checkout", "exit 1");
    let path = fixture.root.parent().unwrap().join("rollback");
    let run = fixture.aisc_env(
        &[
            "start",
            "rollback",
            "--path",
            path.to_str().unwrap(),
            "--sparse",
            "src",
        ],
        &env_refs(&env),
    );
    assert_ne!(run.code, 0);
    assert!(!path.exists());
    assert!(!listed(&fixture, &path));
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "refs/heads/aisc/rollback"]));
    assert!(!registry(&fixture, "rollback").exists());
}

#[test]
fn sparse_start_notes_when_it_enables_per_worktree_config() {
    let fixture = repository();
    let first = fixture.root.parent().unwrap().join("first");
    let value = fixture
        .aisc(&[
            "start",
            "first",
            "--path",
            first.to_str().unwrap(),
            "--sparse",
            "src",
        ])
        .ok()
        .json();
    let note = value["note"].as_str().unwrap();
    assert!(note.contains("extensions.worktreeConfig"), "{note}");
    assert_eq!(
        fixture.git(&["config", "--get", "extensions.worktreeConfig"]),
        "true"
    );
    // Already enabled: nothing new to say.
    let second = fixture.root.parent().unwrap().join("second");
    let value = fixture
        .aisc(&[
            "start",
            "second",
            "--path",
            second.to_str().unwrap(),
            "--sparse",
            "src",
        ])
        .ok()
        .json();
    assert!(value["note"].is_null(), "{value}");
    let human = fixture.root.parent().unwrap().join("third");
    let text = fixture
        .aisc(&[
            "--human",
            "start",
            "third",
            "--path",
            human.to_str().unwrap(),
        ])
        .ok()
        .stdout
        .clone();
    assert!(!text.contains("worktreeConfig"), "{text}");
}

#[test]
fn finish_reports_ignored_build_output_it_deletes() {
    let fixture = repository();
    let path = start(&fixture, "built", &[]);
    checkpoint_and_verify(&path, "top.txt");
    fs::create_dir_all(path.join("build")).unwrap();
    fs::write(path.join("build/app.bin"), vec![1_u8; 2048]).unwrap();
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
    assert_eq!(value["ignored_files"], 1);
    assert_eq!(value["ignored_bytes"], 2048);
    assert!(!path.exists());
}

#[test]
fn finish_keeps_a_worktree_with_ignored_secret_like_files() {
    let fixture = repository();
    let path = start(&fixture, "secrets", &[]);
    checkpoint_and_verify(&path, "top.txt");
    fs::write(path.join(".env"), "TOKEN=x\n").unwrap();
    fs::write(path.join("deploy.key"), "k\n").unwrap();
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    let reason = value["kept_reason"].as_str().unwrap();
    assert!(
        reason.starts_with("ignored secret-like files: "),
        "{reason}"
    );
    assert!(reason.contains(".env") && reason.contains("deploy.key"));
    assert!(reason.contains("--delete-ignored"), "{reason}");
    assert_eq!(value["ignored_files"], 2);
    assert!(path.join(".env").exists());
    let value = aisc_at(&path, &["finish", "--delete-ignored"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
    assert!(!path.exists());
}

#[test]
fn finish_keeps_a_worktree_with_a_nested_repository_in_ignored_files() {
    let fixture = repository();
    let path = start(&fixture, "nested", &[]);
    checkpoint_and_verify(&path, "top.txt");
    let dependency = path.join("build/dep");
    fs::create_dir_all(&dependency).unwrap();
    git_at(&dependency, &["init", "-q", "-b", "main"]);
    git_at(&dependency, &["config", "user.name", "Nested"]);
    git_at(
        &dependency,
        &["config", "user.email", "nested@example.invalid"],
    );
    fs::write(dependency.join("local.txt"), "only here\n").unwrap();
    git_at(&dependency, &["add", "."]);
    git_at(&dependency, &["commit", "-qm", "local only"]);
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    let reason = value["kept_reason"].as_str().unwrap();
    assert!(
        reason.starts_with("nested git repository: build/dep"),
        "{reason}"
    );
    assert!(reason.contains("--delete-ignored"), "{reason}");
    assert!(dependency.join("local.txt").exists());
    let value = aisc_at(&path, &["finish", "--delete-ignored"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
    assert!(!path.exists());
}

/// Starts a worktree task with one checkpointed, verified edit and returns its path.
fn verified(fixture: &Fixture, name: &str) -> PathBuf {
    let path = start(fixture, name, &[]);
    checkpoint_and_verify(&path, "top.txt");
    path
}

fn kept_reason(path: &Path, args: &[&str]) -> String {
    let mut all = vec!["finish"];
    all.extend_from_slice(args);
    let value = aisc_at(path, &all).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    value["kept_reason"].as_str().unwrap().to_owned()
}

#[test]
fn ignored_files_outside_regenerable_dirs_keep_the_worktree() {
    let fixture = repository();
    let path = verified(&fixture, "unknown");
    fs::create_dir_all(path.join("local")).unwrap();
    fs::write(path.join("local/todo.txt"), "mine\n").unwrap();
    let reason = kept_reason(&path, &[]);
    assert_eq!(
        reason,
        "ignored files not known to be regenerable: local/todo.txt (pass --delete-ignored to remove anyway)"
    );
    let value = aisc_at(&path, &["finish", "--delete-ignored"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
}

#[test]
fn bare_repositories_and_secret_dirs_inside_build_output_keep_the_worktree() {
    let fixture = repository();
    let path = verified(&fixture, "bare");
    let bare = path.join("build/remote.git");
    fs::create_dir_all(&bare).unwrap();
    git_at(&bare, &["init", "-q", "--bare"]);
    let reason = kept_reason(&path, &[]);
    assert!(
        reason.starts_with("nested git repository: build/remote.git"),
        "{reason}"
    );

    let path = verified(&fixture, "secretdirs");
    for file in [
        "build/secrets/db.yml",
        "build/.aws/config",
        "build/gcloud/application_default_credentials.json",
        "build/service-account.json",
    ] {
        fs::create_dir_all(path.join(file).parent().unwrap()).unwrap();
        fs::write(path.join(file), "x\n").unwrap();
    }
    let reason = kept_reason(&path, &[]);
    assert!(
        reason.starts_with("ignored secret-like files: "),
        "{reason}"
    );
    assert!(reason.contains("and 1 more"), "{reason}");
}

#[test]
fn node_modules_with_git_dependencies_is_regenerable() {
    let fixture = repository();
    let path = verified(&fixture, "npm");
    for package in 0..30 {
        let directory = path.join(format!("node_modules/pkg{package}/lib"));
        fs::create_dir_all(&directory).unwrap();
        for file in 0..20 {
            fs::write(directory.join(format!("f{file}.js")), "x").unwrap();
        }
    }
    // A git dependency checked out with its own .git directory.
    fs::create_dir_all(path.join("node_modules/gitdep/.git/objects")).unwrap();
    fs::write(path.join("node_modules/gitdep/.git/HEAD"), "ref\n").unwrap();
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
    assert_eq!(value["ignored_scan_truncated"], false);
    assert!(value["ignored_files"].as_u64().unwrap() >= 600);
}

#[test]
fn a_truncated_ignored_scan_fails_closed() {
    let fixture = repository();
    let path = verified(&fixture, "huge");
    fs::create_dir_all(path.join("build")).unwrap();
    for file in 0..20 {
        fs::write(path.join(format!("build/f{file}.o")), "x").unwrap();
    }
    // Verification evidence binds the environment, so verify and finish share the limit.
    let limited = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(args)
            .current_dir(&path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("AISC_IGNORED_SCAN_LIMIT", "5")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    limited(&["verify", "--", "true"]);
    let value = limited(&["finish"]);
    assert!(value["removed_worktree"].is_null(), "{value}");
    assert_eq!(value["ignored_scan_truncated"], true);
    let reason = value["kept_reason"].as_str().unwrap();
    assert!(
        reason.starts_with("ignored file scan truncated"),
        "{reason}"
    );
}

#[test]
fn secret_like_ignored_files_are_matched_case_insensitively() {
    let fixture = repository();
    let path = start(&fixture, "cased", &[]);
    checkpoint_and_verify(&path, "top.txt");
    fs::create_dir_all(path.join("secret")).unwrap();
    for name in [".ENV", "prod.env", "Vault.KDBX"] {
        fs::write(path.join("secret").join(name), "x\n").unwrap();
    }
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert!(value["removed_worktree"].is_null(), "{value}");
    let reason = value["kept_reason"].as_str().unwrap();
    for name in ["secret/.ENV", "secret/prod.env", "secret/Vault.KDBX"] {
        assert!(reason.contains(name), "{reason}");
    }
    assert_eq!(value["ignored_files"], 3);
}

#[test]
fn finish_on_a_detached_head_saves_the_commit_before_removal() {
    let fixture = repository();
    let path = start(&fixture, "loose", &[]);
    git_at(&path, &["checkout", "-q", "--detach"]);
    checkpoint_and_verify(&path, "top.txt");
    let commit = git_at(&path, &["rev-parse", "HEAD"]);
    let value = aisc_at(&path, &["finish"]).ok().json();
    assert_eq!(value["removed_worktree"], path.to_str().unwrap(), "{value}");
    assert_eq!(value["rescue_ref"], "refs/aisc/rescue/loose");
    assert_eq!(
        fixture.git(&["rev-parse", "refs/aisc/rescue/loose"]),
        commit
    );
}
