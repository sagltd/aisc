//! Discard verb tests. The pilot-9 fixture ports vcbench pilot 9 (cleanup noise) from
//! github.com/gitbutlerapp/version-control-bench `scripts/lib/scenario-cleanup.mjs` (MIT,
//! GitButler): committed sources are unformatted, a formatter run rewrites three files without
//! changing meaning, and one real edit must stay.
mod support;

use std::os::unix::fs::PermissionsExt;
use support::Fixture;

const NOISE: [&str; 3] = ["src/catalog.ts", "src/format.ts", "test/catalog.test.ts"];

const BASE: [(&str, &str); 5] = [
    ("src/retry.ts", "export const retryLimit = 1;\n"),
    ("src/catalog.ts", "export const kinds=[\"a\",\"b\"];\n"),
    (
        "src/format.ts",
        "export function pad(s:string,n:number){return s.padStart(n);}\n",
    ),
    (
        "test/catalog.test.ts",
        "import {kinds} from \"../src/catalog\";\nif(kinds.length!==2){throw new Error(\"kinds\");}\n",
    ),
    ("README.md", "Retry example.\n"),
];

const DIRTY: [(&str, &str); 4] = [
    ("src/retry.ts", "export const retryLimit = 3;\n"),
    ("src/catalog.ts", "export const kinds = [\"a\", \"b\"];\n"),
    (
        "src/format.ts",
        "export function pad(s: string, n: number) {\n  return s.padStart(n);\n}\n",
    ),
    (
        "test/catalog.test.ts",
        "import { kinds } from \"../src/catalog\";\n\nif (kinds.length !== 2) {\n  throw new Error(\"kinds\");\n}\n",
    ),
];

fn pilot9() -> Fixture {
    let fixture = Fixture::with_files(&BASE);
    fixture.git(&["switch", "-c", "fix-retry"]);
    fixture.write("src/retry.ts", "export const retryLimit = 2;\n");
    fixture.commit_all("Prepare retry configuration");
    for (path, content) in DIRTY {
        fixture.write(path, content);
    }
    fixture
}

fn base(path: &str) -> &'static str {
    BASE.iter().find(|(name, _)| *name == path).unwrap().1
}

fn dirty(path: &str) -> &'static str {
    DIRTY.iter().find(|(name, _)| *name == path).unwrap().1
}

fn assert_pilot9_final_state(fixture: &Fixture, refs: &str) {
    for path in NOISE {
        assert_eq!(fixture.read(path), base(path), "{path} is back to HEAD");
        // The index entry is the HEAD entry too (nothing pending).
        assert_eq!(
            fixture.git(&["show", &format!(":{path}")]),
            base(path).trim_end()
        );
    }
    assert_eq!(
        fixture.read("src/retry.ts"),
        dirty("src/retry.ts"),
        "the real edit stays"
    );
    assert_eq!(
        fixture.git(&["diff", "HEAD", "--name-only"]),
        "src/retry.ts"
    );
    assert_eq!(
        fixture.git(&["for-each-ref", "refs/heads", "refs/tags"]),
        refs
    );
    assert!(fixture.git(&["stash", "list"]).is_empty());
    assert!(fixture.git(&["ls-files", "-u"]).is_empty());
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
}

#[test]
fn pilot9_discard_by_path_takes_one_call_and_undo_restores_the_bytes() {
    let fixture = pilot9();
    // One noise file is staged: the whole-file selection resets the index too.
    fixture.git(&["add", "src/catalog.ts"]);
    let refs = fixture.git(&["for-each-ref", "refs/heads", "refs/tags"]);
    let run = fixture.aisc(&["discard", NOISE[0], NOISE[1], NOISE[2]]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert!(
        run.stdout
            .lines()
            .next()
            .unwrap()
            .starts_with("discarded 3 files; op "),
        "{}",
        run.stdout
    );
    assert_pilot9_final_state(&fixture, &refs);

    fixture.aisc(&["undo"]).ok();
    for path in NOISE {
        assert_eq!(
            fixture.read(path),
            dirty(path),
            "{path} restored byte for byte"
        );
    }
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        "src/catalog.ts"
    );
    assert_eq!(fixture.read("src/retry.ts"), dirty("src/retry.ts"));
}

