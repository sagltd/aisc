//! `aisc reword`, `aisc drop` and `aisc move --to`.
mod support;

use support::Fixture;

/// main: base; feature: A (a.txt), B (b.txt), C (c.txt); an uncommitted README edit and an
/// untracked notes.md.
fn three() -> Fixture {
    let fixture = Fixture::with_files(&[("README.md", "readme\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    for (file, subject) in [
        ("a.txt", "A"),
        ("b.txt", "Fix tokne refresh"),
        ("c.txt", "C"),
    ] {
        fixture.write(file, &format!("{subject}\n"));
        fixture.commit_all(subject);
    }
    fixture.write("README.md", "readme\nedit\n");
    fixture.write("notes.md", "mine\n");
    fixture
}

fn trees(fixture: &Fixture) -> Vec<String> {
    (0..3)
        .map(|k| fixture.git(&["rev-parse", &format!("feature~{k}^{{tree}}")]))
        .collect()
}

#[test]
fn reword_changes_one_message_and_nothing_else() {
    let fixture = three();
    let before = trees(&fixture);
    let author = fixture.git(&["log", "--format=%an %ae %ad", "-1", "feature~1"]);
    let run = fixture.aisc(&["reword", "Fix tokne refresh", "-m", "Fix token refresh"]);
    run.ok();
    assert!(
        run.stdout.contains("to \"Fix token refresh\""),
        "{}",
        run.stdout
    );
    assert_eq!(
        fixture.subjects("main..feature"),
        ["C", "Fix token refresh", "A"]
    );
    assert_eq!(trees(&fixture), before);
    assert_eq!(
        fixture.git(&["log", "--format=%an %ae %ad", "-1", "feature~1"]),
        author
    );
    assert_eq!(fixture.read("README.md"), "readme\nedit\n");
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
    fixture.aisc(&["undo"]).ok();
    assert_eq!(
        fixture.subjects("main..feature"),
        ["C", "Fix tokne refresh", "A"]
    );
}

#[test]
fn reword_another_branch_and_refuse_an_empty_message() {
    let fixture = three();
    fixture.git(&["switch", "-q", "-c", "other", "main"]);
    let run = fixture.aisc(&["reword", "--branch", "feature", "A", "-m", "Add a"]);
    run.ok();
    assert_eq!(
        fixture.subjects("main..feature"),
        ["C", "Fix tokne refresh", "Add a"]
    );
    let empty = fixture.aisc(&["reword", "--branch", "feature", "C", "-m", "  "]);
    assert_eq!(empty.code, 1, "{}", empty.stderr);
}

#[test]
fn drop_removes_a_commit_and_its_changes_keeping_uncommitted_work() {
    let fixture = three();
    let run = fixture.aisc(&["drop", "Fix tokne refresh"]);
    run.ok();
    assert_eq!(fixture.subjects("main..feature"), ["C", "A"]);
    assert!(!fixture.exists("b.txt"));
    assert_eq!(fixture.read("c.txt"), "C\n");
    assert_eq!(fixture.read("README.md"), "readme\nedit\n");
    assert_eq!(fixture.read("notes.md"), "mine\n");
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M README.md\n?? notes.md"
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(
        fixture.subjects("main..feature"),
        ["C", "Fix tokne refresh", "A"]
    );
    assert_eq!(fixture.read("b.txt"), "Fix tokne refresh\n");
}

#[test]
fn drop_refuses_when_later_commits_depend_on_it() {
    let fixture = Fixture::with_files(&[("a.txt", "1\n")]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write("a.txt", "2\n");
    fixture.commit_all("two");
    fixture.write("a.txt", "3\n");
    fixture.commit_all("three");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["drop", "two"]);
    assert_ne!(run.code, 0, "{}", run.stdout);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn move_to_puts_commits_on_another_branch_in_one_call() {
    let fixture = Fixture::with_files(&[("README.md", "readme\n")]);
    fixture.git(&["switch", "-q", "-c", "billing"]);
    fixture.write("invoice.txt", "invoice\n");
    fixture.commit_all("Add invoice");
    fixture.git(&["switch", "-q", "-c", "feature", "main"]);
    for (file, subject) in [
        ("a.txt", "A"),
        ("b.txt", "Fix tokne refresh"),
        ("c.txt", "C"),
    ] {
        fixture.write(file, &format!("{subject}\n"));
        fixture.commit_all(subject);
    }
    fixture.write("README.md", "readme\nedit\n");
    let run = fixture.aisc(&["move", "Fix tokne refresh", "--to", "billing"]);
    run.ok();
    assert_eq!(fixture.subjects("main..feature"), ["C", "A"]);
    assert_eq!(
        fixture.subjects("main..billing"),
        ["Fix tokne refresh", "Add invoice"]
    );
    assert!(!fixture.exists("b.txt"));
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );
    assert_eq!(fixture.read("README.md"), "readme\nedit\n");
    fixture.aisc(&["undo"]).ok();
    assert_eq!(
        fixture.subjects("main..feature"),
        ["C", "Fix tokne refresh", "A"]
    );
    assert_eq!(fixture.subjects("main..billing"), ["Add invoice"]);
}

#[test]
fn move_to_refuses_unknown_same_or_invalid_branches_and_conflicts_change_nothing() {
    let fixture = three();
    let before = fixture.snapshot();
    for (args, expected) in [
        (&["move", "A", "--to", "nope"][..], "unknown branch"),
        (&["move", "A", "--to", "feature"][..], "already on"),
        (
            &["move", "A", "--to", "feature^"][..],
            "invalid branch name",
        ),
        (
            &["move", "A", "--to", "feature@{1}"][..],
            "invalid branch name",
        ),
    ] {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains(expected), "{args:?}: {}", run.stderr);
        assert_eq!(fixture.snapshot(), before, "{args:?}");
    }
    fixture.git(&["switch", "-q", "-c", "clash", "main"]);
    fixture.write("b.txt", "different\n");
    fixture.commit_all("clash");
    fixture.git(&["switch", "-q", "feature"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "Fix tokne refresh", "--to", "clash"]);
    assert_ne!(run.code, 0, "{}", run.stdout);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn move_to_refuses_a_target_checked_out_here_or_in_another_worktree() {
    let fixture = three();
    fixture.git(&["branch", "billing", "main"]);
    let linked = fixture.root.parent().unwrap().join("billing-wt");
    fixture.git(&["worktree", "add", "-q", linked.to_str().unwrap(), "billing"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "A", "--to", "billing"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("another worktree"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    fixture.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
    // The target is this worktree's own branch: a different message, same refusal.
    fixture.git(&["switch", "-q", "billing"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "--branch", "feature", "A", "--to", "billing"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("checked out here"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn drop_and_move_to_on_another_branch_still_take_the_index_lock() {
    // The worktree is carried only when the branch is checked out, but which branch that is
    // can change between reads: the index lock is taken up front, every time.
    let fixture = three();
    fixture.git(&["branch", "billing", "main"]);
    fixture.git(&["switch", "-q", "-c", "other", "main"]);
    fixture.write(".git/index.lock", "");
    let before = fixture.snapshot();
    for args in [
        &["drop", "--branch", "feature", "Fix tokne refresh"][..],
        &[
            "move",
            "--branch",
            "feature",
            "Fix tokne refresh",
            "--to",
            "billing",
        ][..],
    ] {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains("locked"), "{args:?}: {}", run.stderr);
        assert_eq!(fixture.snapshot(), before, "{args:?}");
    }
    assert!(
        fixture.exists(".git/index.lock"),
        "a foreign lock is never removed"
    );
}

#[test]
fn move_to_a_branch_inside_the_rewritten_range_is_a_typed_refusal() {
    let fixture = three();
    // `stack` sits at B, so it already holds A: refused as a duplicate before anything is written.
    fixture.git(&["branch", "stack", "feature~1"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "A", "--to", "stack"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("already contains"), "{}", run.stderr);
    assert!(!run.stderr.contains("fatal:"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    // `stack` at the tip holds every commit, so the same refusal; it is also the ref that a
    // move of A would update twice (rewritten C, plus A on top), and neither path writes.
    fixture.git(&["branch", "-f", "stack", "feature"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "A", "--to", "stack"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(!run.stderr.contains("fatal:"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn drop_never_loses_an_uncommitted_edit_to_a_dropped_file() {
    let fixture = three();
    fixture.write("b.txt", "Fix tokne refresh\nmy unsaved line\n");
    let run = fixture.aisc(&["drop", "Fix tokne refresh"]);
    let kept = fixture.exists("b.txt") && fixture.read("b.txt").contains("my unsaved line")
        || fixture.exists("b.txt.aisc-conflict")
            && fixture
                .read("b.txt.aisc-conflict")
                .contains("my unsaved line");
    assert!(kept, "{} {}", run.stdout, run.stderr);
    if run.code != 0 {
        assert_eq!(
            fixture.subjects("main..feature"),
            ["C", "Fix tokne refresh", "A"]
        );
    }
}

#[test]
fn move_to_refuses_a_target_that_already_contains_the_change() {
    let fixture = Fixture::with_files(&[("README.md", "readme\n")]);
    fixture.git(&["switch", "-q", "-c", "billing"]);
    fixture.write("invoice.txt", "invoice\n");
    fixture.commit_all("Add invoice");
    fixture.write("b.txt", "B\n");
    fixture.commit_all("Add b elsewhere");
    fixture.git(&["switch", "-q", "-c", "feature", "main"]);
    for (file, subject) in [("a.txt", "A"), ("b.txt", "B"), ("c.txt", "C")] {
        fixture.write(file, &format!("{subject}\n"));
        fixture.commit_all(subject);
    }
    let before = fixture.snapshot();
    // Replaying B onto billing changes nothing there: an empty duplicate, not a move.
    let run = fixture.aisc(&["move", "B", "--to", "billing"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("already contains"), "{}", run.stderr);
    assert!(run.stderr.contains("aisc drop"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        fixture.subjects("main..billing"),
        ["Add b elsewhere", "Add invoice"]
    );
}

#[test]
fn move_to_a_new_branch_with_onto_creates_it_in_one_call() {
    let fixture = three();
    let tip_before = fixture.git(&["rev-parse", "feature"]);
    let run = fixture.aisc(&[
        "move",
        "A",
        "Fix tokne refresh",
        "--to",
        "export",
        "--onto",
        "main",
    ]);
    run.ok();
    assert!(run.stdout.contains("export"), "{}", run.stdout);
    assert_eq!(fixture.subjects("main..export"), ["Fix tokne refresh", "A"]);
    assert_eq!(
        fixture.git(&["rev-parse", "export~2"]),
        fixture.git(&["rev-parse", "main"])
    );
    assert_eq!(fixture.subjects("main..feature"), ["C"]);
    assert!(!fixture.exists("a.txt") && !fixture.exists("b.txt"));
    assert!(fixture.exists("c.txt"));
    assert_eq!(fixture.read("README.md"), "readme\nedit\n");
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );
    // Undo deletes the new branch and brings the commits and their files back.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "feature"]), tip_before);
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "refs/heads/export"]));
    assert!(fixture.exists("a.txt") && fixture.exists("b.txt"));
}

#[test]
fn move_to_onto_refusals_change_nothing() {
    let fixture = three();
    fixture.git(&["branch", "billing", "main"]);
    let before = fixture.snapshot();
    for (args, expected) in [
        (
            &["move", "A", "--to", "billing", "--onto", "main"][..],
            "already exists",
        ),
        (
            &["move", "A", "--to", "export", "--onto", "nope"][..],
            "unknown revision",
        ),
        (&["move", "A", "--to", "export"][..], "--onto"),
    ] {
        let run = fixture.aisc(args);
        assert_eq!(run.code, 1, "{args:?}: {} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains(expected), "{args:?}: {}", run.stderr);
        assert_eq!(fixture.snapshot(), before, "{args:?}");
    }
}
