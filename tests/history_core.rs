mod support;

use std::time::{Duration, Instant};
use support::{Fixture, env_refs, hunk_id};

fn numbered(prefix: &str, count: usize) -> String {
    (1..=count).map(|n| format!("{prefix}{n:02}\n")).collect()
}

/// Two-file repository with two separate hunks in `f.txt`, one in `notes.md` and an untracked file.
fn dirty() -> Fixture {
    let fixture = Fixture::with_files(&[
        ("f.txt", &numbered("line", 20)),
        ("notes.md", "first note line\n"),
    ]);
    let edited = numbered("line", 20)
        .replace("line02\n", "line02 changed\n")
        .replace("line19\n", "line19 changed\n");
    fixture.write("f.txt", &edited);
    fixture.write("notes.md", "first note line\nsecond note line\n");
    fixture.write("new.txt", "alpha\nbeta\n");
    fixture
}

#[test]
fn log_text_and_json_follow_the_grammar() {
    let fixture = dirty();
    let json = fixture.aisc(&["log", "--json"]);
    json.ok();
    let log = json.json();
    let h1 = hunk_id(&log, "f.txt", "line02 changed");
    let h2 = hunk_id(&log, "f.txt", "line19 changed");
    let h3 = hunk_id(&log, "notes.md", "second note line");
    for id in [&h1, &h2, &h3] {
        assert!(
            id.len() >= 3 && id.chars().all(|c| ('g'..='v').contains(&c)),
            "{id}"
        );
    }
    let short = fixture.git(&["rev-parse", "--short=4", "HEAD"]);
    let text = fixture.aisc(&["log"]);
    text.ok();
    let expected = format!(
        "main: last 1 commit (newest first)\n{short} base | f.txt notes.md\nuncommitted:\nf.txt\n {h1} @2 +1-1 line02 changed\n {h2} @19 +1-1 line19 changed\nnew.txt (untracked) +2\nnotes.md {h3} @2 +1 second note line\n"
    );
    assert_eq!(text.stdout, expected);
    assert_eq!(log["branch"], "main");
    assert_eq!(log["base"], serde_json::Value::Null);
    assert_eq!(log["commits"][0]["files"].as_array().unwrap().len(), 2);
    assert_eq!(log["changes"].as_array().unwrap().len(), 3);
    assert_eq!(log["changes"][1]["kind"], "untracked");
    assert_eq!(log["state"], serde_json::Value::Null);
    // Redirected output is compact text by default, JSON only on request or AISC_OUTPUT.
    assert!(
        fixture
            .aisc(&["log"])
            .stdout
            .starts_with("main: last 1 commit")
    );
    let env = fixture.aisc_env(&["log"], &[("AISC_OUTPUT", "json")]);
    assert_eq!(env.json()["branch"], "main");
    let compact = fixture.aisc(&["log", "--no-hunks"]);
    assert!(
        compact
            .stdout
            .contains("uncommitted: f.txt new.txt notes.md"),
        "{}",
        compact.stdout
    );
}

