//! Commit verb tests. The pilot-1 fixture mirrors the shape of vcbench pilot 1 (selective commit)
//! from github.com/gitbutlerapp/version-control-bench (MIT): a validation hunk and a separate
//! logging hunk in one file, other files to commit, leftovers to keep, and an untracked note.
mod support;

use std::os::unix::fs::PermissionsExt;
use support::{Fixture, hunk_id};

fn handler(validation: bool, logging: bool) -> String {
    let mut text = String::from("import { LeadPayload } from \"./lead\";\n\n");
    text.push_str("export function handleLead(payload: LeadPayload): Response {\n");
    if validation {
        text.push_str("  const checked = validateLeadInput(payload);\n");
        text.push_str("  if (!checked.ok) {\n    return reject(checked.reason);\n  }\n");
    }
    text.push_str("  return respond(normalize(payload));\n}\n\n");
    for n in 1..=14 {
        text.push_str(&format!(
            "export function helper{n}(): number {{\n  return {n};\n}}\n\n"
        ));
    }
    text.push_str("export function createLead(payload: LeadPayload): Lead {\n");
    text.push_str("  const lead = build(payload);\n");
    if logging {
        text.push_str("  logLeadCreation(lead);\n");
    }
    text.push_str("  return lead;\n}\n");
    text
}

fn pilot1() -> Fixture {
    let fixture = Fixture::with_files(&[
        ("README.md", "# Lead intake\n\nA small service.\n"),
        ("src/handler.ts", &handler(false, false)),
        (
            "tests/handler.test.ts",
            "test(\"handles leads\", () => {});\n",
        ),
        ("src/config.ts", "export const retries = 3;\n"),
    ]);
    fixture.write(
        "README.md",
        "# Lead intake\n\nA small service.\n\nInput is validated before handling.\n",
    );
    fixture.write("src/handler.ts", &handler(true, true));
    fixture.write(
        "tests/handler.test.ts",
        "test(\"handles leads\", () => {});\ntest(\"rejects invalid leads\", () => {});\n",
    );
    fixture.write("src/config.ts", "export const retries = 5;\n");
    fixture.write("notes/debug-log.md", "scratch notes\n");
    fixture
}

fn assert_pilot1_final_state(fixture: &Fixture, main_before: &str) {
    // The branch exists, is checked out, and holds exactly the three targets.
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "input-validation"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "main"]),
        main_before,
        "main untouched"
    );
    assert_eq!(
        fixture.subjects("main..input-validation"),
        ["Add input validation"]
    );
    let mut files = fixture.commit_files("HEAD");
    files.sort();
    assert_eq!(
        files,
        ["README.md", "src/handler.ts", "tests/handler.test.ts"]
    );
    // Validation is committed; logging is not, and is still in the worktree.
    let committed = fixture.tree_blob("HEAD", "src/handler.ts").unwrap();
    assert!(committed.contains("validateLeadInput") && !committed.contains("logLeadCreation"));
    assert!(fixture.read("src/handler.ts").contains("logLeadCreation"));
    // Leftovers stay uncommitted, the note stays untracked, nothing is staged by the tool.
    assert_eq!(
        fixture.tree_blob("HEAD", "src/config.ts").unwrap(),
        "export const retries = 3;\n"
    );
    assert_eq!(fixture.read("src/config.ts"), "export const retries = 5;\n");
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "notes/debug-log.md"
    );
    let leftover = fixture.git(&["diff", "HEAD", "-U0"]);
    assert!(!leftover.contains("validateLeadInput") && !leftover.contains("Input is validated"));
    assert!(leftover.contains("logLeadCreation") && leftover.contains("retries = 5"));
    assert!(fixture.git(&["stash", "list"]).is_empty());
    for marker in [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
    ] {
        assert!(!fixture.exists(&format!(".git/{marker}")));
    }
    assert!(fixture.git(&["ls-files", "-u"]).is_empty());
    assert!(
        fixture.git(&["diff", "--cached", "--name-only"]).is_empty(),
        "no leftover staged state"
    );
}

