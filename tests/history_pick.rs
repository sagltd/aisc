//! Pick verb tests. The pilot-13 fixture is ported from vcbench `scripts/lib/scenario-copy.mjs`
//! (github.com/gitbutlerapp/version-control-bench, MIT): a revival commit stacked on graph work,
//! copied to a fresh branch while an unrelated branch is checked out with a scratch note.
mod support;

use support::Fixture;

const DRAFT: &str = "Draft: ask about keyboard navigation.\n";
const SELECTED_COMMANDS: &str = "def available_commands():\n    return [\"help\", \"worktree\"]\n";
const SELECTED_TEST: &str = "from commands import available_commands\n\nassert available_commands() == [\"help\", \"worktree\"]\n";

fn pilot13() -> Fixture {
    let fixture = Fixture::with_files(&[
        (
            "README.md",
            "# Commands example\n\nRun the tests with `python3 -B test_commands.py`.\n",
        ),
        (
            "commands.py",
            "def available_commands():\n    return [\"help\"]\n",
        ),
        (
            "graph.py",
            "def neighbors(graph, node):\n    return list(graph.get(node, ()))\n",
        ),
        (
            "test_commands.py",
            "from commands import available_commands\n\nassert available_commands() == [\"help\"]\n",
        ),
    ]);
    fixture.git(&["switch", "-c", "revive-worktree-command"]);
    fixture.write(
        "graph.py",
        "from graph_core import lookup_neighbors\n\ndef neighbors(graph, node):\n    return lookup_neighbors(graph, node)\n",
    );
    fixture.write(
        "graph_core.py",
        "def lookup_neighbors(graph, node):\n    return list(graph.get(node, ()))\n",
    );
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit",
        "-m",
        "Extract graph neighbor lookup",
        "--author",
        "Alex Example <alex@example.test>",
    ]);
    fixture.write("commands.py", SELECTED_COMMANDS);
    fixture.write("test_commands.py", SELECTED_TEST);
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit",
        "-m",
        "Revive worktree command",
        "--author",
        "Morgan Example <morgan@example.test>",
    ]);
    fixture.git(&["switch", "-c", "ui-note", "main"]);
    fixture.write("ui-note.txt", "Keep the compact toolbar.\n");
    fixture.commit_all("Add UI note");
    fixture.write("scratch-note.txt", DRAFT);
    fixture
}

#[test]
fn pilot13_copies_a_commit_without_touching_head_or_the_scratch_note() {
    let fixture = pilot13();
    let before = fixture.snapshot();
    let main = fixture.git(&["rev-parse", "main"]);
    let source = fixture.git(&["rev-parse", "revive-worktree-command"]);
    let ui = fixture.git(&["rev-parse", "ui-note"]);
    let run = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "-b",
        "revive-worktree-no-graph",
        "--onto",
        "main",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    let branch = "revive-worktree-no-graph";
    // One commit on main with the original message and author, and exactly the selected files.
    assert_eq!(fixture.git(&["rev-parse", &format!("{branch}~1")]), main);
    assert_eq!(
        fixture.subjects(&format!("main..{branch}")),
        ["Revive worktree command"]
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an <%ae>", branch]),
        "Morgan Example <morgan@example.test>"
    );
    assert_eq!(
        fixture.commit_files(branch),
        ["commands.py", "test_commands.py"]
    );
    assert!(fixture.tree_blob(branch, "graph_core.py").is_none());
    assert_eq!(
        fixture.tree_blob(branch, "commands.py").unwrap(),
        SELECTED_COMMANDS
    );
    // Source, main and the checked-out branch are intact; HEAD and the scratch note too.
    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    assert_eq!(
        fixture.git(&["rev-parse", "revive-worktree-command"]),
        source
    );
    assert_eq!(fixture.git(&["rev-parse", "ui-note"]), ui);
    let after = fixture.snapshot();
    assert_eq!(after.head, before.head);
    assert_eq!(after.index, before.index);
    assert_eq!(after.files, before.files);
    assert_eq!(fixture.read("scratch-note.txt"), DRAFT);
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "ui-note"
    );
    assert!(
        run.stdout.starts_with("picked 1 commit onto new branch"),
        "{}",
        run.stdout
    );
}

