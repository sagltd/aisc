//! `aisc commit -b NEW --onto BASE`: a parallel branch in one call. HEAD stays where it is; the
//! new branch is BASE plus one commit with the selected changes, which leave the worktree.
mod support;

use support::{Fixture, env_refs, hunk_id};

const APP: &str = "def main():\n    run()\n";
const APP_WIP: &str = "def main():\n    log('start')\n    run()\n";
const GUIDE: &str = "# Guide\n\nInstall it.\nRun it.\nDone.\n";
const GUIDE_EDIT: &str = "# Guide\n\nInstall it with pip.\nRun it.\nDone.\n";
const HELPER: &str = "def helper():\n    return 1\n";

/// main: app.py and docs/guide.md; feature: one commit adding helper.py; the worktree edits
/// app.py (WIP) and docs/guide.md.
fn parallel() -> Fixture {
    let fixture = Fixture::with_files(&[("app.py", APP), ("docs/guide.md", GUIDE)]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write("helper.py", HELPER);
    fixture.commit_all("add helper");
    fixture.write("app.py", APP_WIP);
    fixture.write("docs/guide.md", GUIDE_EDIT);
    fixture
}

fn head_branch(fixture: &Fixture) -> String {
    fixture.git(&["symbolic-ref", "--short", "HEAD"])
}

/// What git's three commands leave: `switch -c docs main; commit -- docs/guide.md; switch feature`.
fn git_end_state() -> Fixture {
    let fixture = parallel();
    fixture.git(&["switch", "-q", "-c", "docs", "main"]);
    fixture.git(&["commit", "-q", "-m", "update guide", "--", "docs/guide.md"]);
    fixture.git(&["switch", "-q", "feature"]);
    fixture
}

fn tree(fixture: &Fixture, revision: &str) -> String {
    fixture.git(&["ls-tree", "-r", revision])
}

#[test]
fn one_call_reaches_gits_three_command_end_state() {
    let fixture = parallel();
    let feature = fixture.git(&["rev-parse", "feature"]);
    let run = fixture.aisc(&[
        "commit",
        "-b",
        "docs",
        "--onto",
        "main",
        "-m",
        "update guide",
        "docs/guide.md",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert_eq!(run.stdout.trim().lines().count(), 1, "{}", run.stdout);
    assert!(
        run.stdout.contains("docs") && run.stdout.contains("update guide"),
        "{}",
        run.stdout
    );
    let git = git_end_state();
    assert_eq!(head_branch(&fixture), "feature");
    assert_eq!(fixture.git(&["rev-parse", "feature"]), feature);
    assert_eq!(
        fixture.git(&["rev-parse", "docs^"]),
        fixture.git(&["rev-parse", "main"])
    );
    assert_eq!(fixture.subjects("main..docs"), ["update guide"]);
    assert_eq!(tree(&fixture, "docs"), tree(&git, "docs"));
    assert_eq!(fixture.read("docs/guide.md"), GUIDE);
    assert_eq!(fixture.read("app.py"), APP_WIP);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        git.git(&["status", "--porcelain"])
    );
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M app.py");
}

#[test]
fn json_names_the_branch_the_base_and_the_commit() {
    let fixture = parallel();
    let run = fixture.aisc(&[
        "commit",
        "--json",
        "-b",
        "docs",
        "--onto",
        "main",
        "-m",
        "update guide",
        "docs/guide.md",
    ]);
    let value = run.json();
    assert_eq!(value["branch"], "docs");
    assert_eq!(value["onto"], "main");
    assert_eq!(value["base"], fixture.git(&["rev-parse", "main"]));
    assert_eq!(value["new"], fixture.git(&["rev-parse", "docs"]));
    assert_eq!(value["committed"][0]["subject"], "update guide");
    assert_eq!(value["removed"][0], "docs/guide.md");
    assert_eq!(value["kept"], false);
    assert_eq!(value["head_changed_to"], serde_json::Value::Null);
}

#[test]
fn a_change_that_does_not_apply_onto_the_base_is_refused() {
    let fixture = Fixture::with_files(&[("app.py", APP), ("docs/guide.md", GUIDE)]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write(
        "docs/guide.md",
        "# Guide\n\nInstall it now.\nRun it.\nDone.\n",
    );
    fixture.commit_all("reword install");
    // The edit rewrites the line feature changed, which main does not have.
    fixture.write("docs/guide.md", GUIDE_EDIT);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "commit",
        "-b",
        "docs",
        "--onto",
        "main",
        "-m",
        "update guide",
        "docs/guide.md",
    ]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("main"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn undo_deletes_the_branch_and_puts_the_change_back() {
    let fixture = parallel();
    fixture.git(&["add", "docs/guide.md"]);
    let refs = fixture.git(&["for-each-ref", "refs/heads"]);
    fixture
        .aisc(&[
            "commit",
            "-b",
            "docs",
            "--onto",
            "main",
            "-m",
            "update guide",
            "docs/guide.md",
        ])
        .ok();
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M app.py");
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["for-each-ref", "refs/heads"]), refs);
    assert_eq!(fixture.read("docs/guide.md"), GUIDE_EDIT);
    assert_eq!(fixture.read("app.py"), APP_WIP);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M app.py\nM  docs/guide.md"
    );
}

#[test]
fn a_crash_after_the_reference_transaction_rolls_forward() {
    let fixture = parallel();
    let kill_after = "\"$REAL_GIT\" \"$@\"; kill -KILL $PPID; exit 0";
    let env = fixture.fake_git("update-ref", kill_after);
    let run = fixture.aisc_env(
        &[
            "commit",
            "-b",
            "docs",
            "--onto",
            "main",
            "-m",
            "update guide",
            "docs/guide.md",
        ],
        &env_refs(&env),
    );
    assert_eq!(
        run.code, -1,
        "expected SIGKILL: {} {}",
        run.stdout, run.stderr
    );
    assert!(fixture.exists(".git/aisc/journal.json"), "crash reproduced");
    let next = fixture.aisc(&["commit", "-m", "unrelated", "app.py"]);
    assert_eq!(next.code, 4, "{} {}", next.stdout, next.stderr);
    assert!(
        fixture.aisc_residue().is_empty(),
        "{:?}",
        fixture.aisc_residue()
    );
    assert_eq!(head_branch(&fixture), "feature");
    assert_eq!(tree(&fixture, "docs"), tree(&git_end_state(), "docs"));
    assert_eq!(fixture.read("docs/guide.md"), GUIDE);
    assert_eq!(fixture.read("app.py"), APP_WIP);
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M app.py");
}

const LONG: &str = "line01\nline02\nline03\nline04\nline05\nline06\nline07\nline08\nline09\nline10\nline11\nline12\nline13\nline14\n";
const LONG_EDIT: &str = "first edited\nline02\nline03\nline04\nline05\nline06\nline07\nline08\nline09\nline10\nline11\nline12\nline13\nlast edited\n";

fn two_hunks() -> Fixture {
    let fixture = Fixture::with_files(&[("long.txt", LONG)]);
    fixture.git(&["switch", "-q", "-c", "feature"]);
    fixture.write("helper.py", HELPER);
    fixture.commit_all("add helper");
    fixture.write("long.txt", LONG_EDIT);
    fixture
}

#[test]
fn a_line_range_takes_only_that_hunk() {
    let fixture = two_hunks();
    fixture
        .aisc(&[
            "commit",
            "-b",
            "top",
            "--onto",
            "main",
            "-m",
            "top",
            "long.txt:1",
        ])
        .ok();
    assert_eq!(
        fixture.tree_blob("top", "long.txt").unwrap(),
        LONG.replace("line01", "first edited")
    );
    assert_eq!(
        fixture.read("long.txt"),
        LONG.replace("line14", "last edited")
    );
}

#[test]
fn a_hunk_id_takes_only_that_hunk() {
    let fixture = two_hunks();
    let log = fixture.aisc(&["log", "--json"]).json();
    let id = hunk_id(&log, "long.txt", "last edited");
    fixture
        .aisc(&[
            "commit", "-b", "bottom", "--onto", "main", "-m", "bottom", &id,
        ])
        .ok();
    assert_eq!(
        fixture.tree_blob("bottom", "long.txt").unwrap(),
        LONG.replace("line14", "last edited")
    );
    assert_eq!(
        fixture.read("long.txt"),
        LONG.replace("line01", "first edited")
    );
}

#[test]
fn an_untracked_file_moves_to_the_new_branch() {
    let fixture = parallel();
    fixture.write("docs/new.md", "new page\n");
    fixture
        .aisc(&[
            "commit",
            "-b",
            "docs",
            "--onto",
            "main",
            "-m",
            "add page",
            "docs/new.md",
        ])
        .ok();
    assert_eq!(
        fixture.tree_blob("docs", "docs/new.md").unwrap(),
        "new page\n"
    );
    assert!(!fixture.exists("docs/new.md"));
    assert_eq!(fixture.read("docs/guide.md"), GUIDE_EDIT);
    assert_eq!(head_branch(&fixture), "feature");
}

#[test]
fn an_existing_branch_is_refused_unless_it_is_the_base() {
    let fixture = parallel();
    fixture.git(&["branch", "docs", "main"]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "commit",
        "-b",
        "docs",
        "--onto",
        "main",
        "-m",
        "update guide",
        "docs/guide.md",
    ]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("already exists"), "{}", run.stderr);
    assert!(run.stderr.contains("--onto docs"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    // Naming the branch as its own base commits on top of it; --existing is implied.
    fixture
        .aisc(&[
            "commit",
            "-b",
            "docs",
            "--onto",
            "docs",
            "-m",
            "update guide",
            "docs/guide.md",
        ])
        .ok();
    assert_eq!(fixture.subjects("main..docs"), ["update guide"]);
    assert_eq!(fixture.read("docs/guide.md"), GUIDE);
}

#[test]
fn keep_leaves_the_change_in_the_worktree() {
    let fixture = parallel();
    fixture
        .aisc(&[
            "commit",
            "-b",
            "docs",
            "--onto",
            "main",
            "--keep",
            "-m",
            "update guide",
            "docs/guide.md",
        ])
        .ok();
    assert_eq!(tree(&fixture, "docs"), tree(&git_end_state(), "docs"));
    assert_eq!(fixture.read("docs/guide.md"), GUIDE_EDIT);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M app.py\n M docs/guide.md"
    );
}

#[test]
fn onto_alone_commits_onto_that_existing_branch() {
    let fixture = parallel();
    fixture.git(&["branch", "docs", "main"]);
    let before = fixture.snapshot();
    let unknown = fixture.aisc(&["commit", "--onto", "nope", "-m", "x", "docs/guide.md"]);
    assert_eq!(unknown.code, 1, "{} {}", unknown.stdout, unknown.stderr);
    assert!(
        unknown.stderr.contains("unknown branch"),
        "{}",
        unknown.stderr
    );
    let not_a_branch = fixture.aisc(&["commit", "--onto", "main~0", "-m", "x", "docs/guide.md"]);
    assert_eq!(
        not_a_branch.code, 1,
        "{} {}",
        not_a_branch.stdout, not_a_branch.stderr
    );
    assert!(
        not_a_branch.stderr.contains("-b NEW"),
        "{}",
        not_a_branch.stderr
    );
    let new_unknown = fixture.aisc(&[
        "commit",
        "-b",
        "x",
        "--onto",
        "nope",
        "-m",
        "x",
        "docs/guide.md",
    ]);
    assert_eq!(
        new_unknown.code, 1,
        "{} {}",
        new_unknown.stdout, new_unknown.stderr
    );
    assert_eq!(before, fixture.snapshot());
    // The common case: commit onto an existing branch without leaving this one.
    fixture
        .aisc(&[
            "commit",
            "--onto",
            "docs",
            "-m",
            "update guide",
            "docs/guide.md",
        ])
        .ok();
    assert_eq!(fixture.subjects("main..docs"), ["update guide"]);
    assert_eq!(fixture.read("docs/guide.md"), GUIDE);
    assert_eq!(head_branch(&fixture), "feature");
}

/// `l1`..`l30` with `edits` (1-based line, text) applied.
fn numbered(edits: &[(usize, &str)]) -> String {
    (1..=30)
        .map(|n| {
            edits
                .iter()
                .find(|(line, _)| *line == n)
                .map_or_else(|| format!("l{n}\n"), |(_, text)| format!("{text}\n"))
        })
        .collect()
}

#[test]
fn a_staged_hunk_elsewhere_in_the_file_stays_staged() {
    // As after `git add -p`: line 3 staged, line 25 only in the worktree.
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(3, "STAGED3")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(3, "STAGED3"), (25, "WT25")]));
    let run = fixture.aisc(&[
        "commit", "-b", "side", "--onto", "main", "-m", "wt25", "f:25",
    ]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(
        fixture.tree_blob("side", "f").unwrap(),
        numbered(&[(25, "WT25")])
    );
    assert_eq!(fixture.read("f"), numbered(&[(3, "STAGED3")]));
    assert_eq!(
        fixture.git(&["show", ":f"]) + "\n",
        numbered(&[(3, "STAGED3")])
    );
}