#[test]
fn pilot1_selective_commit_takes_two_aisc_calls_with_compact_output() {
    let fixture = pilot1();
    let main_before = fixture.git(&["rev-parse", "main"]);
    let log = fixture.aisc(&["log", "--json"]);
    log.ok();
    assert!(
        log.stdout.len() < 2_500,
        "log stays compact: {} bytes",
        log.stdout.len()
    );
    let log = log.json();
    let validation = hunk_id(&log, "src/handler.ts", "validateLeadInput");
    let commit = fixture.aisc(&[
        "commit",
        "-b",
        "input-validation",
        "-m",
        "Add input validation",
        "README.md",
        "tests/handler.test.ts",
        &validation,
    ]);
    commit.ok();
    assert_eq!(fixture.calls(), 2);
    assert!(
        commit.stdout.len() < 1_500,
        "mutation output is compact: {}",
        commit.stdout
    );
    let first = commit.stdout.lines().next().unwrap();
    assert!(
        first.starts_with("committed ")
            && first
                .contains("\"Add input validation\" on new branch input-validation (3 files); op "),
        "{first}"
    );
    assert!(commit.stdout.contains("input-validation: 1 commit on main"));
    assert!(
        commit
            .stdout
            .contains("uncommitted: notes/debug-log.md src/config.ts src/handler.ts"),
        "{}",
        commit.stdout
    );
    assert_pilot1_final_state(&fixture, &main_before);
}

#[test]
fn pilot1_content_selector_needs_one_call() {
    let fixture = pilot1();
    let main_before = fixture.git(&["rev-parse", "main"]);
    let commit = fixture.aisc(&[
        "commit",
        "-b",
        "input-validation",
        "-m",
        "Add input validation",
        "README.md",
        "tests/handler.test.ts",
        "src/handler.ts~validateLeadInput",
    ]);
    commit.ok();
    assert_eq!(fixture.calls(), 1);
    assert_pilot1_final_state(&fixture, &main_before);
}

