//! Repositories and settings the in-process fast paths must hand to Git (or match Git on):
//! SHA-256 object storage, dates Git refuses, clean filters, and the ids a summary must print.
mod support;

use support::Fixture;

/// A SHA-256 repository with one `base` commit.
fn sha256(files: &[(&str, &str)]) -> Fixture {
    let fixture = Fixture::bare();
    std::fs::remove_dir_all(fixture.path(".git")).unwrap();
    fixture.git(&["init", "-q", "-b", "main", "--object-format=sha256"]);
    fixture.git(&["config", "user.name", "Fixture"]);
    fixture.git(&["config", "user.email", "fixture@example.invalid"]);
    for (path, content) in files {
        fixture.write(path, content);
    }
    fixture.commit_all("base");
    fixture
}

#[test]
fn sha256_repositories_commit_amend_and_undo() {
    let fixture = sha256(&[
        ("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n"),
        ("b.txt", "b\n"),
    ]);
    fixture.write("b.txt", "b2\n");
    fixture.aisc(&["commit", "-m", "whole", "b.txt"]).ok();
    fixture.write("a.txt", "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\ntwelve\n");
    fixture.aisc(&["commit", "-m", "part", "a.txt~one"]).ok();
    assert_eq!(fixture.subjects("HEAD"), ["part", "whole", "base"]);
    assert!(
        fixture
            .tree_blob("HEAD", "a.txt")
            .unwrap()
            .starts_with("one\n")
    );
    assert!(
        fixture
            .tree_blob("HEAD", "a.txt")
            .unwrap()
            .ends_with("\n12\n")
    );
    fixture.aisc(&["amend", "--into", "whole", "a.txt"]).ok();
    assert_eq!(fixture.subjects("HEAD"), ["part", "whole", "base"]);
    assert!(
        fixture
            .tree_blob("HEAD~1", "a.txt")
            .unwrap()
            .ends_with("twelve\n")
    );
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M a.txt");
    fixture.git(&["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn a_date_git_refuses_is_refused() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.write("f.txt", "two\n");
    let before = fixture.snapshot();
    // One hour before the epoch: `git var` refuses it, so must aisc.
    let run = fixture.aisc_env(
        &["commit", "-m", "x", "f.txt"],
        &[
            ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00+01:00"),
            ("GIT_AUTHOR_NAME", "A"),
            ("GIT_AUTHOR_EMAIL", "a@example.invalid"),
        ],
    );
    assert_ne!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(before.refs, fixture.snapshot().refs);
    fixture.git(&["fsck", "--strict", "--no-dangling"]);
}

#[test]
fn a_file_a_clean_filter_reports_modified_is_not_overwritten() {
    let fixture = Fixture::with_files(&[("README", "r\n")]);
    // A clean filter that is not idempotent: the stored blob of "x" is "xx".
    fixture.git(&["config", "filter.grow.clean", "sed s/x/xx/"]);
    fixture.write(".gitattributes", "*.txt filter=grow\n");
    fixture.write("f.txt", "x\n");
    fixture.commit_all("filtered");
    fixture.git(&["switch", "-q", "-c", "other"]);
    fixture.write("f.txt", "y\n");
    fixture.commit_all("other");
    fixture.git(&["switch", "-q", "main"]);
    // The worktree bytes equal the stored blob, yet Git sees a modification.
    fixture.write("f.txt", "xx\n");
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M f.txt");
    let run = fixture.aisc(&["switch", "other"]);
    assert_ne!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("f.txt"), "xx\n", "the user's edit is kept");
    assert_eq!(fixture.git(&["symbolic-ref", "--short", "HEAD"]), "main");
}

#[test]
fn moved_branches_print_their_new_tip() {
    let fixture = Fixture::with_files(&[("f.txt", "one\n")]);
    fixture.git(&["switch", "-q", "-c", "topic"]);
    fixture.write("a.txt", "a\n");
    fixture.commit_all("add a");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("add b");
    let squash = fixture.aisc(&["squash", "--into", "a and b", "add a", "add b"]);
    squash.ok();
    assert!(
        squash.stdout.lines().any(|line| line.starts_with("topic ")
            && line.contains('→')
            && line.ends_with(" a and b")),
        "squash names the old and new tip: {}",
        squash.stdout
    );
    let undo = fixture.aisc(&["undo"]);
    undo.ok();
    let tip = fixture.git(&["rev-parse", "HEAD"]);
    // The undone op, then the restored tip: nothing an earlier command already reported.
    assert_eq!(
        undo.stdout.lines().collect::<Vec<_>>()[1..],
        [format!("topic {} add b", &tip[..7])],
        "undo names the restored tip: {}",
        undo.stdout
    );
    assert!(undo.stdout.starts_with("undid squash "), "{}", undo.stdout);
    let switch = fixture.aisc(&["switch", "main"]);
    switch.ok();
    let main = fixture.git(&["rev-parse", "HEAD"]);
    assert!(
        switch.stdout.contains(&main[..7]),
        "switch names the tip it moved to: {}",
        switch.stdout
    );
}

#[test]
fn an_attributes_file_only_in_the_index_still_filters_the_hash() {
    let fixture = Fixture::with_files(&[("README", "r\n")]);
    fixture.git(&["config", "filter.shift.clean", "tr a-y b-z"]);
    fixture.write(".gitattributes", "f.txt filter=shift\n");
    fixture.write("f.txt", "xyy\n");
    fixture.commit_all("filtered");
    fixture.git(&["switch", "-q", "-c", "mod"]);
    fixture.write("f.txt", "abc\n");
    fixture.commit_all("mod");
    fixture.git(&["switch", "-q", "main"]);
    // The worktree holds the stored blob's bytes; the rules now exist only in the index.
    fixture.git(&["checkout", "--", "f.txt"]);
    std::fs::remove_file(fixture.path(".gitattributes")).unwrap();
    assert_eq!(fixture.read("f.txt"), "yzz\n");
    assert!(fixture.git(&["status", "--porcelain"]).contains(" M f.txt"));
    let run = fixture.aisc(&["switch", "mod"]);
    assert_ne!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("f.txt"), "yzz\n", "the modified file is kept");
}

#[test]
fn git_answers_are_remembered_across_fresh_repositories() {
    let first = Fixture::with_files(&[("a.txt", "a\n")]);
    let second = Fixture::with_files(&[("a.txt", "a\n")]);
    let cache = first.directory.path().join("shared-cache");
    let cache = cache.to_str().unwrap();
    let trace = |fixture: &Fixture| {
        let file = fixture.directory.path().join("trace");
        fixture.write("a.txt", "a2\n");
        let run = fixture.aisc_env(
            &["commit", "-m", "edit", "a.txt"],
            &[
                ("XDG_CACHE_HOME", cache),
                ("GIT_TRACE", file.to_str().unwrap()),
            ],
        );
        assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
        std::fs::read_to_string(file).unwrap_or_default()
    };
    assert!(
        trace(&first).contains(" var "),
        "the first repository asks Git"
    );
    // A fresh repository with an empty $GIT_COMMON_DIR/aisc reuses the user cache.
    assert!(!trace(&second).contains(" var "));
}

#[test]
fn quiet_undo_prints_one_line_even_with_the_stack_view() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("a.txt", "a2\n");
    fixture.aisc(&["commit", "-q", "-m", "two", "a.txt"]).ok();
    let run = fixture.aisc(&["undo", "--stack", "-q"]);
    run.ok();
    assert_eq!(run.stdout.trim().lines().count(), 1, "{}", run.stdout);
    assert!(run.stdout.starts_with("undid commit"), "{}", run.stdout);
}
