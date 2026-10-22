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

fn residue(fixture: &Fixture) -> Vec<String> {
    fixture.aisc_residue()
}

/// Re-arms the fake-git hook for every call of the subcommand except `update-ref --stdin` (the
/// reference transaction), where it runs `action`.
fn on_transaction(fixture: &Fixture, action: &str) -> Vec<(String, String)> {
    let mark = fixture.directory.path().join("mark");
    let hook = format!(
        "case \"$*\" in *--stdin*) {action};; *) rm -f '{}';; esac",
        mark.display()
    );
    fixture.fake_git("update-ref", &hook)
}

/// Kills a `commit -m two f.txt` right after its reference transaction landed.
fn crash_after_commit_transaction(fixture: &Fixture) {
    fixture.write("f.txt", "one\ntwo\n");
    let env = fixture.fake_git("update-ref", KILL_AFTER);
    assert_killed(&fixture.aisc_env(&["commit", "-m", "two", "f.txt"], &env_refs(&env)));
    assert!(fixture.exists(".git/aisc/journal.json"), "crash reproduced");
}

/// After a crash past the reference transaction: the next command completes the operation,
/// reports it (exit 4) and changes nothing else.
fn recover_forward(fixture: &Fixture) -> Run {
    let next = fixture.aisc(&["commit", "-m", "unrelated", "f.txt"]);
    assert_eq!(next.code, 4, "{}", all(&next));
    assert!(all(&next).contains("interrupted"), "{}", all(&next));
    assert!(residue(fixture).is_empty(), "{:?}", residue(fixture));
    next
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
    // Reads still work and leave a live owner alone.
    fixture.aisc(&["log"]).ok();
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

/// A sync of `topic` onto `main` paused on `f.txt`, whose markers are in `f.txt.aisc-conflict`
/// (a dirty edit keeps `f.txt`), with the side file already resolved.
fn paused_sync_with_resolved_side_file() -> Fixture {
    let fixture = Fixture::with_files(&[("f.txt", "a\nb\nc\nd\ne\n"), ("src/g.txt", "g\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.txt", "a\nB1\nc\nd\ne\n");
    fixture.commit_all("topic edit");
    fixture.git(&["switch", "main"]);
    fixture.write("f.txt", "a\nB2\nc\nd\ne\n");
    fixture.write("src/g.txt", "g2\n");
    fixture.commit_all("main edit");
    fixture.git(&["switch", "topic"]);
    fixture.write("f.txt", "a\nB1\nc\nd\nE\n");
    fixture.aisc(&["sync", "main"]).ok();
    assert!(fixture.exists("f.txt.aisc-conflict"));
    fixture.write("f.txt.aisc-conflict", "a\nB1\nc\nd\ne\n");
    fixture
}

#[test]
fn partial_sync_apply_removes_its_conflict_side_files() {
    let fixture = paused_sync_with_resolved_side_file();
    // Make src unwritable while the reference transaction runs: the carry write then fails.
    let env = on_transaction(
        &fixture,
        &format!("chmod a-w '{}'", fixture.path("src").display()),
    );
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

#[test]
fn a_user_removed_index_lock_still_gets_the_new_index() {
    // Git's own advice after a crash is to delete index.lock; nothing else changed.
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    crash_after_commit_transaction(&fixture);
    std::fs::remove_file(fixture.path(".git/index.lock")).unwrap();
    let next = recover_forward(&fixture);
    assert!(!all(&next).contains("left as found"), "{}", all(&next));
    assert_eq!(fixture.subjects("HEAD"), ["two", "base"]);
    assert!(
        fixture.git(&["status", "--porcelain"]).is_empty(),
        "index brought forward"
    );
}

#[test]
fn any_history_read_recovers_an_interrupted_operation() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    crash_after_commit_transaction(&fixture);
    let log = fixture.recover();
    assert!(log.stderr.contains("interrupted"), "{}", all(&log));
    assert!(log.stdout.contains("two"), "{}", log.stdout);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_task_verb_blocked_by_a_crash_points_at_recovery() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    crash_after_commit_transaction(&fixture);
    let blocked = fixture.aisc(&["checkpoint", "-m", "cp", "--", "f.txt"]);
    assert_ne!(blocked.code, 0, "{}", all(&blocked));
    assert!(all(&blocked).contains("aisc log"), "{}", all(&blocked));
    assert!(
        !all(&blocked).contains("before removing"),
        "{}",
        all(&blocked)
    );
    fixture.recover();
}

/// Rewrites the journal's owner pid (and the temporary files named after it).
fn retag_journal(fixture: &Fixture, pid: u32) {
    let path = fixture.path(".git/aisc/journal.json");
    let mut journal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let old = journal["pid"].as_u64().unwrap();
    journal["pid"] = pid.into();
    std::fs::write(&path, journal.to_string()).unwrap();
    for entry in std::fs::read_dir(fixture.path(".git/aisc/tmp")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(&format!("-{old}")) {
            let renamed = entry.path().with_file_name(format!("{stem}-{pid}"));
            std::fs::rename(entry.path(), renamed).unwrap();
        }
    }
}

fn age_journal(fixture: &Fixture, seconds: u64) {
    let file = std::fs::File::options()
        .write(true)
        .open(fixture.path(".git/aisc/journal.json"))
        .unwrap();
    let time = std::time::SystemTime::now() - Duration::from_secs(seconds);
    file.set_modified(time).unwrap();
}

#[test]
fn a_journal_without_its_lock_is_recovered_only_when_its_owner_is_surely_gone() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    crash_after_commit_transaction(&fixture);
    let dead = serde_json::from_str::<serde_json::Value>(&fixture.read(".git/aisc/journal.json"))
        .unwrap()["pid"]
        .as_u64()
        .unwrap() as u32;
    // Someone removed the operation lock by hand: ownership now rests on the journal alone.
    std::fs::remove_file(fixture.path(".git/aisc-operation.lock")).unwrap();
    let refused = |why: &str| {
        let run = fixture.aisc(&["commit", "-m", "x", "f.txt"]);
        assert_eq!(run.code, 1, "{why}: {}", all(&run));
        assert!(all(&run).contains("journal"), "{why}: {}", all(&run));
        assert!(fixture.exists(".git/aisc/journal.json"), "{why}");
        assert!(fixture.exists(".git/index.lock"), "{why}");
        assert!(!fixture.exists(".git/aisc-operation.lock"), "{why}");
    };
    // A live owner pid (this test process).
    retag_journal(&fixture, std::process::id());
    age_journal(&fixture, 60);
    refused("live owner");
    // A dead owner, but the journal was written moments ago.
    retag_journal(&fixture, dead);
    age_journal(&fixture, 0);
    refused("fresh journal");
    // A dead owner and an old journal: recovered.
    age_journal(&fixture, 60);
    recover_forward(&fixture);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn recovery_removes_only_side_files_still_as_the_plan_saw_them() {
    for edited in [false, true] {
        let fixture = paused_sync_with_resolved_side_file();
        let env = on_transaction(&fixture, KILL_AFTER);
        assert_killed(&fixture.aisc_env(&["sync", "--continue"], &env_refs(&env)));
        if edited {
            fixture.write("f.txt.aisc-conflict", "edited after the crash\n");
        }
        let next = fixture.aisc(&["sync", "--abort"]);
        assert_eq!(next.code, 4, "{}", all(&next));
        assert!(all(&next).contains("interrupted"), "{}", all(&next));
        assert!(residue(&fixture).is_empty(), "{:?}", residue(&fixture));
        assert!(!fixture.exists(".git/aisc/sync.json"));
        if edited {
            assert_eq!(
                fixture.read("f.txt.aisc-conflict"),
                "edited after the crash\n"
            );
            assert!(all(&next).contains("f.txt.aisc-conflict"), "{}", all(&next));
        } else {
            assert!(!fixture.exists("f.txt.aisc-conflict"));
        }
    }
}
