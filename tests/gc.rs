//! `aisc gc`: only registered, verified, clean and finished-or-merged worktrees are removed.
mod support;

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{Fixture, Run};

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

fn repository() -> Fixture {
    Fixture::with_files(&[
        ("top.txt", "top\n"),
        ("src/lib.txt", "lib\n"),
        (".gitignore", ".env\nbuild/\n*.pem\n"),
    ])
}

fn start(fixture: &Fixture, name: &str) -> PathBuf {
    let path = fixture.root.parent().unwrap().join(name);
    fixture
        .aisc(&["start", name, "--path", path.to_str().unwrap()])
        .ok();
    path
}

/// Commits an edit, verifies and finishes while keeping the worktree.
fn finish_kept(path: &Path) {
    fs::write(path.join("top.txt"), "task edit\n").unwrap();
    aisc_at(path, &["checkpoint", "-m", "edit", "--", "top.txt"]).ok();
    aisc_at(path, &["verify", "--", "true"]).ok();
    aisc_at(path, &["finish", "--keep"]).ok();
}

fn gc(fixture: &Fixture, args: &[&str]) -> Value {
    let mut all = vec!["gc", "--json", "--min-age", "0"];
    all.extend_from_slice(args);
    fixture.aisc(&all).ok().json()
}

fn entry<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {report}"))
}

fn listed(fixture: &Fixture, path: &Path) -> bool {
    fixture
        .git(&["worktree", "list", "--porcelain"])
        .lines()
        .any(|line| line == format!("worktree {}", path.display()))
}

#[test]
fn dry_run_lists_size_age_and_state_without_removing() {
    let fixture = repository();
    let finished = start(&fixture, "finished");
    finish_kept(&finished);
    let dirty = start(&fixture, "dirty");
    fs::write(dirty.join("scratch.txt"), "untracked\n").unwrap();
    let report = gc(&fixture, &[]);
    assert_eq!(report["applied"], false);
    let done = entry(&report, "finished");
    assert_eq!(done["state"], "clean");
    assert_eq!(done["finished"], true);
    assert_eq!(done["action"], "remove");
    assert!(done["bytes"].as_u64().unwrap() > 0);
    assert!(done["allocated_bytes"].as_u64().unwrap() > 0);
    assert!(done["age_seconds"].is_u64());
    let kept = entry(&report, "dirty");
    assert_eq!(kept["state"], "dirty");
    assert_eq!(kept["action"], "keep");
    assert_eq!(report["reclaimable"], 1);
    assert_eq!(report["reclaimable_bytes"], done["allocated_bytes"]);
    assert!(finished.join("top.txt").exists());
    assert!(dirty.join("scratch.txt").exists());
    let text = fixture.aisc(&["gc", "--min-age", "0"]).ok().stdout.clone();
    assert!(text.starts_with("aisc worktrees: 2 ("), "{text}");
    assert!(text.contains("reclaimable 1 ("), "{text}");
    assert!(text.contains("dry run; pass --apply"), "{text}");
    assert!(
        text.contains("finished") && text.contains("remove"),
        "{text}"
    );
}

#[test]
fn apply_removes_only_clean_finished_or_merged_worktrees() {
    let fixture = repository();
    let finished = start(&fixture, "finished");
    finish_kept(&finished);
    // No commits beyond the base: merged.
    let merged = start(&fixture, "merged");
    // Commits that are neither finished nor merged.
    let working = start(&fixture, "working");
    fs::write(working.join("top.txt"), "unfinished\n").unwrap();
    aisc_at(&working, &["checkpoint", "-m", "wip", "--", "top.txt"]).ok();
    let dirty = start(&fixture, "dirty");
    fs::write(dirty.join("src/lib.txt"), "dirty\n").unwrap();
    let locked = start(&fixture, "locked");
    fixture.git(&["worktree", "lock", locked.to_str().unwrap()]);
    let foreign = fixture.root.parent().unwrap().join("foreign");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "foreign",
        foreign.to_str().unwrap(),
    ]);

    let report = gc(&fixture, &["--apply"]);
    assert_eq!(report["applied"], true);
    assert_eq!(entry(&report, "finished")["action"], "removed");
    assert_eq!(entry(&report, "merged")["action"], "removed");
    for name in ["working", "dirty", "locked"] {
        assert_eq!(entry(&report, name)["action"], "keep", "{name}: {report}");
    }
    assert_eq!(report["foreign_ignored"], 1);
    assert_eq!(report["removed"], 2);
    assert!(report["freed_bytes"].as_u64().unwrap() > 0);

    assert!(!finished.exists() && !merged.exists());
    assert!(!listed(&fixture, &finished) && !listed(&fixture, &merged));
    assert!(working.exists() && dirty.join("src/lib.txt").exists() && locked.exists());
    assert!(foreign.join("top.txt").exists() && listed(&fixture, &foreign));
    // Branches survive removal, so no commit is lost.
    for branch in ["aisc/finished", "aisc/merged", "aisc/working"] {
        assert!(fixture.git_ok(&["rev-parse", "--verify", &format!("refs/heads/{branch}")]));
    }
    assert_eq!(
        fixture.tree_blob("aisc/finished", "top.txt").unwrap(),
        "task edit\n"
    );
    let registry = fixture.root.join(".git/aisc/worktrees");
    assert!(!registry.join("finished.json").exists());
    assert!(registry.join("working.json").exists());
}