#[test]
fn head_and_option_like_names_are_refused() {
    let fixture = parallel();
    let before = fixture.snapshot();
    for name in ["HEAD", "-x"] {
        let run = fixture.aisc(&[
            "commit",
            "-b",
            name,
            "--onto",
            "main",
            "-m",
            "t",
            "docs/guide.md",
        ]);
        assert_ne!(run.code, 0, "{name}: {} {}", run.stdout, run.stderr);
        assert_eq!(before, fixture.snapshot(), "{name}");
    }
}

#[test]
fn a_name_that_conflicts_with_an_existing_branch_says_so() {
    let fixture = parallel();
    fixture.git(&["branch", "a/b", "main"]);
    fixture.git(&["branch", "c", "main"]);
    let before = fixture.snapshot();
    for (name, existing) in [("a", "refs/heads/a/b"), ("c/d", "refs/heads/c")] {
        let run = fixture.aisc(&[
            "commit",
            "-b",
            name,
            "--onto",
            "main",
            "-m",
            "t",
            "docs/guide.md",
        ]);
        assert_eq!(run.code, 1, "{name}: {} {}", run.stdout, run.stderr);
        assert!(
            run.stderr.contains("conflicts with") && run.stderr.contains(existing),
            "{name}: {}",
            run.stderr
        );
        assert_eq!(before, fixture.snapshot(), "{name}");
    }
}

