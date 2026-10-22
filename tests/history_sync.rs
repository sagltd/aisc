//! Sync tests. Pilot 6 and pilot 11 fixtures mirror the content of vcbench pilots 6
//! (update-dirty-branch) and 11 (dual-source-sync) from github.com/gitbutlerapp/version-control-bench
//! (MIT): scripts/lib/pilot6-content.mjs and scripts/lib/scenario-sync.mjs.
mod support;

use support::Fixture;

const README: &str = "# Notify Service\n\nSmall TypeScript service used by the update-dirty-branch benchmark pilot.\n";
const NOTIFY: &str = "export type Channel = \"email\" | \"sms\" | \"push\";\n\nexport const activeChannels: Channel[] = [\"email\"];\n\nexport function notifyTargets(user: string): string[] {\n  return activeChannels.map((channel) => `${channel}:${user}`);\n}\n";
const CONFIG: &str = "export interface NotifyConfig {\n  region: string;\n  retryLimit: number;\n  batchSize: number;\n}\n\nexport const config: NotifyConfig = {\n  region: \"local\",\n  retryLimit: 2,\n  batchSize: 20,\n};\n";
const DIRTY_README: &str = "# Notify Service\n\nSmall TypeScript service used by the update-dirty-branch benchmark pilot.\n\nRetry tuning notes are being collected on this branch before release.\n";
const CHECKLIST: &str = "# Rollout Checklist\n\n- Verify relay quotas before enabling the higher retry limit.\n- Keep this checklist out of version control.\n";

fn commit_as(fixture: &Fixture, message: &str) {
    fixture.git(&["add", "-A"]);
    fixture.git(&["commit", "-m", message]);
}

fn pilot6() -> Fixture {
    let fixture = Fixture::with_files(&[
        ("README.md", README),
        ("src/notify.ts", NOTIFY),
        ("src/config.ts", CONFIG),
    ]);
    fixture.git(&["switch", "-c", "notify-retry"]);
    fixture.write(
        "src/notify.ts",
        &NOTIFY.replace("[\"email\"]", "[\"email\", \"sms\"]"),
    );
    commit_as(&fixture, "Add sms channel");
    fixture.write(
        "src/config.ts",
        &CONFIG.replace("retryLimit: 2", "retryLimit: 4"),
    );
    commit_as(&fixture, "Tune retry budget");
    fixture.git(&["switch", "main"]);
    fixture.write(
        "src/notify.ts",
        &NOTIFY.replace("[\"email\"]", "[\"email\", \"push\"]"),
    );
    commit_as(&fixture, "Add push channel");
    fixture.write(
        "src/config.ts",
        &CONFIG.replace("retryLimit: 2", "retryLimit: 5"),
    );
    commit_as(&fixture, "Raise retry limit");
    fixture.git(&["switch", "notify-retry"]);
    fixture.write("README.md", DIRTY_README);
    fixture.write("notes/rollout-checklist.md", CHECKLIST);
    fixture
}

fn all(run: &support::Run) -> String {
    format!("{}{}", run.stdout, run.stderr)
}

