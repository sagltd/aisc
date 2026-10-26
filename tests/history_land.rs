//! Land and switch tests. The pilot 12 fixture mirrors vcbench pilot 12 (integrate topics) from
//! github.com/gitbutlerapp/version-control-bench (MIT): scripts/lib/scenario-integration.mjs.
mod support;

use support::Fixture;

fn commit_by(fixture: &Fixture, message: &str, name: &str) {
    fixture.git(&["add", "-A"]);
    let author = format!(
        "{name} <{}@example.test>",
        name.to_lowercase().replace(' ', ".")
    );
    fixture.git(&["commit", "-m", message, "--author", &author]);
}

fn all(run: &support::Run) -> String {
    format!("{}{}", run.stdout, run.stderr)
}

fn pilot12() -> Fixture {
    let fixture = Fixture::with_files(&[
        ("package.json", "{\"type\":\"module\"}\n"),
        ("README.md", "Small greeting example.\n"),
    ]);
    fixture.git(&["switch", "-c", "normalize"]);
    fixture.write(
        "src/normalize.js",
        "export const normalize = (name) => name.trim().toLowerCase();\n",
    );
    commit_by(&fixture, "Normalize names", "Normalize Author");
    fixture.git(&["switch", "-c", "greeting"]);
    fixture.write(
        "src/greeting.js",
        "import { normalize } from \"./normalize.js\";\nexport const greeting = (n) => `Hello, ${normalize(n)}`;\n",
    );
    commit_by(&fixture, "Greet normalized names", "Greeting Author");
    fixture.git(&["switch", "-c", "docs", "main"]);
    fixture.write(
        "docs/usage.md",
        "Call greeting(\"  ADA \") to get \"Hello, ada\".\n",
    );
    commit_by(&fixture, "Document greeting usage", "Docs Author");
    fixture.git(&["switch", "main"]);
    fixture
}

#[test]
fn pilot12_land_stack_and_docs_in_one_call() {
    let fixture = pilot12();
    let run = fixture.aisc(&["land", "greeting", "docs", "--into", "main", "--switch"]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        fixture.subjects("HEAD"),
        [
            "Document greeting usage",
            "Greet normalized names",
            "Normalize names",
            "base"
        ]
    );
    let authors = fixture.git(&["log", "--format=%an", "HEAD~3..HEAD"]);
    assert_eq!(authors, "Docs Author\nGreeting Author\nNormalize Author");
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    for path in ["src/normalize.js", "src/greeting.js", "docs/usage.md"] {
        assert!(fixture.exists(path), "{path}");
        assert_eq!(fixture.tree_blob("HEAD", path).unwrap(), fixture.read(path));
    }
    assert!(
        fixture
            .git(&["status", "--porcelain", "--untracked-files=all"])
            .is_empty()
    );
    assert!(fixture.git(&["stash", "list"]).is_empty());
    assert!(fixture.git(&["ls-files", "-u"]).is_empty());
    for marker in ["rebase-merge", "MERGE_HEAD", "CHERRY_PICK_HEAD"] {
        assert!(!fixture.exists(&format!(".git/{marker}")));
    }
    assert!(fixture.git_ok(&["merge-base", "--is-ancestor", "greeting", "main"]));
    // Undo puts everything back.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.subjects("HEAD"), ["base"]);
    assert!(!fixture.exists("src/greeting.js"));
}