#[test]
fn clean_worktree_has_no_uncommitted_section_and_branch_view_reports_base() {
    let fixture = Fixture::with_files(&[("a.txt", "one\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("a.txt", "one\ntwo\n");
    fixture.commit_all("add two");
    fixture.write("b.txt", "bee\n");
    fixture.commit_all("add b");
    fixture.git(&["switch", "main"]);
    fixture.write("c.txt", "see\n");
    fixture.commit_all("main moves");
    fixture.git(&["switch", "topic"]);
    let log = fixture.aisc(&["log"]);
    log.ok();
    let lines: Vec<&str> = log.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "topic: 2 commits on main, main +1 ahead (aisc sync) (newest first)"
    );
    assert!(lines[1].ends_with(" add b | b.txt"));
    assert!(lines[2].ends_with(" add two | a.txt"));
    assert_eq!(lines.len(), 3, "{}", log.stdout);
    let other = fixture.aisc(&["log", "--branch", "main"]);
    other.ok();
    assert!(
        other.stdout.starts_with("main: last 2 commits"),
        "{}",
        other.stdout
    );
}

#[test]
fn hunk_ids_are_stable_and_edited_hunks_get_new_ids() {
    let fixture = dirty();
    let first = fixture.aisc(&["log", "--json"]).json();
    let second = fixture.aisc(&["log", "--json"]).json();
    assert_eq!(first["changes"], second["changes"]);
    let id = hunk_id(&first, "f.txt", "line02 changed");
    let keeper = hunk_id(&first, "f.txt", "line19 changed");
    // An unrelated edit elsewhere keeps both ids.
    fixture.write("other.txt", "unrelated\n");
    let third = fixture.aisc(&["log", "--json"]).json();
    assert_eq!(hunk_id(&third, "f.txt", "line02 changed"), id);
    // Editing the hunk changes its id only; the old id is rejected and nothing changes.
    let edited = fixture
        .read("f.txt")
        .replace("line02 changed", "line02 changed again");
    fixture.write("f.txt", &edited);
    let fourth = fixture.aisc(&["log", "--json"]).json();
    assert_ne!(hunk_id(&fourth, "f.txt", "line02 changed again"), id);
    assert_eq!(hunk_id(&fourth, "f.txt", "line19 changed"), keeper);
    let before = fixture.snapshot();
    let rejected = fixture.aisc(&["commit", "-m", "x", &id]);
    assert_eq!(rejected.code, 1);
    assert!(
        rejected.stderr.contains("no hunk with id"),
        "{}",
        rejected.stderr
    );
    assert!(rejected.stderr.contains("hint: "), "{}", rejected.stderr);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn selector_errors_name_the_problem_and_change_nothing() {
    let fixture = dirty();
    let before = fixture.snapshot();
    let cases: [(&[&str], &str); 5] = [
        (
            &["commit", "-m", "x", "missing.txt"],
            "unknown selector 'missing.txt'",
        ),
        (
            &["commit", "-m", "x", "f.txt~absent text"],
            "no changed line in f.txt contains",
        ),
        (&["commit", "-m", "x", "f.txt:0-3"], "invalid line range"),
        (
            &["commit", "-m", "x", "f.txt:15"],
            "no change in f.txt intersects",
        ),
        (
            &[
                "commit",
                "--into",
                "one",
                "f.txt",
                "--into",
                "two",
                "f.txt~line19",
            ],
            "selected twice",
        ),
    ];
    for (args, message) in cases {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(message), "{args:?}: {}", run.stderr);
        assert!(run.stdout.is_empty());
        assert_eq!(before, fixture.snapshot(), "{args:?}");
    }
    let json = fixture.aisc(&["--json", "commit", "-m", "x", "missing.txt"]);
    assert_eq!(json.code, 1);
    let error: serde_json::Value = serde_json::from_str(&json.stderr).unwrap();
    assert_eq!(error["code"], "selector");
    assert!(error["hint"].is_string());
}

#[test]
fn show_caps_changed_lines_and_full_adds_context() {
    let fixture = Fixture::with_files(&[("big.txt", "head\ntail\n")]);
    let mut content = String::from("head\n");
    for n in 1..=20 {
        content.push_str(&format!("added {n:02}\n"));
    }
    content.push_str("tail\n");
    fixture.write("big.txt", &content);
    let capped = fixture.aisc(&["show"]);
    capped.ok();
    assert!(capped.stdout.contains("+added 12\n"), "{}", capped.stdout);
    assert!(!capped.stdout.contains("+added 13"), "{}", capped.stdout);
    assert!(capped.stdout.contains("… +8"), "{}", capped.stdout);
    let full = fixture.aisc(&["show", "--full"]);
    full.ok();
    assert!(full.stdout.contains("+added 20\n"));
    assert!(full.stdout.contains(" head\n"));
    assert!(!full.stdout.contains("…"));
}

#[test]
fn show_a_commit_and_selected_hunks() {
    let fixture = Fixture::with_files(&[("f.txt", &numbered("line", 20))]);
    let edited = numbered("line", 20)
        .replace("line02\n", "line02 changed\n")
        .replace("line19\n", "line19 changed\n");
    fixture.write("f.txt", &edited);
    fixture.commit_all("two hunks");
    let show = fixture.aisc(&["show", "two hunks"]);
    show.ok();
    assert!(
        show.stdout.contains(" two hunks\nf.txt\n"),
        "{}",
        show.stdout
    );
    assert!(show.stdout.contains("+line19 changed"));
    let one = fixture.aisc(&["show", "HEAD", "f.txt~line19"]);
    one.ok();
    assert!(one.stdout.contains("+line19 changed"));
    assert!(!one.stdout.contains("line02 changed"), "{}", one.stdout);
    let range = fixture.aisc(&["show", "two hunks..two hunks"]);
    range.ok();
}

#[test]
fn concurrent_ref_update_exits_four_and_changes_nothing() {
    let fixture = dirty();
    let hook = "c=$(\"$REAL_GIT\" commit-tree 'main^{tree}' -p main -m external) && \"$REAL_GIT\" update-ref refs/heads/main \"$c\"";
    let env = fixture.fake_git("update-ref", hook);
    let before = fixture.snapshot();
    let run = fixture.aisc_env(&["commit", "-m", "mine", "f.txt"], &env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("nothing was changed"), "{}", run.stderr);
    let after = fixture.snapshot();
    assert_eq!(before.index, after.index, "index byte-identical");
    assert_eq!(before.files, after.files, "worktree byte-identical");
    assert_eq!(before.head, after.head);
    assert!(
        !after.refs.contains("refs/aisc"),
        "no oplog entry: {}",
        after.refs
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s", "main"]),
        "external"
    );
    assert!(!fixture.exists(".git/aisc-operation.lock") && !fixture.exists(".git/index.lock"));
}

#[test]
fn index_changed_during_operation_is_refused() {
    let fixture = dirty();
    let hook = "cp .git/index .git/idx2 && GIT_INDEX_FILE=.git/idx2 \"$REAL_GIT\" add new.txt && cp .git/idx2 .git/index && rm .git/idx2";
    let env = fixture.fake_git("hash-object", hook);
    let before = fixture.snapshot();
    let run = fixture.aisc_env(&["commit", "-m", "mine", "f.txt"], &env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("index changed"), "{}", run.stderr);
    let after = fixture.snapshot();
    assert_eq!(before.refs, after.refs);
    assert_eq!(before.head, after.head);
    assert_eq!(before.files, after.files);
    assert_ne!(
        before.index, after.index,
        "the external index change is kept, not overwritten"
    );
    assert_eq!(fixture.git(&["ls-files", "new.txt"]), "new.txt");
    assert!(!fixture.exists(".git/index.lock"));
}

#[test]
fn held_locks_refuse_without_changes() {
    let fixture = dirty();
    for lock in [".git/aisc-operation.lock", ".git/index.lock"] {
        fixture.write(lock, "");
        let before = fixture.snapshot();
        let run = fixture.aisc(&["commit", "-m", "x", "f.txt"]);
        assert_eq!(run.code, 1, "{lock}: {}", run.stderr);
        assert!(run.stderr.contains("locked"), "{lock}: {}", run.stderr);
        assert_eq!(before, fixture.snapshot());
        assert!(fixture.exists(lock), "a foreign lock is never removed");
        std::fs::remove_file(fixture.path(lock)).unwrap();
        assert!(!fixture.exists(".git/aisc-operation.lock"));
    }
}

#[test]
fn undo_restores_branch_head_and_index_and_redo_replays() {
    let fixture = dirty();
    fixture.git(&["add", "notes.md"]); // user-staged change must survive
    let staged = fixture.git(&["ls-files", "-s"]);
    let main_before = fixture.git(&["rev-parse", "main"]);
    let run = fixture.aisc(&["commit", "-b", "topic", "-m", "Edit f", "f.txt"]);
    run.ok();
    let commit = fixture.git(&["rev-parse", "topic"]);
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
    assert_eq!(fixture.git(&["rev-parse", "main"]), main_before);
    assert!(
        fixture
            .git(&["diff", "--cached", "--name-only"])
            .contains("notes.md")
    );
    assert!(!fixture.git(&["status", "--porcelain"]).contains("f.txt"));

    let undo = fixture.aisc(&["undo"]);
    undo.ok();
    assert!(undo.stdout.starts_with("undid "), "{}", undo.stdout);
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "--quiet", "refs/heads/topic"]));
    assert_eq!(
        fixture.git(&["ls-files", "-s"]),
        staged,
        "index entries restored"
    );
    assert!(fixture.git(&["status", "--porcelain"]).contains(" M f.txt"));
    assert_eq!(
        fixture.read("f.txt").matches("changed").count(),
        2,
        "worktree untouched"
    );

    let redo = fixture.aisc(&["undo"]);
    redo.ok();
    assert_eq!(fixture.git(&["rev-parse", "topic"]), commit);
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
    let log = fixture.aisc(&["oplog"]);
    log.ok();
    let lines: Vec<&str> = log.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", log.stdout);
    assert!(lines[0].contains(" undo ") && lines[1].contains(" undo "));
    assert!(lines[2].contains(" commit "));
}