#[test]
fn quiet_prints_only_the_result_line_and_json_describes_the_commit() {
    let fixture = pilot1();
    let quiet = fixture.aisc(&["commit", "-q", "-m", "Config", "src/config.ts"]);
    quiet.ok();
    assert_eq!(quiet.stdout.lines().count(), 1, "{}", quiet.stdout);
    let json = fixture.aisc(&["--json", "commit", "-m", "Readme", "README.md"]);
    json.ok();
    let value = json.json();
    assert_eq!(value["verb"], "commit");
    assert_eq!(value["branch"], "main");
    assert_eq!(value["committed"][0]["subject"], "Readme");
    assert_eq!(value["moved_refs"][0]["ref"], "refs/heads/main");
    assert!(
        value["uncommitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "src/handler.ts")
    );
    assert!(value["op"].as_str().unwrap().len() >= 4);
}

#[test]
fn all_respects_ignores_and_commits_untracked_files() {
    let fixture = Fixture::with_files(&[(".gitignore", "target/\n*.log\n"), ("a.txt", "a\n")]);
    fixture.write("a.txt", "a\nmore\n");
    fixture.write("src/new.rs", "fn main() {}\n");
    fixture.write("target/out.bin", "junk\n");
    fixture.write("run.log", "noise\n");
    let run = fixture.aisc(&[
        "commit",
        "-b",
        "app-checkpoint",
        "-m",
        "Checkpoint app",
        "--all",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    let mut files = fixture.commit_files("HEAD");
    files.sort();
    assert_eq!(files, ["a.txt", "src/new.rs"]);
    assert!(
        fixture.git(&["status", "--porcelain"]).is_empty(),
        "clean after checkpoint"
    );
    assert!(fixture.exists("target/out.bin") && fixture.exists("run.log"));
}

#[test]
fn into_stack_builds_cumulative_trees_one_commit_per_group() {
    let fixture = Fixture::with_files(&[
        (
            "f.txt",
            &(1..=20)
                .map(|n| format!("line{n:02}\n"))
                .collect::<String>(),
        ),
        ("g.txt", "g\n"),
    ]);
    let edited = fixture
        .read("f.txt")
        .replace("line02\n", "line02 first topic\n")
        .replace("line19\n", "line19 second topic\n");
    fixture.write("f.txt", &edited);
    fixture.write("g.txt", "g\ng more\n");
    fixture.write("h.txt", "brand new\n");
    let main_before = fixture.git(&["rev-parse", "main"]);
    let run = fixture.aisc(&[
        "commit",
        "--into",
        "First topic",
        "f.txt~first topic",
        "--into",
        "Second topic",
        "f.txt~second topic",
        "g.txt",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert!(
        run.stdout
            .lines()
            .next()
            .unwrap()
            .starts_with("committed 2 commits "),
        "{}",
        run.stdout
    );
    assert_eq!(
        fixture.subjects("HEAD~2..HEAD"),
        ["Second topic", "First topic"]
    );
    assert_eq!(fixture.git(&["rev-parse", "HEAD~2"]), main_before);
    let first = fixture.tree_blob("HEAD~1", "f.txt").unwrap();
    assert!(first.contains("first topic") && !first.contains("second topic"));
    assert_eq!(fixture.tree_blob("HEAD~1", "g.txt").unwrap(), "g\n");
    let second = fixture.tree_blob("HEAD", "f.txt").unwrap();
    assert!(second.contains("first topic") && second.contains("second topic"));
    assert_eq!(fixture.tree_blob("HEAD", "g.txt").unwrap(), "g\ng more\n");
    assert_eq!(fixture.git(&["ls-files", "--others"]), "h.txt");
    // One undo reverses the whole stack.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "main"]), main_before);
}

#[test]
fn deleted_binary_mode_and_untracked_changes_commit_by_path() {
    let fixture = Fixture::with_files(&[
        ("gone.txt", "bye\n"),
        ("script.sh", "#!/bin/sh\necho hi\n"),
        ("data.bin", "AAAA"),
    ]);
    std::fs::remove_file(fixture.path("gone.txt")).unwrap();
    std::fs::set_permissions(
        fixture.path("script.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture.write_bytes("data.bin", &[0, 1, 2, 3, 0, 255]);
    fixture.write_bytes("fresh.bin", &[9, 0, 9]);
    fixture.write("untracked dir/new file.txt", "hello\n");
    let log = fixture.aisc(&["log"]);
    log.ok();
    for expected in [
        "gone.txt (deleted)",
        "script.sh (mode)",
        "data.bin (binary)",
        "fresh.bin (untracked)",
        "untracked dir/new file.txt (untracked) +1",
    ] {
        assert!(
            log.stdout.contains(expected),
            "missing {expected:?} in\n{}",
            log.stdout
        );
    }
    let run = fixture.aisc(&[
        "commit",
        "-m",
        "Mixed",
        "gone.txt",
        "script.sh",
        "data.bin",
        "fresh.bin",
        "untracked dir/new file.txt",
    ]);
    run.ok();
    assert!(fixture.tree_blob("HEAD", "gone.txt").is_none());
    assert!(
        fixture
            .git(&["ls-tree", "HEAD", "script.sh"])
            .starts_with("100755")
    );
    assert_eq!(
        fixture.git_bytes(&["show", "HEAD:data.bin"]),
        vec![0, 1, 2, 3, 0, 255]
    );
    assert_eq!(
        fixture.git_bytes(&["show", "HEAD:fresh.bin"]),
        vec![9, 0, 9]
    );
    assert_eq!(
        fixture
            .tree_blob("HEAD", "untracked dir/new file.txt")
            .unwrap(),
        "hello\n"
    );
    assert!(
        fixture.git(&["status", "--porcelain"]).is_empty(),
        "{}",
        fixture.git(&["status", "--porcelain"])
    );
}

#[test]
fn symlinks_commit_as_links() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    std::os::unix::fs::symlink("a.txt", fixture.path("link")).unwrap();
    fixture.aisc(&["commit", "-m", "Link", "link"]).ok();
    assert!(
        fixture
            .git(&["ls-tree", "HEAD", "link"])
            .starts_with("120000")
    );
    assert_eq!(fixture.git(&["show", "HEAD:link"]), "a.txt");
}

#[test]
fn existing_branch_name_fails_without_changes() {
    let fixture = pilot1();
    fixture.git(&["branch", "taken"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["commit", "-b", "taken", "-m", "x", "README.md"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("already exists"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    let bad = fixture.aisc(&["commit", "-b", "bad..name", "-m", "x", "README.md"]);
    assert_eq!(bad.code, 1);
    assert_eq!(before, fixture.snapshot());
    let branch_flag = fixture.aisc(&["commit", "--branch", "main", "-m", "x", "README.md"]);
    assert_eq!(branch_flag.code, 1);
    assert!(
        branch_flag.stderr.contains("hint: use -b"),
        "{}",
        branch_flag.stderr
    );
    let empty = fixture.aisc(&["commit", "-m", "x"]);
    assert_eq!(empty.code, 1);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn detached_head_advances_head_and_undo_restores_it() {
    let fixture = pilot1();
    fixture.git(&["checkout", "--detach"]);
    let start = fixture.git(&["rev-parse", "HEAD"]);
    fixture
        .aisc(&["commit", "-m", "Detached work", "src/config.ts"])
        .ok();
    assert_ne!(fixture.git(&["rev-parse", "HEAD"]), start);
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
    assert_eq!(
        fixture.git(&["rev-parse", "main"]),
        start,
        "branches untouched"
    );
    let log = fixture.aisc(&["log"]);
    assert!(
        log.stdout.starts_with("HEAD (detached): 1 commit on main"),
        "{}",
        log.stdout
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), start);
}

#[test]
fn staged_state_is_preserved_and_index_mtime_kept() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    fixture.write("a.txt", "a\nstaged\n");
    fixture.git(&["add", "a.txt"]);
    fixture.write("a.txt", "a\nstaged\nunstaged\n");
    fixture.write("b.txt", "b\nchanged\n");
    fixture.write("c.txt", "c\nchanged\n");
    // Settle the index (plain git may refresh it), then pin its mtime to a fixed time in the past
    // so the comparison never depends on wall-clock progression. Worktree files are newer than
    // the index, i.e. racily dirty: exactly the case the mtime rule protects.
    fixture.git(&["status", "--porcelain"]);
    let index_before = std::time::UNIX_EPOCH + std::time::Duration::new(1_600_000_000, 123_456_789);
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.path(".git/index"))
        .unwrap()
        .set_modified(index_before)
        .unwrap();
    let staged_before = fixture.git(&["show", ":a.txt"]);
    // Hard-link the installed index just after aisc installs it (before its own post-operation
    // `git diff`, which may legitimately refresh and replace the file).
    let env = fixture.fake_git("for-each-ref", "ln .git/index .git/index.snap");
    fixture
        .aisc_env(&["commit", "-m", "B", "b.txt"], &support::env_refs(&env))
        .ok();
    let index_after = std::fs::metadata(fixture.path(".git/index.snap"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        fixture.git(&["show", ":a.txt"]),
        staged_before,
        "unrelated staging preserved"
    );
    assert_eq!(
        fixture.git(&["show", ":b.txt"]),
        "b\nchanged",
        "committed file no longer shows a staged revert"
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        "MM a.txt\n M c.txt"
    );
    assert_eq!(
        index_before, index_after,
        "racy-index rule: index copy keeps the original mtime"
    );
}

#[test]
fn partially_staged_selected_file_keeps_the_users_staging() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("a.txt", "a\nstaged\n");
    fixture.git(&["add", "a.txt"]);
    fixture.write("a.txt", "a\nstaged\nmore\n");
    fixture.aisc(&["commit", "-m", "A", "a.txt"]).ok();
    assert_eq!(
        fixture.tree_blob("HEAD", "a.txt").unwrap(),
        "a\nstaged\nmore\n"
    );
    // Index was neither HEAD's old entry nor untouched: it keeps the user's staged blob.
    assert_eq!(fixture.git(&["show", ":a.txt"]), "a\nstaged");
}

#[test]
fn a_file_changed_between_read_and_commit_is_a_stale_error() {
    let fixture = pilot1();
    let hook = "printf 'sneaky\\n' >> README.md";
    let env = fixture.fake_git("hash-object", hook);
    let before_refs = fixture.git(&["for-each-ref"]);
    let run = fixture.aisc_env(
        &["commit", "-m", "x", "README.md"],
        &support::env_refs(&env),
    );
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert_eq!(before_refs, fixture.git(&["for-each-ref"]));
}

#[test]
fn quote_leading_file_names_are_hashed_from_their_own_content() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    let outside = fixture.root.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "SECRET\n").unwrap();
    // `git hash-object --stdin-paths` would C-unquote both names and read other files.
    let traversal = r#""\056\056\057outside.txt""#;
    let quoted = r#""x""#;
    fixture.write("x", "plain x\n");
    fixture.write(traversal, "own traversal content\n");
    fixture.write(quoted, "own quoted content\n");
    fixture.aisc(&["commit", "-m", "Odd names", "--all"]).ok();
    assert_eq!(
        fixture.tree_blob("HEAD", traversal).unwrap(),
        "own traversal content\n"
    );
    assert_eq!(
        fixture.tree_blob("HEAD", quoted).unwrap(),
        "own quoted content\n"
    );
    assert_eq!(fixture.tree_blob("HEAD", "x").unwrap(), "plain x\n");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "SECRET\n");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn undo_refuses_after_head_moved_to_another_branch_or_detached() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    fixture.aisc(&["commit", "-m", "c1", "f.txt"]).ok();
    fixture.git(&["switch", "-c", "other"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["undo"]);
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("HEAD"), "{}", run.stderr);
    assert_eq!(
        before,
        fixture.snapshot(),
        "no staged revert on the other branch"
    );
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    // Back on the branch the op ran on, undo works again.
    fixture.git(&["switch", "main"]);
    fixture.aisc(&["undo"]).ok();
    assert!(fixture.git(&["status", "--porcelain"]).contains(" M f.txt"));

    let detached = Fixture::with_files(&[("f.txt", "one\n")]);
    detached.write("f.txt", "one\ntwo\n");
    detached.aisc(&["commit", "-m", "c1", "f.txt"]).ok();
    detached.git(&["checkout", "--detach"]);
    let before = detached.snapshot();
    let run = detached.aisc(&["undo"]);
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("HEAD"), "{}", run.stderr);
    assert_eq!(before, detached.snapshot());
}

#[test]
fn intent_to_add_entries_do_not_survive_as_user_staging() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("new.txt", "brand new\n");
    fixture.git(&["add", "-N", "new.txt"]);
    fixture.aisc(&["commit", "-m", "Add new", "new.txt"]).ok();
    assert_eq!(fixture.git(&["status", "--porcelain"]), "");
    assert!(
        fixture
            .git(&["diff", "--cached", "--name-status"])
            .is_empty()
    );
    // The user's next commit must not delete the file.
    fixture.git(&["commit", "--allow-empty", "-m", "follow-up"]);
    assert!(fixture.exists("new.txt"));
    assert_eq!(fixture.tree_blob("HEAD", "new.txt").unwrap(), "brand new\n");
    assert_eq!(fixture.git(&["ls-files", "new.txt"]), "new.txt");
}

#[test]
fn line_ranges_are_validated_for_every_file_state() {
    let fixture = Fixture::with_files(&[
        ("gone.txt", "bye\n"),
        ("data.bin", "AAAA"),
        ("keep.txt", "k\n"),
    ]);
    std::fs::remove_file(fixture.path("gone.txt")).unwrap();
    fixture.write_bytes("data.bin", &[0, 1, 2]);
    fixture.write("added.txt", "one\ntwo\n");
    fixture.git(&["add", "added.txt"]);
    fixture.write("fresh.txt", "one\ntwo\n");
    let before = fixture.snapshot();
    for (token, message) in [
        ("added.txt:50-60", "added.txt has 2 lines"),
        ("fresh.txt:50-60", "fresh.txt has 2 lines"),
        ("gone.txt:1", "gone.txt"),
        ("data.bin:1-2", "data.bin"),
    ] {
        let run = fixture.aisc(&["commit", "-m", "x", token]);
        assert_eq!(run.code, 1, "{token}: {}", run.stdout);
        assert!(run.stderr.contains(message), "{token}: {}", run.stderr);
        assert_eq!(before, fixture.snapshot(), "{token}");
    }
    // A valid range on a whole-file item still selects the file.
    fixture.aisc(&["commit", "-m", "ok", "added.txt:1-2"]).ok();
    assert_eq!(fixture.commit_files("HEAD"), ["added.txt"]);
}