#[test]
fn foreign_worktrees_are_never_listed_or_touched() {
    let fixture = repository();
    let foreign = fixture.root.parent().unwrap().join("foreign");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "foreign",
        foreign.to_str().unwrap(),
    ]);
    // A worktree that pretends to be AISC's but has no registry entry stays foreign.
    let session = fixture
        .root
        .join(".git/worktrees/foreign/aisc-session.json");
    fs::write(
        &session,
        r#"{"name":"foreign","base":"x","mode":"worktree","finished":true}"#,
    )
    .unwrap();
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(report["worktrees"].as_array().unwrap().len(), 0);
    assert_eq!(report["foreign_ignored"], 1);
    assert!(foreign.join("top.txt").exists());
    // A registry entry pointing at a foreign worktree without the session marker is unverified.
    let path = foreign.to_str().unwrap();
    fs::remove_file(&session).unwrap();
    fs::create_dir_all(fixture.root.join(".git/aisc/worktrees")).unwrap();
    fs::write(
        fixture.root.join(".git/aisc/worktrees/foreign.json"),
        format!(
            r#"{{"name":"foreign","path":"{path}","branch":"foreign","base":"x","base_branch":"main","created_at":1,"aisc_version":"0","command":[],"sparse":null}}"#
        ),
    )
    .unwrap();
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "foreign")["action"], "keep", "{report}");
    assert!(foreign.join("top.txt").exists() && listed(&fixture, &foreign));
}

#[test]
fn missing_directory_is_pruned_but_foreign_missing_metadata_is_left() {
    let fixture = repository();
    let gone = start(&fixture, "gone");
    fs::remove_dir_all(&gone).unwrap();
    let foreign = fixture.root.parent().unwrap().join("foreign-gone");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "foreign-gone",
        foreign.to_str().unwrap(),
    ]);
    fs::remove_dir_all(&foreign).unwrap();
    let dry = gc(&fixture, &[]);
    assert_eq!(entry(&dry, "gone")["state"], "missing");
    assert_eq!(entry(&dry, "gone")["action"], "remove");
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "gone")["action"], "removed");
    assert!(!listed(&fixture, &gone));
    assert!(!fixture.root.join(".git/aisc/worktrees/gone.json").exists());
    assert!(fixture.git_ok(&["rev-parse", "--verify", "refs/heads/aisc/gone"]));
    // Git metadata of a worktree AISC did not create is not pruned.
    assert!(listed(&fixture, &foreign));
}

#[test]
fn stale_registry_entry_without_a_worktree_is_forgotten() {
    let fixture = repository();
    let gone = start(&fixture, "pruned");
    fs::remove_dir_all(&gone).unwrap();
    fixture.git(&["worktree", "prune"]);
    let dry = gc(&fixture, &[]);
    assert_eq!(entry(&dry, "pruned")["state"], "stale");
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "pruned")["action"], "removed");
    assert!(
        !fixture
            .root
            .join(".git/aisc/worktrees/pruned.json")
            .exists()
    );
}