fn assert_no_git_residue(fixture: &Fixture) {
    for marker in [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
    ] {
        assert!(!fixture.exists(&format!(".git/{marker}")), "{marker}");
    }
    assert!(fixture.git(&["stash", "list"]).is_empty());
    assert!(fixture.git(&["ls-files", "-u"]).is_empty());
    assert!(!fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn pilot6_dirty_sync_takes_two_aisc_calls() {
    let fixture = pilot6();
    let main = fixture.git(&["rev-parse", "main"]);
    let old_tip = fixture.git(&["rev-parse", "HEAD"]);
    let paused = fixture.aisc(&["sync"]);
    paused.ok();
    assert!(
        paused
            .stdout
            .starts_with("CONFLICTS: sync notify-retry onto main"),
        "{}",
        paused.stdout
    );
    assert!(paused.stdout.contains("src/notify.ts in "));
    assert!(paused.stdout.contains("src/config.ts in "));
    assert!(paused.stdout.contains("<<<<<<< main"));
    assert!(paused.stdout.contains("||||||| base"));
    assert!(paused.stdout.contains(">>>>>>> "));
    assert!(paused.stdout.len() < 2_500, "{} bytes", paused.stdout.len());
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), old_tip);
    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    assert!(fixture.read("src/notify.ts").contains("<<<<<<< main"));
    assert!(fixture.read("src/config.ts").contains("||||||| base"));
    assert_eq!(fixture.read("README.md"), DIRTY_README);
    assert!(fixture.exists(".git/aisc/sync.json"));
    let again = fixture.aisc(&["sync"]);
    assert_eq!(again.code, 1, "second sync while paused: {}", all(&again));

    // Continuing with markers left is refused and keeps the pause.
    let refused = fixture.aisc(&["sync", "--continue"]);
    assert_eq!(refused.code, 1);
    assert!(
        all(&refused).contains("conflict markers"),
        "{}",
        all(&refused)
    );
    assert!(fixture.exists(".git/aisc/sync.json"));

    fixture.write(
        "src/notify.ts",
        &NOTIFY.replace("[\"email\"]", "[\"email\", \"sms\", \"push\"]"),
    );
    fixture.write(
        "src/config.ts",
        &CONFIG.replace("retryLimit: 2", "retryLimit: 4"),
    );
    let done = fixture.aisc(&["sync", "--continue"]);
    done.ok();
    assert!(
        done.stdout.contains("synced notify-retry onto main"),
        "{}",
        done.stdout
    );

    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    assert_eq!(
        fixture.subjects("main..notify-retry"),
        ["Tune retry budget", "Add sms channel"]
    );
    assert_eq!(fixture.git(&["rev-parse", "HEAD~2"]), main);
    let notify = fixture.tree_blob("HEAD", "src/notify.ts").unwrap();
    assert!(notify.contains("[\"email\", \"sms\", \"push\"]") && !notify.contains("<<<<"));
    assert!(
        fixture
            .tree_blob("HEAD", "src/config.ts")
            .unwrap()
            .contains("retryLimit: 4")
    );
    assert_eq!(fixture.tree_blob("HEAD", "README.md").unwrap(), README);
    assert_eq!(fixture.read("README.md"), DIRTY_README);
    assert_eq!(fixture.read("notes/rollout-checklist.md"), CHECKLIST);
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "notes/rollout-checklist.md"
    );
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(fixture.git(&["diff", "--name-only"]), "README.md");
    assert_eq!(fixture.read("src/notify.ts"), notify);
    assert_no_git_residue(&fixture);
    let log = fixture.aisc(&["log"]);
    log.ok();
    assert!(!log.stdout.contains("sync paused"));
}

#[test]
fn prefer_branch_resolves_without_pausing() {
    let fixture = pilot6();
    let run = fixture.aisc(&[
        "sync",
        "--prefer-branch",
        "src/notify.ts",
        "--prefer-branch",
        "src/config.ts",
    ]);
    run.ok();
    assert!(run.stdout.contains("synced notify-retry"), "{}", run.stdout);
    assert!(
        fixture
            .tree_blob("HEAD", "src/notify.ts")
            .unwrap()
            .contains("[\"email\", \"sms\"]")
    );
    assert!(
        fixture
            .tree_blob("HEAD", "src/config.ts")
            .unwrap()
            .contains("retryLimit: 4")
    );
    assert_eq!(fixture.read("README.md"), DIRTY_README);
    assert_no_git_residue(&fixture);
}

#[test]
fn prefer_onto_keeps_upstream_and_drops_emptied_commits() {
    let fixture = pilot6();
    let run = fixture.aisc(&[
        "sync",
        "--prefer-onto",
        "src/notify.ts",
        "--prefer-onto",
        "src/config.ts",
    ]);
    run.ok();
    assert!(run.stdout.contains("2 dropped"), "{}", run.stdout);
    assert!(fixture.subjects("main..notify-retry").is_empty());
    assert!(
        fixture
            .tree_blob("HEAD", "src/config.ts")
            .unwrap()
            .contains("retryLimit: 5")
    );
}

