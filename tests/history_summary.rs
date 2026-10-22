//! The short post-mutation summary is derived from what the verb already knew. It must always
//! agree with a fresh read (`aisc log -q`) right after, and stay within the Git process budget.
mod support;

use std::os::unix::fs::PermissionsExt;
use support::{Fixture, Run};

/// The summary lines that a fresh `aisc log -q` also prints: the branch header (without the
/// commit-list suffix), the uncommitted paths and the state lines.
fn summary_of(run: &Run) -> Vec<String> {
    run.stdout
        .lines()
        .skip(1) // the result line
        .filter(|line| {
            line.starts_with("uncommitted: ")
                || line.starts_with("sync paused")
                || line.starts_with("ignored nested")
                || line.contains(": ") && (line.contains(" commit") || line.contains(" last "))
        })
        .filter(|line| !line.starts_with("moved branch"))
        .map(|line| line.trim_end_matches(" (newest first)").to_owned())
        .collect()
}

fn fresh(fixture: &Fixture) -> Vec<String> {
    let log = fixture.aisc(&["log", "-q"]);
    log.ok();
    let mut lines: Vec<String> = log
        .stdout
        .lines()
        .take(1)
        .map(|line| line.trim_end_matches(" (newest first)").to_owned())
        .collect();
    lines.extend(
        log.stdout
            .lines()
            .skip(1)
            .filter(|line| {
                line.starts_with("uncommitted: ")
                    || line.starts_with("sync paused")
                    || line.starts_with("ignored nested")
            })
            .map(str::to_owned),
    );
    lines
}

fn agrees(fixture: &Fixture, args: &[&str]) -> Run {
    let run = fixture.aisc(args);
    run.ok();
    assert_eq!(
        summary_of(&run),
        fresh(fixture),
        "{args:?} printed:\n{}",
        run.stdout
    );
    run
}

fn topic() -> Fixture {
    let fixture = Fixture::with_files(&[
        (
            "a.txt",
            "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n",
        ),
        ("b.txt", "b\n"),
    ]);
    fixture.git(&["switch", "-q", "-c", "topic"]);
    fixture.write("c.txt", "c\n");
    fixture.commit_all("add c");
    fixture.write("d.txt", "d\n");
    fixture.commit_all("add d");
    fixture
}

#[test]
fn commit_partial_whole_and_untracked_agree_with_a_fresh_read() {
    let fixture = topic();
    fixture.write(
        "a.txt",
        "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\nfifteen\n",
    );
    fixture.write("b.txt", "b2\n");
    fixture.write("new.txt", "new\n");
    fixture.write("notes/n.md", "n\n");
    agrees(&fixture, &["commit", "-m", "partial", "a.txt~one", "b.txt"]);
    agrees(&fixture, &["commit", "-m", "untracked", "new.txt"]);
    // Every hunk of the rest: the file becomes clean.
    agrees(&fixture, &["commit", "-m", "rest", "a.txt"]);
    // Ignored files never show, whether or not a rule exists.
    fixture.write(".gitignore", "*.log\n");
    fixture.write("debug.log", "x\n");
    agrees(&fixture, &["commit", "-m", "ignore", ".gitignore"]);
}

#[test]
fn rewrites_agree_with_a_fresh_read() {
    let fixture = topic();
    fixture.write("e.txt", "e\n");
    fixture.commit_all("add e");
    fixture.write("b.txt", "b changed\n");
    fixture.write("scratch.txt", "scratch\n");
    agrees(&fixture, &["amend", "--into", "add c", "b.txt"]);
    agrees(
        &fixture,
        &["squash", "--into", "add c and d", "add c", "add d"],
    );
    agrees(&fixture, &["move", "add e", "--before", "add c and d"]);
    agrees(&fixture, &["undo"]);
    agrees(
        &fixture,
        &[
            "split",
            "add c and d",
            "--into",
            "c",
            "c.txt",
            "--into",
            "d",
            "d.txt",
        ],
    );
    // Moving a file out of a commit leaves it untracked.
    agrees(&fixture, &["uncommit", "c", "c.txt"]);
    agrees(&fixture, &["undo"]);
    // With an ignore rule that matches, the untracked file is hidden again.
    fixture.write(".git/info/exclude", "c.txt\n");
    agrees(&fixture, &["uncommit", "c", "c.txt"]);
}

#[test]
fn switches_discards_and_staged_reverts_agree_with_a_fresh_read() {
    let fixture = topic();
    fixture.write("b.txt", "dirty\n");
    agrees(&fixture, &["switch", "main"]);
    agrees(&fixture, &["switch", "topic"]);
    agrees(&fixture, &["commit", "-b", "side", "-m", "side", "b.txt"]);
    // Staged, then reverted in the worktree: `git diff HEAD` no longer lists it.
    fixture.write("a.txt", "x\n");
    fixture.git(&["add", "a.txt"]);
    fixture.write(
        "a.txt",
        "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n",
    );
    fixture.write("b.txt", "again\n");
    agrees(&fixture, &["squash", "--into", "c and d", "add c", "add d"]);
    agrees(&fixture, &["discard", "b.txt"]);
}

#[test]
fn stack_and_quiet_modes() {
    let fixture = topic();
    fixture.write("b.txt", "b2\n");
    let stack = fixture.aisc(&["commit", "--stack", "-m", "b", "b.txt"]);
    stack.ok();
    assert!(
        stack.stdout.contains("(newest first)") && stack.stdout.contains(" b | b.txt"),
        "{}",
        stack.stdout
    );
    let default = fixture.aisc(&["squash", "--into", "c and d", "add c", "add d"]);
    default.ok();
    assert!(
        !default.stdout.contains("(newest first)"),
        "{}",
        default.stdout
    );
    // The squash result names the new commit; the re-parented one above it is listed.
    assert!(
        default
            .stdout
            .lines()
            .skip(1)
            .any(|line| line.ends_with(" b")),
        "the rewritten commit's new id and subject: {}",
        default.stdout
    );
    let undo = fixture.aisc(&["undo", "-q"]);
    undo.ok();
    assert_eq!(undo.stdout.lines().count(), 1, "{}", undo.stdout);
    let switch = fixture.aisc(&["switch", "-q", "main"]);
    switch.ok();
    assert_eq!(switch.stdout.lines().count(), 1, "{}", switch.stdout);
    let log = fixture.aisc(&["log", "-q"]);
    log.ok();
    assert!(!log.stdout.contains(" | "), "no file lists: {}", log.stdout);
}

#[test]
fn typical_verbs_stay_within_the_git_process_budget() {
    let fixture = topic();
    let counter = fixture.directory.path().join("count");
    let wrapper = fixture.directory.path().join("git-counting");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho x >> '{}'\nexec '{}' \"$@\"\n",
            counter.display(),
            support::real_git()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = wrapper.display().to_string();
    let spawns = |args: &[&str]| -> usize {
        let _ = std::fs::remove_file(&counter);
        fixture
            .aisc_env(args, &[("AISC_GIT", wrapper.as_str())])
            .ok();
        std::fs::read_to_string(&counter)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    fixture.write("b.txt", "b2\n");
    assert!(spawns(&["log"]) <= 3);
    assert!(spawns(&["commit", "-m", "b", "b.txt"]) <= 8);
    fixture.write("c.txt", "c2\n");
    assert!(spawns(&["amend", "--into", "add c", "c.txt"]) <= 8);
    assert!(spawns(&["squash", "--into", "c and d", "add c", "add d"]) <= 8);
    assert!(spawns(&["move", "b", "--before", "c and d"]) <= 8);
    assert!(spawns(&["undo"]) <= 8);
}