#[test]
fn min_age_protects_recently_active_worktrees() {
    let fixture = repository();
    let recent = start(&fixture, "recent");
    finish_kept(&recent);
    let report = fixture.aisc(&["gc", "--json", "--apply"]).ok().json();
    let entry = entry(&report, "recent");
    assert_eq!(entry["action"], "keep");
    assert!(
        entry["reason"].as_str().unwrap().contains("min-age"),
        "{report}"
    );
    assert!(recent.join("top.txt").exists());
}

#[test]
fn current_and_busy_worktrees_are_kept() {
    let fixture = repository();
    let current = start(&fixture, "current");
    finish_kept(&current);
    let report = aisc_at(&current, &["gc", "--json", "--min-age", "0", "--apply"])
        .ok()
        .json();
    assert_eq!(entry(&report, "current")["action"], "keep", "{report}");
    assert!(current.join("top.txt").exists());

    let busy = start(&fixture, "busy");
    finish_kept(&busy);
    let lock = fixture.root.join(".git/worktrees/busy/aisc-operation.lock");
    fs::write(&lock, "").unwrap();
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "busy")["action"], "keep", "{report}");
    assert!(busy.join("top.txt").exists() && lock.exists());
}

#[test]
fn reported_size_matches_bytes_written() {
    let fixture = repository();
    let path = start(&fixture, "sized");
    let before = entry(&gc(&fixture, &[]), "sized")["bytes"]
        .as_u64()
        .unwrap();
    fs::write(path.join("payload.bin"), vec![7_u8; 256 * 1024]).unwrap();
    let after = entry(&gc(&fixture, &[]), "sized")["bytes"]
        .as_u64()
        .unwrap();
    // Directory entries grow by a few bytes on some filesystems.
    let added = after - before;
    assert!((256 * 1024..256 * 1024 + 4096).contains(&added), "{added}");
}

#[test]
fn a_registry_entry_naming_the_main_checkout_never_removes_it() {
    let fixture = repository();
    let root = fixture.root.to_str().unwrap();
    fs::create_dir_all(fixture.root.join(".git/aisc/worktrees")).unwrap();
    fs::write(
        fixture.root.join(".git/aisc/worktrees/main.json"),
        format!(
            r#"{{"name":"main","path":"{root}","branch":"main","base":"x","base_branch":"main","created_at":1,"aisc_version":"0","command":[],"sparse":null}}"#
        ),
    )
    .unwrap();
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "main")["state"], "invalid", "{report}");
    assert_eq!(entry(&report, "main")["action"], "keep");
    assert!(fixture.path("top.txt").exists());
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

