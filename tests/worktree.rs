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
    assert!(
        value["warning"].as_str().unwrap().contains("worktree"),
        "{value}"
    );
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
