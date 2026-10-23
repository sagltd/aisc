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
fn move_to_refuses_unknown_or_same_branch_and_conflicts_change_nothing() {
    let fixture = three();
    let before = fixture.snapshot();
    for args in [
        &["move", "A", "--to", "nope"][..],
        &["move", "A", "--to", "feature"][..],
    ] {
        let run = fixture.aisc(args);
        assert_ne!(run.code, 0, "{args:?}: {}", run.stdout);
    }
    fixture.git(&["switch", "-q", "-c", "clash", "main"]);
    fixture.write("b.txt", "different\n");
    fixture.commit_all("clash");
    fixture.git(&["switch", "-q", "feature"]);
    let before = {
        let _ = before;
        fixture.snapshot()
    };
    let run = fixture.aisc(&["move", "Fix tokne refresh", "--to", "clash"]);
    assert_ne!(run.code, 0, "{}", run.stdout);
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