#[test]
fn pilot9_whitespace_only_leaves_the_real_edit() {
    let fixture = pilot9();
    let refs = fixture.git(&["for-each-ref", "refs/heads", "refs/tags"]);
    let log = fixture.aisc(&["log", "--json"]);
    log.ok();
    let files = log.json()["changes"].clone();
    let ws: Vec<&str> = files
        .as_array()
        .unwrap()
        .iter()
        .filter(|file| file["ws"] == true)
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(ws, NOISE, "log flags exactly the formatter noise");
    let run = fixture.aisc(&["discard", "--whitespace-only"]);
    run.ok();
    assert_eq!(fixture.calls(), 2);
    assert_pilot9_final_state(&fixture, &refs);
    fixture.aisc(&["undo"]).ok();
    for path in NOISE {
        assert_eq!(fixture.read(path), dirty(path));
    }
}

#[test]
fn whitespace_only_with_a_path_filters_to_whitespace_hunks() {
    let fixture = pilot9();
    // A real change in the same file as noise stays.
    fixture.write("README.md", "Retry example.\n\n\nMore text.\n");
    let run = fixture.aisc(&["discard", "--whitespace-only", "src/retry.ts"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains("whitespace-only"), "{}", run.stderr);
    assert_eq!(fixture.read("src/retry.ts"), dirty("src/retry.ts"));
    fixture
        .aisc(&["discard", "--whitespace-only", "src/catalog.ts"])
        .ok();
    assert_eq!(fixture.read("src/catalog.ts"), base("src/catalog.ts"));
    assert_eq!(fixture.read("src/format.ts"), dirty("src/format.ts"));
}

const TWO_HUNKS_HEAD: &str =
    "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n";
const TWO_HUNKS_W: &str =
    "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nTWELVE\n";

#[test]
fn a_partial_discard_keeps_other_hunks_and_the_staged_state_follows() {
    let fixture = Fixture::with_files(&[("f.txt", TWO_HUNKS_HEAD), ("g.txt", TWO_HUNKS_HEAD)]);
    fixture.write("f.txt", TWO_HUNKS_W);
    fixture.write("g.txt", TWO_HUNKS_W);
    // f.txt staged equal to the worktree; g.txt staged equal to HEAD (not staged).
    fixture.git(&["add", "f.txt"]);
    let run = fixture.aisc(&["discard", "f.txt~ONE", "g.txt~TWELVE"]);
    run.ok();
    let expected_f = TWO_HUNKS_W.replace("ONE", "one");
    let expected_g = TWO_HUNKS_W.replace("TWELVE", "twelve");
    assert_eq!(fixture.read("f.txt"), expected_f);
    assert_eq!(fixture.read("g.txt"), expected_g);
    assert_eq!(
        fixture.git(&["show", ":f.txt"]),
        expected_f.trim_end(),
        "staged copy follows"
    );
    assert_eq!(
        fixture.git(&["show", ":g.txt"]),
        TWO_HUNKS_HEAD.trim_end(),
        "unstaged stays"
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        "M  f.txt\n M g.txt"
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.read("f.txt"), TWO_HUNKS_W);
    assert_eq!(fixture.read("g.txt"), TWO_HUNKS_W);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        "M  f.txt\n M g.txt"
    );
}