fn write_registry(fixture: &Fixture, file: &str, name: &str, path: &str) {
    let directory = fixture.root.join(".git/aisc/worktrees");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(file),
        serde_json::json!({
            "name": name, "path": path, "branch": "aisc/x", "base": "x",
            "base_branch": "main", "created_at": 1, "aisc_version": "0",
            "command": [], "sparse": null
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn missing_directory_respects_min_age_and_rescues_an_unreachable_head() {
    let fixture = repository();
    let path = start(&fixture, "moved");
    git_at(&path, &["checkout", "-q", "--detach"]);
    fs::write(path.join("top.txt"), "only here\n").unwrap();
    git_at(&path, &["commit", "-qam", "detached work"]);
    let commit = git_at(&path, &["rev-parse", "HEAD"]);
    let moved = fixture.root.parent().unwrap().join("moved-elsewhere");
    fs::rename(&path, &moved).unwrap();
    // Default min-age: a recently active worktree is kept even when its directory is gone.
    let report = fixture.aisc(&["gc", "--json", "--apply"]).ok().json();
    assert_eq!(entry(&report, "moved")["state"], "missing");
    assert_eq!(entry(&report, "moved")["action"], "keep", "{report}");
    assert!(listed(&fixture, &path));
    // Past min-age it is pruned, but only after its HEAD is saved in a rescue ref.
    let report = gc(&fixture, &["--apply"]);
    let row = entry(&report, "moved");
    assert_eq!(row["action"], "removed", "{report}");
    assert_eq!(row["rescue_ref"], "refs/aisc/rescue/moved");
    assert_eq!(
        fixture.git(&["rev-parse", "refs/aisc/rescue/moved"]),
        commit
    );
    fixture.git(&["reflog", "expire", "--all", "--expire=now"]);
    fixture.git(&["gc", "-q", "--prune=now"]);
    assert!(fixture.git_ok(&["cat-file", "-e", &commit]));
}

#[test]
fn registry_entries_with_unsafe_names_are_reported_and_never_acted_on() {
    let fixture = repository();
    let victim = fixture.root.parent().unwrap().join("victim.json");
    fs::write(&victim, "keep me").unwrap();
    let missing = fixture.root.parent().unwrap().join("nowhere");
    let missing = missing.to_str().unwrap();
    write_registry(&fixture, "evil.json", "../../../../victim", missing);
    write_registry(&fixture, "slash.json", "a/b", missing);
    write_registry(&fixture, "other.json", "mismatch", missing);
    let report = gc(&fixture, &["--apply"]);
    let rows = report["worktrees"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{report}");
    for row in rows {
        assert_eq!(row["state"], "invalid", "{report}");
        assert_eq!(row["action"], "keep", "{report}");
    }
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    assert!(fixture.root.join(".git/aisc/worktrees/evil.json").exists());
}

#[test]
fn registry_paths_that_cover_the_repository_or_home_are_refused_without_walking() {
    let fixture = repository();
    let parent = fixture.root.parent().unwrap().to_str().unwrap().to_owned();
    let home = std::env::var("HOME").unwrap();
    for (name, path) in [
        ("root", "/"),
        ("home", home.as_str()),
        ("above", parent.as_str()),
    ] {
        write_registry(&fixture, &format!("{name}.json"), name, path);
    }
    let report = gc(&fixture, &["--apply"]);
    for name in ["root", "home", "above"] {
        let row = entry(&report, name);
        assert_eq!(row["state"], "invalid", "{report}");
        assert_eq!(row["action"], "keep");
        assert_eq!(row["bytes"], 0, "{name} was walked: {report}");
    }
    assert!(fixture.path("top.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_worktree_directory_replaced_by_a_symlink_is_unverified() {
    let fixture = repository();
    let path = start(&fixture, "swapped");
    finish_kept(&path);
    let foreign = fixture.root.parent().unwrap().join("foreign");
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "foreign",
        foreign.to_str().unwrap(),
    ]);
    fs::remove_dir_all(&path).unwrap();
    std::os::unix::fs::symlink(&foreign, &path).unwrap();
    let report = gc(&fixture, &[]);
    let row = entry(&report, "swapped");
    assert_eq!(row["state"], "unverified", "{report}");
    assert_eq!(row["action"], "keep");
    let report = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&report, "swapped")["action"], "keep", "{report}");
    assert!(foreign.join("top.txt").exists() && listed(&fixture, &foreign));
}

#[test]
fn ignored_files_are_reported_and_secret_like_ones_block_removal() {
    let fixture = repository();
    let build = start(&fixture, "build");
    finish_kept(&build);
    fs::create_dir_all(build.join("build")).unwrap();
    fs::write(build.join("build/out.o"), vec![0_u8; 3000]).unwrap();
    let secret = start(&fixture, "secret");
    finish_kept(&secret);
    fs::write(secret.join(".env"), "TOKEN=x\n").unwrap();
    fs::write(secret.join("server.pem"), "key\n").unwrap();

    let dry = gc(&fixture, &[]);
    let row = entry(&dry, "build");
    assert_eq!(row["action"], "remove", "{dry}");
    assert_eq!(row["ignored_files"], 1);
    assert_eq!(row["ignored_bytes"], 3000);
    let row = entry(&dry, "secret");
    assert_eq!(row["action"], "keep", "{dry}");
    assert_eq!(row["ignored_files"], 2);
    let reason = row["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("ignored secret-like files: "),
        "{reason}"
    );
    assert!(
        reason.contains(".env") && reason.contains("server.pem"),
        "{reason}"
    );
    let text = fixture.aisc(&["gc", "--min-age", "0"]).ok().stdout.clone();
    assert!(text.contains("1 ignored file"), "{text}");

    let applied = gc(&fixture, &["--apply"]);
    assert_eq!(entry(&applied, "build")["action"], "removed");
    assert_eq!(entry(&applied, "secret")["action"], "keep");
    assert!(secret.join(".env").exists());
    let forced = gc(&fixture, &["--apply", "--delete-ignored-secrets"]);
    assert_eq!(entry(&forced, "secret")["action"], "removed", "{forced}");
    assert!(!secret.exists());
}