#[test]
fn land_conflict_changes_nothing() {
    let fixture = Fixture::with_files(&[("a.txt", "1\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("a.txt", "topic\n");
    commit_by(&fixture, "topic edit", "T");
    fixture.git(&["switch", "main"]);
    fixture.write("a.txt", "main\n");
    commit_by(&fixture, "main edit", "M");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["land", "topic", "--into", "main"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("a.txt conflicts"), "{}", all(&run));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn land_into_other_branch_leaves_worktree_alone() {
    let fixture = pilot12();
    fixture.git(&["switch", "docs"]);
    fixture.write("docs/draft.md", "draft\n");
    fixture.git(&["branch", "release", "main"]);
    let run = fixture.aisc(&["land", "greeting", "--into", "release"]);
    run.ok();
    assert_eq!(
        fixture.subjects("release"),
        ["Greet normalized names", "Normalize names", "base"]
    );
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "docs");
    assert_eq!(fixture.read("docs/draft.md"), "draft\n");
}

#[test]
fn land_into_dirty_checked_out_target_carries_edits() {
    let fixture = pilot12();
    fixture.write("README.md", "Small greeting example.\nlocal note\n");
    fixture.aisc(&["land", "docs", "--into", "main"]).ok();
    assert!(fixture.exists("docs/usage.md"));
    assert_eq!(
        fixture.read("README.md"),
        "Small greeting example.\nlocal note\n"
    );
}

#[test]
fn switch_carries_edits() {
    let fixture = pilot12();
    fixture.git(&["switch", "docs"]);
    fixture.write("README.md", "Small greeting example.\nlocal note\n");
    fixture.write("scratch.txt", "untracked\n");
    let run = fixture.aisc(&["switch", "greeting"]);
    run.ok();
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "greeting"
    );
    assert!(fixture.exists("src/greeting.js") && !fixture.exists("docs/usage.md"));
    assert_eq!(
        fixture.read("README.md"),
        "Small greeting example.\nlocal note\n"
    );
    assert_eq!(fixture.read("scratch.txt"), "untracked\n");
    assert_eq!(fixture.git(&["diff", "--name-only"]), "README.md");
    let create = fixture.aisc(&["switch", "-c", "next"]);
    create.ok();
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "next");
    assert_eq!(
        fixture.git(&["rev-parse", "next"]),
        fixture.git(&["rev-parse", "greeting"])
    );
    assert_eq!(
        fixture.read("README.md"),
        "Small greeting example.\nlocal note\n"
    );
}

#[test]
fn switch_refuses_on_conflict_and_changes_nothing() {
    let fixture = Fixture::with_files(&[("a.txt", "one\ntwo\nthree\n")]);
    fixture.git(&["switch", "-c", "other"]);
    fixture.write("a.txt", "one\nTWO other\nthree\n");
    commit_by(&fixture, "other", "O");
    fixture.git(&["switch", "main"]);
    fixture.write("a.txt", "one\nTWO local\nthree\n");
    fixture.write("new.txt", "x\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["switch", "other"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert!(all(&run).contains("a.txt"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn switch_and_land_refuse_branches_checked_out_elsewhere() {
    let fixture = pilot12();
    let other = fixture.directory.path().join("wt-docs");
    let other = other.to_str().unwrap().to_owned();
    fixture.git(&["worktree", "add", "-q", &other, "docs"]);
    let before = fixture.snapshot();
    let switched = fixture.aisc(&["switch", "docs"]);
    assert_eq!(switched.code, 1, "{}", all(&switched));
    assert!(all(&switched).contains("another worktree"));
    let landed = fixture.aisc(&["land", "greeting", "--into", "docs"]);
    assert_eq!(landed.code, 1, "{}", all(&landed));
    assert!(all(&landed).contains("another worktree"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn switch_refuses_untracked_file_where_a_directory_is_needed() {
    let fixture = pilot12();
    fixture.write("docs", "a file named docs\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["switch", "docs"]);
    assert_eq!(run.code, 1, "{}", all(&run));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn land_delete_removes_the_landed_branches_and_undo_restores_them() {
    let fixture = pilot12();
    let before = fixture.git(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/heads",
    ]);
    let run = fixture.aisc(&[
        "land",
        "normalize",
        "greeting",
        "docs",
        "--into",
        "main",
        "--delete",
    ]);
    run.ok();
    assert!(
        run.stdout.contains("deleted normalize, greeting, docs"),
        "{}",
        run.stdout
    );
    assert_eq!(
        fixture.git(&["branch", "--format=%(refname:short)"]),
        "main"
    );
    assert_eq!(
        fixture.subjects("main"),
        [
            "Document greeting usage",
            "Greet normalized names",
            "Normalize names",
            "base"
        ]
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(
        fixture.git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads"
        ]),
        before
    );
}

#[test]
fn land_delete_refuses_the_checked_out_branch_unless_it_switches_away() {
    let fixture = pilot12();
    fixture.git(&["switch", "-q", "docs"]);
    let refused = fixture.aisc(&["land", "docs", "--into", "main", "--delete"]);
    assert_eq!(refused.code, 1, "{} {}", refused.stdout, refused.stderr);
    assert!(fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/docs"]));
    let run = fixture.aisc(&["land", "docs", "--into", "main", "--switch", "--delete"]);
    run.ok();
    assert_eq!(fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/docs"]));
}

#[test]
fn land_delete_of_already_landed_branches_and_duplicates() {
    let fixture = pilot12();
    fixture
        .aisc(&["land", "normalize", "greeting", "--into", "main"])
        .ok();
    let twice = fixture.aisc(&["land", "greeting", "greeting", "--into", "main", "--delete"]);
    assert_eq!(twice.code, 1, "{} {}", twice.stdout, twice.stderr);
    assert!(twice.stderr.contains("named twice"), "{}", twice.stderr);
    let main = fixture.git(&["rev-parse", "main"]);
    fixture
        .aisc(&[
            "land",
            "normalize",
            "greeting",
            "--into",
            "main",
            "--delete",
        ])
        .ok();
    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    assert!(!fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/greeting"]));
    fixture.aisc(&["undo"]).ok();
    assert!(fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/greeting"]));
    assert!(fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/normalize"]));
}

#[test]
fn land_delete_refuses_a_branch_being_rebased_in_another_worktree() {
    let fixture = pilot12();
    let other = fixture.directory.path().join("other");
    let other = other.to_str().unwrap();
    fixture.git(&["worktree", "add", "-q", other, "docs"]);
    // A rebase that stops leaves HEAD detached with rebase-merge/head-name naming docs.
    let admin = fixture.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let admin = std::path::Path::new(&admin).join("worktrees/other");
    std::fs::create_dir_all(admin.join("rebase-merge")).unwrap();
    std::fs::write(admin.join("rebase-merge/head-name"), "refs/heads/docs\n").unwrap();
    let docs = fixture.git(&["rev-parse", "docs"]);
    std::fs::write(admin.join("HEAD"), format!("{docs}\n")).unwrap();
    let run = fixture.aisc(&["land", "docs", "--into", "main", "--delete"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/heads/docs"]));
}

#[test]
fn land_lists_the_landed_commits_in_its_result() {
    let fixture = pilot12();
    // The checked-out branch is main here: the result names every landed commit, so the agent
    // can check messages without a second call.
    let run = fixture.aisc(&["land", "greeting", "docs", "--into", "main", "--switch"]);
    run.ok();
    for subject in [
        "Normalize names",
        "Greet normalized names",
        "Document greeting usage",
    ] {
        assert!(run.stdout.contains(subject), "{subject}: {}", run.stdout);
    }
    fixture.aisc(&["undo"]).ok();
    // Landing into a branch that is not checked out shows that branch, not this one.
    fixture.git(&["switch", "-q", "greeting"]);
    let run = fixture.aisc(&["land", "docs", "--into", "main"]);
    run.ok();
    assert!(run.stdout.contains("main: last"), "{}", run.stdout);
    assert!(
        run.stdout.contains("Document greeting usage"),
        "{}",
        run.stdout
    );
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "greeting"
    );
}