#[test]
fn undo_after_an_external_move_is_refused() {
    let fixture = dirty();
    fixture.aisc(&["commit", "-m", "mine", "f.txt"]).ok();
    fixture.git(&["commit", "--allow-empty", "-m", "external"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["undo"]);
    assert_eq!(run.code, 4, "{}", run.stderr);
    assert!(
        run.stderr.contains("moved after that operation"),
        "{}",
        run.stderr
    );
    assert_eq!(before, fixture.snapshot());
    let nothing = Fixture::with_files(&[("a", "a\n")]).aisc(&["undo"]);
    assert_eq!(nothing.code, 1);
    assert!(nothing.stderr.contains("no recorded operations"));
}

#[test]
fn recorded_objects_survive_aggressive_gc() {
    let fixture = dirty();
    fixture
        .aisc(&["commit", "-b", "topic", "-m", "Edit f", "f.txt"])
        .ok();
    let commit = fixture.git(&["rev-parse", "topic"]);
    fixture.aisc(&["undo"]).ok();
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now", "-q"]);
    assert!(
        fixture.git_ok(&["cat-file", "-e", &commit]),
        "undone commit kept by the oplog"
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "topic"]), commit);
    fixture.git(&["fsck", "--no-dangling"]);
}

#[test]
fn stub_verbs_are_wired_and_report_not_implemented() {
    let fixture = dirty();
    let before = fixture.snapshot();
    let verbs: [&[&str]; 1] = [&["gc"]];
    for args in verbs {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {}", run.stderr);
        assert!(
            run.stderr.contains("not implemented"),
            "{args:?}: {}",
            run.stderr
        );
    }
    assert_eq!(before, fixture.snapshot());
    // Flag surface is final: parse errors are exit 2.
    assert_eq!(fixture.aisc(&["move", "a"]).code, 2);
    assert_eq!(fixture.aisc(&["commit", "--all", "f.txt"]).code, 2);
    assert_eq!(fixture.aisc(&["pick", "a", "--to", "x", "-b", "y"]).code, 2);
    assert_eq!(fixture.aisc(&["sync", "--continue", "--abort"]).code, 2);
}