#[test]
fn pick_onto_the_current_branch_carries_the_worktree() {
    let fixture = pilot13();
    fixture
        .aisc(&[
            "pick",
            "--from",
            "revive-worktree-command",
            "Revive worktree command",
        ])
        .ok();
    assert_eq!(
        fixture.subjects("main..HEAD"),
        ["Revive worktree command", "Add UI note"]
    );
    assert_eq!(fixture.read("commands.py"), SELECTED_COMMANDS);
    assert_eq!(fixture.read("scratch-note.txt"), DRAFT);
    // Only the scratch note is uncommitted; nothing is staged.
    assert_eq!(
        fixture.git(&["status", "--porcelain", "--untracked-files=all"]),
        "?? scratch-note.txt"
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.subjects("main..HEAD"), ["Add UI note"]);
    assert_eq!(
        fixture.read("commands.py"),
        "def available_commands():\n    return [\"help\"]\n"
    );
}

#[test]
fn several_commits_keep_their_relative_order() {
    let fixture = pilot13();
    fixture
        .aisc(&[
            "pick",
            "--from",
            "revive-worktree-command",
            "Revive worktree command",
            "Extract graph neighbor lookup",
            "-b",
            "both",
            "--onto",
            "main",
        ])
        .ok();
    assert_eq!(
        fixture.subjects("main..both"),
        ["Revive worktree command", "Extract graph neighbor lookup"]
    );
    assert_eq!(
        fixture.git(&["rev-parse", "both^{tree}"]),
        fixture.git(&["rev-parse", "revive-worktree-command^{tree}"])
    );
}

#[test]
fn to_an_existing_branch_appends_without_touching_head() {
    let fixture = pilot13();
    fixture.git(&["branch", "target", "main"]);
    let before = fixture.snapshot();
    fixture
        .aisc(&[
            "pick",
            "--from",
            "revive-worktree-command",
            "Revive worktree command",
            "--to",
            "target",
        ])
        .ok();
    assert_eq!(
        fixture.subjects("main..target"),
        ["Revive worktree command"]
    );
    let after = fixture.snapshot();
    assert_eq!(after.head, before.head);
    assert_eq!(after.index, before.index);
    assert_eq!(after.files, before.files);
}