#[test]
fn a_branch_named_like_a_tag_is_created_with_a_warning() {
    let fixture = parallel();
    fixture.git(&["tag", "v1", "main"]);
    let run = fixture.aisc(&[
        "commit",
        "-b",
        "v1",
        "--onto",
        "main",
        "-m",
        "update guide",
        "docs/guide.md",
    ]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("ambiguous"), "{}", run.stderr);
    assert_eq!(fixture.subjects("main..refs/heads/v1"), ["update guide"]);
}

#[test]
fn a_staged_version_that_differs_from_both_is_never_dropped() {
    // Staged S5, then the same line edited again to W5: the selection covers the whole file.
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(5, "S5")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(5, "W5")]));
    let before = fixture.snapshot();
    let run = fixture.aisc(&["commit", "-b", "side", "--onto", "main", "-m", "t", "f:5"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("staged") && run.stderr.contains("git add"),
        "{}",
        run.stderr
    );
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn a_file_changed_between_read_and_onto_commit_is_a_stale_error() {
    let fixture = parallel();
    // The edit lands after aisc read the diff and before it hashes the file.
    let env = fixture.fake_git_after("diff", "printf 'more\\n' >> docs/guide.md");
    let run = fixture.aisc_env(
        &[
            "commit",
            "-b",
            "docs",
            "--onto",
            "main",
            "-m",
            "update guide",
            "docs/guide.md",
        ],
        &env_refs(&env),
    );
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(fixture.read("docs/guide.md").ends_with("more\n"));
    assert!(fixture.git(&["branch", "--list", "docs"]).is_empty());
}