#[test]
fn abort_restores_files_byte_for_byte() {
    let fixture = pilot6();
    // A dirty edit inside a conflicting file sends its markers to a side file.
    fixture.write(
        "src/config.ts",
        &CONFIG.replace("batchSize: 20", "batchSize: 25"),
    );
    let before = fixture.snapshot();
    let paused = fixture.aisc(&["sync"]);
    paused.ok();
    assert!(paused.stdout.contains("aisc-conflict"), "{}", paused.stdout);
    assert!(fixture.exists("src/config.ts.aisc-conflict"));
    assert!(fixture.read("src/notify.ts").contains("<<<<<<<"));
    fixture.aisc(&["sync", "--abort"]).ok();
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn dirty_edit_conflicting_with_incoming_aborts_before_any_change() {
    let fixture = Fixture::with_files(&[("a.txt", "one\ntwo\nthree\n"), ("b.txt", "x\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("b.txt", "y\n");
    commit_as(&fixture, "change b");
    fixture.git(&["switch", "main"]);
    fixture.write("a.txt", "one\nTWO upstream\nthree\n");
    commit_as(&fixture, "change a upstream");
    fixture.git(&["switch", "topic"]);
    fixture.write("a.txt", "one\nTWO local\nthree\n");
    fixture.write("scratch.md", "keep\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("a.txt"));
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn other_verbs_are_refused_while_paused() {
    let fixture = pilot6();
    fixture.aisc(&["sync"]).ok();
    for args in [
        vec!["commit", "-m", "x", "--all"],
        vec!["switch", "main"],
        vec!["land", "main"],
        vec!["undo"],
    ] {
        let run = fixture.aisc(&args);
        assert_eq!(run.code, 1, "{args:?}: {}", all(&run));
        assert!(
            all(&run).contains("sync is paused"),
            "{args:?}: {}",
            all(&run)
        );
    }
    let log = fixture.aisc(&["log"]);
    log.ok();
    assert!(
        log.stdout.contains("sync paused: 2 conflicts"),
        "{}",
        log.stdout
    );
}

#[test]
fn branch_moved_during_pause_is_stale() {
    let fixture = pilot6();
    fixture.aisc(&["sync"]).ok();
    fixture.write("src/notify.ts", NOTIFY);
    fixture.write("src/config.ts", CONFIG);
    fixture.git(&["update-ref", "refs/heads/notify-retry", "HEAD~1"]);
    let before = fixture.git(&["rev-parse", "refs/heads/notify-retry"]);
    let run = fixture.aisc(&["sync", "--continue"]);
    assert_eq!(run.code, 4, "{}", all(&run));
    assert_eq!(
        fixture.git(&["rev-parse", "refs/heads/notify-retry"]),
        before
    );
    assert!(fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn commit_already_upstream_is_dropped() {
    let fixture = Fixture::with_files(&[("a.txt", "1\n"), ("b.txt", "1\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("b.txt", "2\n");
    commit_as(&fixture, "bump b");
    fixture.write("a.txt", "2\n");
    commit_as(&fixture, "bump a");
    fixture.git(&["switch", "main"]);
    fixture.write("b.txt", "2\n");
    commit_as(&fixture, "bump b upstream");
    fixture.git(&["switch", "topic"]);
    let run = fixture.aisc(&["sync"]);
    run.ok();
    assert!(
        run.stdout.contains("1 dropped") && run.stdout.contains("bump b"),
        "{}",
        run.stdout
    );
    assert_eq!(fixture.subjects("main..topic"), ["bump a"]);
    let again = fixture.aisc(&["sync"]);
    again.ok();
    assert!(again.stdout.contains("up to date"));
}

#[test]
fn sync_is_undoable() {
    let fixture = pilot6();
    let tip = fixture.git(&["rev-parse", "HEAD"]);
    fixture
        .aisc(&[
            "sync",
            "--prefer-branch",
            "src/notify.ts",
            "--prefer-branch",
            "src/config.ts",
        ])
        .ok();
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), tip);
    assert_eq!(fixture.read("README.md"), DIRTY_README);
}

#[test]
fn pilot11_fetch_and_sync_onto_remote_main() {
    let fixture = Fixture::with_files(&[
        ("README.md", "Service example.\n"),
        ("config/runtime.json", "{\"timeout\":10}\n"),
        ("src/formats.ts", "export const formats = [];\n"),
    ]);
    fixture.git(&["switch", "-c", "feature-service"]);
    fixture.write("src/formats.ts", "export const formats = [\"document\"];\n");
    commit_as(&fixture, "Support document format");
    let remote = fixture.directory.path().join("origin.git");
    let remote = remote.to_str().unwrap().to_owned();
    fixture.git(&["init", "--bare", "-b", "main", &remote]);
    fixture.git(&["remote", "add", "origin", &remote]);
    fixture.git(&["push", "-u", "origin", "main", "feature-service"]);
    fixture.write("src/retry.ts", "export const retryLimit = 3;\n");
    commit_as(&fixture, "Add local retry setting");
    // Another clone advances origin/main.
    let publisher = fixture.directory.path().join("publisher");
    let publisher = publisher.to_str().unwrap().to_owned();
    fixture.git(&["clone", "-q", &remote, &publisher]);
    let in_publisher = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&publisher)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Main Author")
            .env("GIT_AUTHOR_EMAIL", "main@example.test")
            .env("GIT_COMMITTER_NAME", "Main Author")
            .env("GIT_COMMITTER_EMAIL", "main@example.test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    std::fs::write(
        format!("{publisher}/config/runtime.json"),
        "{\"timeout\":20}\n",
    )
    .unwrap();
    in_publisher(&["commit", "-qam", "Increase default timeout"]);
    in_publisher(&["push", "-q", "origin", "main"]);
    let remote_refs = || {
        std::process::Command::new("git")
            .args(["for-each-ref"])
            .current_dir(&remote)
            .output()
            .unwrap()
            .stdout
    };
    let remote_before = remote_refs();
    fixture.git(&["switch", "-c", "other-topic", "main"]);
    fixture.write("notes/other-topic.md", "Unrelated local experiment.\n");
    commit_as(&fixture, "Document local experiment");
    fixture.git(&["switch", "feature-service"]);
    fixture.write("README.md", "Service example.\nUnfinished release notes.\n");
    let stale = fixture.git(&["rev-parse", "origin/main"]);
    let run = fixture.aisc(&["sync", "origin/main", "--fetch"]);
    run.ok();
    assert_ne!(fixture.git(&["rev-parse", "origin/main"]), stale, "fetched");
    assert_eq!(
        fixture.git(&["rev-parse", "HEAD~2"]),
        fixture.git(&["rev-parse", "origin/main"])
    );
    assert_eq!(
        fixture.subjects("origin/main..HEAD"),
        ["Add local retry setting", "Support document format"]
    );
    assert_eq!(
        fixture.read("README.md"),
        "Service example.\nUnfinished release notes.\n"
    );
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(fixture.git(&["diff", "--name-only"]), "README.md");
    assert_eq!(fixture.read("config/runtime.json"), "{\"timeout\":20}\n");
    assert_eq!(
        fixture.subjects("main..other-topic"),
        ["Document local experiment"]
    );
    assert_no_git_residue(&fixture);
    assert_eq!(remote_refs(), remote_before, "origin untouched");
    assert_eq!(fixture.git(&["log", "-1", "--format=%an"]), "Fixture");
    assert_eq!(fixture.calls(), 1);
}

#[test]
fn branch_option_reports_conflicts_without_state() {
    let fixture = pilot6();
    fixture.git(&["checkout", "--", "README.md"]);
    std::fs::remove_dir_all(fixture.path("notes")).unwrap();
    fixture.git(&["switch", "main"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync", "--branch", "notify-retry"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("src/notify.ts"));
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.exists(".git/aisc/sync.json"));
}

fn collision_fixture() -> Fixture {
    let fixture = Fixture::with_files(&[("a.txt", "1\n"), ("b.txt", "1\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("b.txt", "2\n");
    commit_as(&fixture, "change b");
    fixture.git(&["switch", "main"]);
    fixture.write("d/x", "incoming\n");
    commit_as(&fixture, "add d/x upstream");
    fixture.git(&["switch", "topic"]);
    fixture
}

#[test]
fn untracked_file_where_incoming_needs_a_directory_refuses_before_any_change() {
    let fixture = collision_fixture();
    fixture.write("d", "i am a file\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("d/x"), "{}", all(&run));
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn symlinked_parent_refuses_before_any_change() {
    let fixture = collision_fixture();
    let elsewhere = fixture.directory.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fixture.path("d")).unwrap();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert_eq!(fixture.snapshot(), before);
    assert!(!elsewhere.join("x").exists());
}

#[test]
fn continue_refuses_new_collision_and_keeps_the_pause() {
    let fixture = pilot6();
    fixture.git(&["switch", "main"]);
    fixture.write("d/x", "incoming\n");
    fixture.git(&["add", "d/x"]);
    fixture.git(&["commit", "-m", "add d/x"]);
    fixture.git(&["switch", "notify-retry"]);
    fixture.aisc(&["sync"]).ok();
    fixture.write("src/notify.ts", NOTIFY);
    fixture.write("src/config.ts", CONFIG);
    fixture.write("d", "i am a file\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync", "--continue"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    let after = fixture.snapshot();
    // Only the sync pin (an aisc-private ref) may be refreshed; branches, HEAD, index, files stay.
    let public = |refs: &str| {
        refs.lines()
            .filter(|line| !line.starts_with("refs/aisc/sync/"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(public(&after.refs), public(&before.refs));
    assert_eq!(after.head, before.head);
    assert_eq!(after.index, before.index);
    assert_eq!(after.files, before.files);
    assert!(fixture.exists(".git/aisc/sync.json"));
}

#[test]
fn pause_state_survives_gc_and_is_unpinned_afterwards() {
    let fixture = pilot6();
    let before = fixture.snapshot();
    fixture.aisc(&["sync"]).ok();
    let pins = fixture.git(&["for-each-ref", "refs/aisc/sync"]);
    assert!(pins.contains("refs/aisc/sync/main"), "{pins:?}");
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now", "-q"]);
    fixture.aisc(&["sync", "--abort"]).ok();
    assert!(fixture.git(&["for-each-ref", "refs/aisc/sync"]).is_empty());
    assert_eq!(fixture.snapshot().files, before.files);
}

#[test]
fn abort_refuses_when_the_branch_moved() {
    let fixture = pilot6();
    fixture.aisc(&["sync"]).ok();
    fixture.git(&["update-ref", "refs/heads/notify-retry", "HEAD~1"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["sync", "--abort"]);
    assert_eq!(run.code, 4, "{}", all(&run));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn leftover_equals_line_is_refused() {
    let fixture = pilot6();
    fixture.aisc(&["sync"]).ok();
    fixture.write("src/notify.ts", &format!("{NOTIFY}=======\n"));
    fixture.write("src/config.ts", CONFIG);
    let run = fixture.aisc(&["sync", "--continue"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("conflict markers"));
}

#[test]
fn continue_removes_side_files_and_keeps_dirty_edits() {
    let fixture = pilot6();
    fixture.write(
        "src/config.ts",
        &CONFIG
            .replace("retryLimit: 2", "retryLimit: 4")
            .replace("batchSize: 20", "batchSize: 25"),
    );
    fixture.aisc(&["sync"]).ok();
    assert!(fixture.exists("src/config.ts.aisc-conflict"));
    fixture.write("src/notify.ts", NOTIFY);
    fixture.write(
        "src/config.ts.aisc-conflict",
        &CONFIG.replace("retryLimit: 2", "retryLimit: 4"),
    );
    fixture.aisc(&["sync", "--continue"]).ok();
    assert!(!fixture.exists("src/config.ts.aisc-conflict"));
    let config = fixture.read("src/config.ts");
    assert!(config.contains("batchSize: 25") && config.contains("retryLimit: 4"));
    assert_no_git_residue(&fixture);
}

#[test]
fn partial_apply_clears_the_pause_so_undo_works() {
    let fixture = pilot6();
    fixture.aisc(&["sync"]).ok();
    fixture.write("src/notify.ts", NOTIFY);
    fixture.write("src/config.ts", CONFIG);
    let tip = fixture.git(&["rev-parse", "HEAD"]);
    // A fake git makes src unwritable right when the reference transaction runs.
    let bin = fixture.directory.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *update-ref*--stdin*) if [ ! -e '{m}' ]; then : > '{m}'; chmod a-w '{r}/src'; fi;; esac\nexec '{g}' \"$@\"\n",
        m = fixture.directory.path().join("mark").display(),
        r = fixture.root.display(),
        g = support::real_git()
    );
    let path = bin.join("git");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let existing = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{existing}", bin.display());
    let run = fixture.aisc_env(&["sync", "--continue"], &[("PATH", &new_path)]);
    std::fs::set_permissions(fixture.path("src"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("partially applied"), "{}", all(&run));
    assert!(
        !fixture.exists(".git/aisc/sync.json"),
        "stale pause cleared"
    );
    assert!(fixture.git(&["for-each-ref", "refs/aisc/sync"]).is_empty());
    let abort = fixture.aisc(&["sync", "--abort"]);
    assert_eq!(abort.code, 1, "no pause to abort: {}", all(&abort));
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), tip);
}

#[test]
fn case_only_rename_ends_with_the_new_name_on_disk() {
    let fixture = Fixture::with_files(&[("Readme.md", "docs\n"), ("b.txt", "1\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("b.txt", "2\n");
    commit_as(&fixture, "change b");
    fixture.git(&["switch", "main"]);
    fixture.git(&["mv", "Readme.md", "README.md"]);
    fixture.git(&["commit", "-m", "rename readme"]);
    fixture.git(&["switch", "topic"]);
    fixture.aisc(&["sync"]).ok();
    let names: Vec<String> = std::fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"README.md".to_owned()), "{names:?}");
    assert!(!names.contains(&"Readme.md".to_owned()), "{names:?}");
    assert_eq!(fixture.read("README.md"), "docs\n");
}

#[test]
fn sync_also_takes_new_commits_pushed_to_the_branch_upstream() {
    let fixture = Fixture::with_files(&[
        ("README.md", "Service example.\n"),
        ("config/runtime.json", "{\"timeout\":10}\n"),
        ("src/formats.ts", "export const formats = [];\n"),
    ]);
    fixture.git(&["switch", "-c", "feature-service"]);
    fixture.write("src/formats.ts", "export const formats = [\"document\"];\n");
    commit_as(&fixture, "Support document format");
    let remote = fixture.directory.path().join("origin.git");
    let remote = remote.to_str().unwrap().to_owned();
    fixture.git(&["init", "--bare", "-b", "main", &remote]);
    fixture.git(&["remote", "add", "origin", &remote]);
    fixture.git(&["push", "-u", "origin", "main", "feature-service"]);
    fixture.write("src/retry.ts", "export const retryLimit = 3;\n");
    commit_as(&fixture, "Add local retry setting");
    // Another clone advances origin/main and pushes to origin/feature-service.
    let publisher = fixture.directory.path().join("publisher");
    let publisher = publisher.to_str().unwrap().to_owned();
    fixture.git(&["clone", "-q", &remote, &publisher]);
    let in_publisher = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&publisher)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Main Author")
            .env("GIT_AUTHOR_EMAIL", "main@example.test")
            .env("GIT_COMMITTER_NAME", "Main Author")
            .env("GIT_COMMITTER_EMAIL", "main@example.test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    std::fs::write(
        format!("{publisher}/config/runtime.json"),
        "{\"timeout\":20}\n",
    )
    .unwrap();
    in_publisher(&["commit", "-qam", "Increase default timeout"]);
    in_publisher(&["push", "-q", "origin", "main"]);
    in_publisher(&["switch", "-q", "feature-service"]);
    std::fs::create_dir_all(format!("{publisher}/.agent")).unwrap();
    std::fs::write(format!("{publisher}/.agent/environment.json"), "{}\n").unwrap();
    in_publisher(&["add", ".agent/environment.json"]);
    in_publisher(&["commit", "-qm", "Add agent environment"]);
    in_publisher(&["push", "-q", "origin", "feature-service"]);
    fixture.write("README.md", "Service example.\nUnfinished release notes.\n");

    let run = fixture.aisc(&["sync", "--fetch"]);
    run.ok();
    assert!(
        run.stdout.contains("with origin/feature-service"),
        "{}",
        run.stdout
    );
    assert_eq!(
        fixture.subjects("origin/main..HEAD"),
        [
            "Add local retry setting",
            "Add agent environment",
            "Support document format"
        ]
    );
    assert_eq!(fixture.read("config/runtime.json"), "{\"timeout\":20}\n");
    assert_eq!(
        fixture.read("README.md"),
        "Service example.\nUnfinished release notes.\n"
    );
    assert_no_git_residue(&fixture);
    assert_eq!(fixture.calls(), 1);
    // Undo restores the branch exactly.
    let synced = fixture.git(&["rev-parse", "HEAD"]);
    fixture.aisc(&["undo"]).ok();
    assert_ne!(fixture.git(&["rev-parse", "HEAD"]), synced);
    assert_eq!(
        fixture.subjects("main..HEAD"),
        ["Add local retry setting", "Support document format"]
    );
}