#[test]
fn to_a_branch_checked_out_elsewhere_is_refused() {
    let fixture = pilot13();
    fixture.git(&["branch", "target", "main"]);
    let elsewhere = fixture.directory.path().join("elsewhere");
    fixture.git(&["worktree", "add", elsewhere.to_str().unwrap(), "target"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "--to",
        "target",
    ]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains("checked out"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

fn conflicting() -> Fixture {
    let fixture = Fixture::with_files(&[("f.txt", "one\ntwo\nthree\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write("f.txt", "one\nfeature\nthree\n");
    fixture.write("g.txt", "feature only\n");
    fixture.commit_all("feature edit");
    fixture.git(&["switch", "-c", "main-line", "main"]);
    fixture.write("f.txt", "one\nmain-line\nthree\n");
    fixture.commit_all("main-line edit");
    fixture.write("scratch.txt", "scratch\n");
    fixture
}

#[test]
fn conflicting_pick_aborts_atomically_with_a_report() {
    let fixture = conflicting();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["pick", "--from", "feature", "feature edit"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stderr.contains("f.txt conflicts") && run.stderr.contains("feature edit"),
        "{}",
        run.stderr
    );
    assert_eq!(fixture.snapshot(), before);
    // The same failure on a new branch creates nothing.
    let other = fixture.aisc(&[
        "pick",
        "--from",
        "feature",
        "feature edit",
        "-b",
        "copy",
        "--onto",
        "main-line",
    ]);
    assert_eq!(other.code, 1);
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/copy"]));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn pick_refuses_to_overwrite_overlapping_uncommitted_work() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write("g.txt", "from feature\n");
    fixture.commit_all("add g");
    fixture.git(&["switch", "main"]);
    fixture.write("g.txt", "my own g\n"); // untracked file in the way
    let before = fixture.snapshot();
    let run = fixture.aisc(&["pick", "--from", "feature", "add g"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("g.txt"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn already_applied_commit_is_reported_and_changes_nothing() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write("f.txt", "two\n");
    fixture.commit_all("change f");
    fixture.git(&["switch", "-c", "same", "main"]);
    fixture.write("f.txt", "two\n");
    fixture.commit_all("same change elsewhere");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["pick", "--from", "feature", "change f"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("already applied"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

/// `main` has `a/old`-style content; `feature` adds `path`; HEAD is on `main`.
fn adding(path: &str) -> Fixture {
    let fixture = Fixture::with_files(&[("keep.txt", "keep\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write(path, "picked\n");
    fixture.commit_all("add path");
    fixture.git(&["switch", "main"]);
    fixture
}

fn refused(fixture: &Fixture, needle: &str) {
    let before = fixture.snapshot();
    let run = fixture.aisc(&["pick", "--from", "feature", "add path"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains(needle), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/aisc/oplog/main"]));
}

#[test]
fn untracked_file_where_a_directory_is_needed_is_refused() {
    let fixture = adding("a/b.txt");
    fixture.write("a", "in the way\n");
    refused(&fixture, "a is a file where a directory is needed");
}

#[test]
fn ignored_file_where_a_directory_is_needed_is_refused() {
    let fixture = adding("a/b.txt");
    fixture.write(".git/info/exclude", "a\n");
    fixture.write("a", "ignored\n");
    refused(&fixture, "a is a file where a directory is needed");
}

#[test]
fn ignored_file_at_the_target_path_is_refused() {
    let fixture = adding("out.log");
    fixture.write(".git/info/exclude", "*.log\n");
    fixture.write("out.log", "ignored\n");
    refused(&fixture, "an untracked or ignored file is in the way");
}

#[test]
fn untracked_directory_where_a_file_is_needed_is_refused() {
    let fixture = adding("a");
    fixture.write("a/inner.txt", "untracked\n");
    refused(&fixture, "directory");
}

#[test]
fn symlinked_parent_is_refused() {
    let fixture = adding("a/b.txt");
    fixture.write("elsewhere/keep", "x\n");
    std::os::unix::fs::symlink("elsewhere", fixture.path("a")).unwrap();
    refused(&fixture, "a is a symlink");
    assert!(!fixture.exists("elsewhere/b.txt"));
}

#[test]
fn clean_directory_replaced_by_a_file_is_refused_clearly() {
    let fixture = Fixture::with_files(&[("a/b.txt", "tracked\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.git(&["rm", "-q", "-r", "a"]);
    fixture.write("a", "now a file\n");
    fixture.commit_all("add path");
    fixture.git(&["switch", "main"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["pick", "--from", "feature", "add path"]);
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.contains("a directory is where") && !run.stderr.contains("uncommitted"),
        "{}",
        run.stderr
    );
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn clean_file_replaced_by_a_directory_is_picked() {
    // One plan deletes `a` before it writes `a/b.txt`, so the file can become a directory.
    let fixture = Fixture::with_files(&[("a", "tracked file\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.git(&["rm", "-q", "a"]);
    fixture.write("a/b.txt", "now a dir\n");
    fixture.commit_all("add path");
    fixture.git(&["switch", "main"]);
    fixture
        .aisc(&["pick", "--from", "feature", "add path"])
        .ok();
    assert_eq!(fixture.read("a/b.txt"), "now a dir\n");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn dirty_tracked_overlap_is_refused_as_uncommitted() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write("f.txt", "two\n");
    fixture.commit_all("add path");
    fixture.git(&["switch", "main"]);
    fixture.write("f.txt", "mine\n");
    refused(&fixture, "f.txt has uncommitted changes");
}

#[test]
fn staged_only_overlap_is_refused() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-c", "feature"]);
    fixture.write("f.txt", "two\n");
    fixture.commit_all("add path");
    fixture.git(&["switch", "main"]);
    fixture.write("f.txt", "staged\n");
    fixture.git(&["add", "f.txt"]);
    fixture.write("f.txt", "one\n"); // worktree == HEAD, index differs
    refused(&fixture, "f.txt has staged changes");
}

#[test]
fn branch_with_new_branch_is_a_usage_error() {
    let fixture = pilot13();
    fixture.git(&["branch", "target", "main"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "--branch",
        "target",
        "-b",
        "fresh",
    ]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("not both"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn new_branch_name_must_be_free_and_valid() {
    let fixture = pilot13();
    let before = fixture.snapshot();
    let existing = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "-b",
        "main",
    ]);
    assert_eq!(existing.code, 1);
    let bad = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "-b",
        "bad..name",
    ]);
    assert_eq!(bad.code, 1);
    assert_eq!(fixture.snapshot(), before);
}

/// main plus a remote-tracking `origin/feature` holding one commit that no local branch has.
fn with_remote_feature() -> (Fixture, String) {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    let origin = fixture.directory.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    fixture.git(&["init", "-q", "--bare", origin]);
    fixture.git(&["remote", "add", "origin", origin]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write("f.txt", "remote work\n");
    fixture.commit_all("add remote work");
    fixture.git(&["push", "-q", "origin", "feature"]);
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    fixture.git(&["switch", "-q", "main"]);
    fixture.git(&["branch", "-q", "-D", "feature"]);
    fixture.git(&["fetch", "-q", "origin"]);
    (fixture, oid)
}

#[test]
fn log_and_pick_accept_a_remote_tracking_branch() {
    let (fixture, oid) = with_remote_feature();
    let log = fixture.aisc(&["log", "origin/feature"]);
    log.ok();
    assert!(log.stdout.contains("add remote work"), "{}", log.stdout);
    let pick = fixture.aisc(&["pick", "--from", "origin/feature", &oid[..7]]);
    pick.ok();
    assert_eq!(fixture.subjects("main")[0], "add remote work");
    assert_eq!(
        fixture.git(&["rev-parse", "origin/feature"]),
        oid,
        "the remote-tracking branch is never rewritten"
    );
}

#[test]
fn history_rewrites_still_refuse_a_remote_tracking_branch() {
    let (fixture, oid) = with_remote_feature();
    for args in [
        &[
            "uncommit",
            "--branch",
            "origin/feature",
            "add remote work",
            "f.txt",
        ][..],
        &[
            "move",
            "--branch",
            "origin/feature",
            "add remote work",
            "--before",
            "base",
        ][..],
    ] {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains("origin/feature"), "{}", run.stderr);
    }
    assert_eq!(fixture.git(&["rev-parse", "origin/feature"]), oid);
}

#[test]
fn pick_switch_moves_to_the_new_branch_carrying_the_scratch_note() {
    let fixture = pilot13();
    let main = fixture.git(&["rev-parse", "main"]);
    let ui = fixture.git(&["rev-parse", "ui-note"]);
    let run = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "-b",
        "revive-worktree-no-graph",
        "--onto",
        "main",
        "--switch",
    ]);
    run.ok();
    let branch = "revive-worktree-no-graph";
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), branch);
    assert_eq!(fixture.git(&["rev-parse", &format!("{branch}~1")]), main);
    assert_eq!(
        fixture.subjects(&format!("main..{branch}")),
        ["Revive worktree command"]
    );
    // The work tree is the new branch's, the scratch note came along, nothing is staged.
    assert_eq!(fixture.read("commands.py"), SELECTED_COMMANDS);
    assert!(!fixture.exists("ui-note.txt"));
    assert_eq!(fixture.read("scratch-note.txt"), DRAFT);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        "?? scratch-note.txt"
    );
    assert_eq!(fixture.git(&["rev-parse", "ui-note"]), ui);
    assert!(run.stdout.contains("switched"), "{}", run.stdout);
    // Undo: back on ui-note with its file, the new branch gone, the note still there.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "ui-note"
    );
    assert!(!fixture.git_ok(&["rev-parse", "--verify", &format!("refs/heads/{branch}")]));
    assert!(fixture.exists("ui-note.txt"));
    assert_eq!(fixture.read("scratch-note.txt"), DRAFT);
    // --switch needs a destination branch.
    let before = fixture.snapshot();
    let bad = fixture.aisc(&[
        "pick",
        "--from",
        "revive-worktree-command",
        "Revive worktree command",
        "--switch",
    ]);
    assert_eq!(bad.code, 1, "{} {}", bad.stdout, bad.stderr);
    assert!(bad.stderr.contains("--switch"), "{}", bad.stderr);
    assert_eq!(fixture.snapshot(), before);
}
