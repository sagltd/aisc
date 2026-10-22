//! Crash consistency: an aisc process killed (SIGKILL) at any phase of a rewrite never leaves the
//! checkout stuck. The next mutating command rolls the operation forward or back, never losing the
//! user's index or files, and a lock held by a live process is always refused.
mod support;

use std::time::{Duration, Instant};
use support::{Fixture, Run, env_refs};

/// A fake-git hook that kills aisc (the wrapper's parent) before the real git runs.
const KILL_BEFORE: &str = "kill -KILL $PPID; exit 1";
/// A fake-git hook that runs the real git, then kills aisc before it can see the result.
const KILL_AFTER: &str = "\"$REAL_GIT\" \"$@\"; kill -KILL $PPID; exit 0";

fn all(run: &Run) -> String {
    format!("{}{}", run.stdout, run.stderr)
}

fn assert_killed(run: &Run) {
    assert_eq!(run.code, -1, "expected SIGKILL: {}", all(run));
}

/// Lock, journal and temporary files an interrupted operation may leave behind.
fn residue(fixture: &Fixture) -> Vec<String> {
    let mut found: Vec<String> = [
        ".git/aisc-operation.lock",
        ".git/index.lock",
        ".git/aisc/journal.json",
    ]
    .into_iter()
    .filter(|path| fixture.exists(path))
    .map(str::to_owned)
    .collect();
    if let Ok(entries) = std::fs::read_dir(fixture.path(".git/aisc/tmp")) {
        for entry in entries {
            found.push(entry.unwrap().file_name().to_string_lossy().into_owned());
        }
    }
    found
}

/// After a crash past the reference transaction: the next command completes the operation,
/// reports it (exit 4) and changes nothing else.
fn recover_forward(fixture: &Fixture) {
    let next = fixture.aisc(&["commit", "-m", "unrelated", "f.txt"]);
    assert_eq!(next.code, 4, "{}", all(&next));
    assert!(all(&next).contains("interrupted"), "{}", all(&next));
    assert!(residue(fixture).is_empty(), "{:?}", residue(fixture));
}

#[test]
fn kill_while_restoring_placeholders_completes_the_undo() {
    // Reviewer repro: `git add -N n.txt; aisc discard n.txt; aisc undo` killed at `git add -N`.
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("n.txt", "new\n");
    fixture.git(&["add", "-N", "n.txt"]);
    fixture.aisc(&["discard", "n.txt"]).ok();
    assert!(!fixture.exists("n.txt"));
    let env = fixture.fake_git("add", KILL_BEFORE);
    assert_killed(&fixture.aisc_env(&["undo"], &env_refs(&env)));
    assert!(
        fixture.exists(".git/aisc-operation.lock"),
        "crash reproduced"
    );
    assert!(fixture.exists(".git/index.lock"), "crash reproduced");
    recover_forward(&fixture);
    assert_eq!(fixture.read("n.txt"), "new\n");
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " A n.txt",
        "placeholder restored"
    );
    // The completed undo is an ordinary op: undoing it discards the file again.
    fixture.aisc(&["undo"]).ok();
    assert!(!fixture.exists("n.txt"));
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn kill_before_the_reference_transaction_rolls_back() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    let before = fixture.snapshot();
    let env = fixture.fake_git("update-ref", KILL_BEFORE);
    assert_killed(&fixture.aisc_env(&["commit", "-m", "two", "f.txt"], &env_refs(&env)));
    assert_eq!(before, fixture.snapshot(), "nothing changed");
    assert!(!residue(&fixture).is_empty(), "crash reproduced");
    // Nothing was in flight: the next command cleans up and proceeds.
    fixture.aisc(&["commit", "-m", "two", "f.txt"]).ok();
    assert_eq!(fixture.subjects("HEAD"), ["two", "base"]);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
}

#[test]
fn kill_while_preparing_rolls_back() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    fixture.git(&["add", "f.txt"]);
    let before = fixture.snapshot();
    // The op record's tree is written while the plan is prepared, before any journal.
    let env = fixture.fake_git("write-tree", KILL_BEFORE);
    assert_killed(&fixture.aisc_env(&["discard", "f.txt"], &env_refs(&env)));
    assert_eq!(before, fixture.snapshot(), "nothing changed");
    fixture.aisc(&["discard", "f.txt"]).ok();
    assert_eq!(fixture.read("f.txt"), "one\n");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
}

#[test]
fn kill_right_after_the_reference_transaction_rolls_forward() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    fixture.git(&["add", "f.txt"]);
    let env = fixture.fake_git("update-ref", KILL_AFTER);
    assert_killed(&fixture.aisc_env(&["discard", "f.txt"], &env_refs(&env)));
    assert_eq!(
        fixture.read("f.txt"),
        "one\ntwo\n",
        "worktree not written yet"
    );
    recover_forward(&fixture);
    assert_eq!(fixture.read("f.txt"), "one\n");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    // The user's staged change comes back with the undo.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.read("f.txt"), "one\ntwo\n");
    assert_eq!(fixture.git(&["status", "--porcelain"]), "M  f.txt");
}