#[test]
fn untracked_deleted_and_mode_changes_are_reverted_by_path() {
    let fixture = Fixture::with_files(&[
        ("keep.txt", "keep\n"),
        ("gone.txt", "gone\n"),
        ("run.sh", "echo\n"),
    ]);
    fixture.write("scratch.txt", "scratch\n");
    std::fs::remove_file(fixture.path("gone.txt")).unwrap();
    std::fs::set_permissions(
        fixture.path("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture.write("keep.txt", "keep\nmore\n");
    fixture.git(&["add", "keep.txt"]);
    let run = fixture.aisc(&["discard", "scratch.txt", "gone.txt", "run.sh", "keep.txt"]);
    run.ok();
    assert!(!fixture.exists("scratch.txt"));
    assert_eq!(fixture.read("gone.txt"), "gone\n");
    assert_eq!(fixture.read("keep.txt"), "keep\n");
    let mode = std::fs::metadata(fixture.path("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn undo_restores_the_executable_bit_of_a_mode_only_discard() {
    let fixture = Fixture::with_files(&[("run.sh", "echo\n")]);
    std::fs::set_permissions(
        fixture.path("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture.aisc(&["discard", "run.sh"]).ok();
    let mode = |f: &Fixture| {
        std::fs::metadata(f.path("run.sh"))
            .unwrap()
            .permissions()
            .mode()
    };
    assert_eq!(mode(&fixture) & 0o111, 0);
    fixture.aisc(&["undo"]).ok();
    assert_ne!(mode(&fixture) & 0o111, 0, "exec bit is back");
}

#[test]
fn an_untracked_file_changed_between_read_and_discard_is_a_stale_error() {
    let fixture = pilot9();
    fixture.write("scratch.txt", "scratch\n");
    let env = fixture.fake_git("ls-tree", "printf 'sneaky\\n' >> scratch.txt");
    let run = fixture.aisc_env(&["discard", "scratch.txt"], &support::env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("scratch.txt"), "scratch\nsneaky\n");
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

#[test]
fn an_intent_to_add_file_is_removed_with_its_placeholder() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("new.txt", "new\n");
    fixture.git(&["add", "-N", "new.txt"]);
    fixture.aisc(&["discard", "new.txt"]).ok();
    assert!(!fixture.exists("new.txt"));
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn undo_of_discarding_an_intent_to_add_file_restores_the_placeholder() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n"), ("b.txt", "b\n")]);
    fixture.write("b.txt", "b\nstaged\n");
    fixture.git(&["add", "b.txt"]);
    fixture.write("n.txt", "new\n");
    fixture.git(&["add", "-N", "n.txt"]);
    fixture.aisc(&["discard", "n.txt"]).ok();
    assert!(!fixture.exists("n.txt"));
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.read("n.txt"), "new\n");
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        "M  b.txt\n A n.txt"
    );
    assert_eq!(fixture.git(&["show", ":b.txt"]), "b\nstaged");
    // Redo the discard.
    fixture.aisc(&["undo"]).ok();
    assert!(!fixture.exists("n.txt"));
    assert_eq!(fixture.git(&["status", "--porcelain"]), "M  b.txt");
}

#[test]
fn failures_change_nothing() {
    let fixture = pilot9();
    fixture.git(&["add", "src/catalog.ts"]);
    let before = fixture.snapshot();
    let unknown = fixture.aisc(&["discard", "nope.txt"]);
    assert_eq!(unknown.code, 1);
    let none = fixture.aisc(&["discard"]);
    assert_eq!(none.code, 1);
    let missing_text = fixture.aisc(&["discard", "src/retry.ts~absent"]);
    assert_eq!(missing_text.code, 1);
    let other = fixture.aisc(&["discard", "--branch", "main", "src/retry.ts"]);
    assert_eq!(other.code, 1);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn a_file_changed_between_read_and_discard_is_a_stale_error() {
    let fixture = pilot9();
    let env = fixture.fake_git("update-index", "printf 'sneaky\\n' >> src/format.ts");
    let run = fixture.aisc_env(&["discard", "src/format.ts"], &support::env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(
        fixture.read("src/format.ts").ends_with("sneaky\n"),
        "the user's newer bytes are kept"
    );
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

#[test]
fn a_file_changed_after_its_hash_was_checked_is_a_stale_error() {
    // The hook runs after the hash check, when the HEAD blobs are read.
    let fixture = pilot9();
    let env = fixture.fake_git("cat-file", "printf 'sneaky\\n' >> src/format.ts");
    let run = fixture.aisc_env(&["discard", "src/format.ts"], &support::env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(
        fixture.read("src/format.ts").ends_with("sneaky\n"),
        "the user's newer bytes are kept"
    );
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

#[test]
fn an_untracked_file_changed_after_its_digest_was_checked_is_a_stale_error() {
    let fixture = pilot9();
    fixture.write("scratch.txt", "scratch\n");
    let env = fixture.fake_git("cat-file", "printf 'sneaky\\n' >> scratch.txt");
    let run = fixture.aisc_env(
        &["discard", "scratch.txt", "src/format.ts"],
        &support::env_refs(&env),
    );
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("scratch.txt"), "scratch\nsneaky\n");
    assert_eq!(fixture.read("src/format.ts"), dirty("src/format.ts"));
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

/// Larger than the 16 MiB limit above which the listing records only the size.
const BIG: usize = 17 * 1024 * 1024;

#[test]
fn a_large_untracked_file_rewritten_in_place_is_a_stale_error() {
    let fixture = pilot9();
    fixture.write_bytes("big.bin", &vec![b'x'; BIG]);
    // Same size, different bytes: only the content (or its change time) can tell.
    let hook = "sleep 0.05; printf 'Z' | dd of=big.bin bs=1 count=1 conv=notrunc 2>/dev/null";
    let env = fixture.fake_git("ls-tree", hook);
    let run = fixture.aisc_env(&["discard", "big.bin"], &support::env_refs(&env));
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    let bytes = std::fs::read(fixture.path("big.bin")).unwrap();
    assert_eq!(bytes.len(), BIG);
    assert_eq!(bytes[0], b'Z', "the concurrent edit is kept");
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

#[test]
fn a_large_untracked_file_that_grows_is_a_stale_error() {
    let fixture = pilot9();
    fixture.write_bytes("big.bin", &vec![b'x'; BIG]);
    let env = fixture.fake_git("cat-file", "printf 'more' >> big.bin");
    let run = fixture.aisc_env(
        &["discard", "big.bin", "src/format.ts"],
        &support::env_refs(&env),
    );
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    let size = std::fs::metadata(fixture.path("big.bin")).unwrap().len();
    assert_eq!(size, BIG as u64 + 4);
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

#[test]
fn an_unchanged_large_untracked_file_is_discarded_and_undo_restores_it() {
    let fixture = pilot9();
    let mut content = vec![b'x'; BIG];
    content[BIG - 1] = b'\n';
    fixture.write_bytes("big.bin", &content);
    fixture.aisc(&["discard", "big.bin"]).ok();
    assert!(!fixture.exists("big.bin"));
    fixture.aisc(&["undo"]).ok();
    assert_eq!(std::fs::read(fixture.path("big.bin")).unwrap(), content);
}

#[test]
fn autocrlf_on_a_file_committed_with_crlf_is_not_a_concurrent_change() {
    let fixture = Fixture::with_files(&[("f", "a\r\nb\r\nc\r\n")]);
    fixture.git(&["config", "core.autocrlf", "true"]);
    fixture.write(".git/info/attributes", "* text=auto\n");
    fixture.write("f", "A\nb\r\nc\r\n");
    let run = fixture.aisc(&["discard", "f"]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(
        std::fs::read(fixture.path("f")).unwrap(),
        b"a\r\nb\r\nc\r\n"
    );
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
fn discarding_an_unstaged_hunk_keeps_a_staged_hunk_elsewhere() {
    // As after `git add -p`: line 3 staged, line 25 only in the worktree.
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(3, "STAGED3")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(3, "STAGED3"), (25, "WT25")]));
    let run = fixture.aisc(&["discard", "f:25"]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("f"), numbered(&[(3, "STAGED3")]));
    assert_eq!(
        fixture.git(&["show", ":f"]) + "\n",
        numbered(&[(3, "STAGED3")])
    );
    assert_eq!(fixture.git(&["status", "--porcelain"]), "M  f");
}

#[test]
fn a_staged_copy_with_the_discarded_hunk_loses_it_too() {
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(3, "STAGED3"), (25, "BOTH25")]));
    fixture.git(&["add", "f"]);
    fixture.write(
        "f",
        &numbered(&[(3, "STAGED3"), (14, "WT14"), (25, "BOTH25")]),
    );
    fixture.aisc(&["discard", "f:25"]).ok();
    assert_eq!(fixture.read("f"), numbered(&[(3, "STAGED3"), (14, "WT14")]));
    assert_eq!(
        fixture.git(&["show", ":f"]) + "\n",
        numbered(&[(3, "STAGED3")])
    );
}

#[test]
fn discarding_a_whole_file_never_drops_a_staged_version_that_differs_from_both() {
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(5, "S5")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(5, "W5")]));
    let before = fixture.snapshot();
    for selector in ["f:5", "f"] {
        let run = fixture.aisc(&["discard", selector]);
        assert_eq!(run.code, 1, "{selector}: {} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains("git add"), "{}", run.stderr);
        assert_eq!(before, fixture.snapshot(), "{selector}");
    }
}

#[test]
fn discarding_a_whole_file_also_takes_its_hunks_out_of_a_partly_staged_copy() {
    // Staged S3; the worktree adds W25: the whole file goes back to HEAD, index included.
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(3, "S3")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(3, "S3"), (25, "W25")]));
    fixture.aisc(&["discard", "f"]).ok();
    assert_eq!(fixture.read("f"), numbered(&[]));
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_staged_change_that_really_overlaps_is_refused() {
    let fixture = Fixture::with_files(&[("f", &numbered(&[]))]);
    fixture.write("f", &numbered(&[(25, "STAGED25")]));
    fixture.git(&["add", "f"]);
    fixture.write("f", &numbered(&[(3, "WT3"), (25, "WT25")]));
    let before = fixture.snapshot();
    let run = fixture.aisc(&["discard", "f:25"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("overlap"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
}