#[test]
fn start_defaults_to_in_place() {
    let fixture = Fixture::with_files(&[("a", "a\n")]);
    let run = fixture.aisc(&["--json", "start", "task"]);
    run.ok();
    assert_eq!(run.json()["mode"], "in-place");
}

fn wait_for_exit(child: &mut std::process::Child, seconds: u64) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(
                std::os::unix::process::ExitStatusExt::signal(&status)
                    .map_or_else(|| status.code().unwrap(), |signal| 128 + signal),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn sigterm_kills_the_running_git_child_and_exits_143() {
    let fixture = dirty();
    let pidfile = fixture.directory.path().join("child.pid");
    let hook = format!("echo $$ > '{}'; sleep 30", pidfile.display());
    let env = fixture.fake_git("diff", &hook);
    let mut child = fixture.spawn(&["log"], &env_refs(&env));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pidfile.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pidfile.exists(), "fake git never started");
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: plain signal delivery to a process we spawned.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(wait_for_exit(&mut child, 10), Some(143));
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: signal 0 only probes for existence.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the orphaned git child must be killed");
}

#[test]
fn signal_during_the_ref_transaction_is_deferred_until_it_completes() {
    let fixture = dirty();
    let env = fixture.fake_git("update-ref", "kill -TERM $PPID; sleep 0.3");
    let mut child = fixture.spawn(
        &["commit", "-b", "topic", "-m", "mine", "f.txt"],
        &env_refs(&env),
    );
    assert_eq!(wait_for_exit(&mut child, 20), Some(143));
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic");
    assert_eq!(fixture.git(&["log", "-1", "--format=%s", "topic"]), "mine");
    assert!(
        !fixture.git(&["status", "--porcelain"]).contains(" f.txt"),
        "index installed"
    );
    assert!(!fixture.exists(".git/index.lock") && !fixture.exists(".git/aisc-operation.lock"));
}

#[test]
fn closed_stdout_exits_quietly() {
    let fixture = dirty();
    let mut child = fixture.spawn(&["log"], &[]);
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_ne!(output.status.code(), Some(101), "must not panic on EPIPE");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
}

#[test]
fn hostile_characters_are_escaped_in_text_output() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("evil\u{202e}name.txt", "x\n");
    fixture.write("a.txt", "a\n\u{202e}reversed text\n");
    fixture.write_bytes("c1.txt", "carriage\u{85}return\n".as_bytes());
    fixture.git(&[
        "commit",
        "--allow-empty",
        "-m",
        "subject \u{85} with C1 and \u{2066}isolate",
    ]);
    let log = fixture.aisc(&["log"]);
    log.ok();
    for hostile in ['\u{202e}', '\u{85}', '\u{2066}', '\u{1b}'] {
        assert!(
            !log.stdout.contains(hostile),
            "{hostile:?} leaked: {}",
            log.stdout
        );
    }
    assert!(log.stdout.contains("\\u{202e}"), "{}", log.stdout);
    assert!(log.stdout.contains("\\u{85}"), "{}", log.stdout);
}