#[test]
fn kill_after_moving_head_finishes_the_switch() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("g.txt", "topic file\n");
    fixture.commit_all("add g");
    fixture.git(&["switch", "main"]);
    // `symbolic-ref -q HEAD` reads re-arm the hook; the move itself runs, then aisc is killed.
    let mark = fixture.directory.path().join("mark");
    let hook = format!(
        "case \"$*\" in *-q*) rm -f '{}';; *) {KILL_AFTER};; esac",
        mark.display()
    );
    let env = fixture.fake_git("symbolic-ref", &hook);
    assert_killed(&fixture.aisc_env(&["switch", "topic"], &env_refs(&env)));
    assert_eq!(
        fixture.git(&["symbolic-ref", "HEAD"]),
        "refs/heads/topic",
        "HEAD moved before the kill"
    );
    let next = fixture.aisc(&["switch", "main"]);
    assert_eq!(next.code, 4, "{}", all(&next));
    assert!(all(&next).contains("interrupted"), "{}", all(&next));
    assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
    assert_eq!(fixture.read("g.txt"), "topic file\n", "worktree carried");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    fixture.aisc(&["switch", "main"]).ok();
    assert!(!fixture.exists("g.txt"));
}

#[test]
fn a_live_owner_keeps_its_locks() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    let before = fixture.snapshot();
    let env = fixture.fake_git("update-ref", "sleep 15; exit 1");
    let mut owner = fixture.spawn(&["commit", "-m", "two", "f.txt"], &env_refs(&env));
    let mark = fixture.directory.path().join("mark");
    let started = Instant::now();
    while !mark.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "owner never ran"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let contender = fixture.aisc(&["commit", "-m", "mine", "f.txt"]);
    assert_eq!(contender.code, 1, "{}", all(&contender));
    assert!(all(&contender).contains("locked"), "{}", all(&contender));
    assert!(fixture.exists(".git/aisc-operation.lock"), "live lock kept");
    assert!(fixture.exists(".git/index.lock"), "live lock kept");
    owner.kill().unwrap();
    owner.wait().unwrap();
    assert_eq!(before, fixture.snapshot());
    // Once the owner is dead the same command recovers and runs.
    fixture.aisc(&["commit", "-m", "two", "f.txt"]).ok();
    assert_eq!(fixture.subjects("HEAD"), ["two", "base"]);
    assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
}

#[test]
fn an_index_lock_aisc_did_not_create_is_never_taken_over() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    let env = fixture.fake_git("update-ref", KILL_BEFORE);
    assert_killed(&fixture.aisc_env(&["commit", "-m", "two", "f.txt"], &env_refs(&env)));
    // Someone else now holds index.lock (a fresh file, not the one aisc created).
    std::fs::remove_file(fixture.path(".git/index.lock")).unwrap();
    fixture.write(".git/index.lock", "foreign");
    let refused = fixture.aisc(&["commit", "-m", "two", "f.txt"]);
    assert_eq!(refused.code, 1, "{}", all(&refused));
    assert!(all(&refused).contains("index.lock"), "{}", all(&refused));
    assert_eq!(fixture.read(".git/index.lock"), "foreign");
    std::fs::remove_file(fixture.path(".git/index.lock")).unwrap();
    fixture.aisc(&["commit", "-m", "two", "f.txt"]).ok();
    assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
}

#[test]
fn a_foreign_operation_lock_is_never_removed() {
    // An empty lock (a task verb, or a manual one) carries no owner: always refused.
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "one\ntwo\n");
    fixture.write(".git/aisc-operation.lock", "");
    let refused = fixture.aisc(&["commit", "-m", "two", "f.txt"]);
    assert_eq!(refused.code, 1, "{}", all(&refused));
    assert!(fixture.exists(".git/aisc-operation.lock"));
}

#[test]
fn partial_sync_apply_removes_its_conflict_side_files() {
    let fixture = Fixture::with_files(&[("f.txt", "a\nb\nc\nd\ne\n"), ("src/g.txt", "g\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.txt", "a\nB1\nc\nd\ne\n");
    fixture.commit_all("topic edit");
    fixture.git(&["switch", "main"]);
    fixture.write("f.txt", "a\nB2\nc\nd\ne\n");
    fixture.write("src/g.txt", "g2\n");
    fixture.commit_all("main edit");
    fixture.git(&["switch", "topic"]);
    // A dirty edit in the conflicting file sends its markers to a side file.
    fixture.write("f.txt", "a\nB1\nc\nd\nE\n");
    fixture.aisc(&["sync", "main"]).ok();
    assert!(fixture.exists("f.txt.aisc-conflict"));
    fixture.write("f.txt.aisc-conflict", "a\nB1\nc\nd\ne\n");
    // Make src unwritable while the reference transaction (`update-ref --stdin`; other
    // update-ref calls re-arm the hook) runs: the carry write then fails.
    let mark = fixture.directory.path().join("mark");
    let hook = format!(
        "case \"$*\" in *--stdin*) chmod a-w '{}';; *) rm -f '{}';; esac",
        fixture.path("src").display(),
        mark.display()
    );
    let env = fixture.fake_git("update-ref", &hook);
    let run = fixture.aisc_env(&["sync", "--continue"], &env_refs(&env));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fixture.path("src"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("partially applied"), "{}", all(&run));
    assert!(
        !fixture.exists("f.txt.aisc-conflict"),
        "side file of the finished sync removed"
    );
    assert!(!fixture.exists(".git/aisc/sync.json"));
    assert_eq!(fixture.read("f.txt"), "a\nB1\nc\nd\nE\n", "dirty edit kept");
}
