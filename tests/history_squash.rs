//! Squash verb tests. The pilot-5 fixture is ported from vcbench `scripts/lib/pilot5-content.mjs`
//! (github.com/gitbutlerapp/version-control-bench, MIT): seven commits, two groups to squash.
mod support;

use support::Fixture;

const TOKEN_TS: &str = r#"export interface ParsedToken {
  kind: "email" | "name";
  value: string;
}

export function normalizeToken(value: string): string {
  return value.trim().toLowerCase();
}
"#;

const TOKEN_TEST: &str = r#"import { normalizeToken } from "../src/token";

export function testNormalizeToken(): void {
  const normalized = normalizeToken(" ADA@EXAMPLE.COM ");

  if (normalized !== "ada@example.com") {
    throw new Error(`unexpected normalized token: ${normalized}`);
  }
}
"#;

const PARSER_HELPERS: &str = r#"import { normalizeToken } from "./token";

export function splitRecordLine(line: string): string[] {
  return line.split(",").map((part) => normalizeToken(part));
}
"#;

const PARSER_PIPELINE: &str = r#"import { normalizeToken } from "./token";

export interface CustomerRecord {
  email: string;
  name: string;
}

export function splitRecordLine(line: string): string[] {
  return line.split(",").map((part) => normalizeToken(part));
}

export function parseCustomerRecord(line: string): CustomerRecord {
  const [email, name] = splitRecordLine(line);
  return { email, name };
}
"#;

const PARSER_TEST: &str = r#"import { parseCustomerRecord } from "../src/parser";

export function testParseCustomerRecord(): void {
  const record = parseCustomerRecord(" ADA@EXAMPLE.COM , Ada Lovelace ");

  if (record.email !== "ada@example.com" || record.name !== "ada lovelace") {
    throw new Error("customer record was not parsed");
  }
}
"#;

const EXPORTER_TS: &str = r#"import { CustomerRecord } from "./parser";

export function exportRecords(records: CustomerRecord[]): string {
  return records.map((record) => `${record.email}|${record.name}`).join("\n");
}
"#;

const EXPORTER_TEST: &str = r#"import { exportRecords } from "../src/exporter";

export function testExportRecords(): void {
  const output = exportRecords([{ email: "ada@example.com", name: "ada lovelace" }]);

  if (output !== "ada@example.com|ada lovelace") {
    throw new Error("records were not exported");
  }
}
"#;

const RETRY_TS: &str = r#"export interface RetryOption {
  attempts: number;
  delayMs: number;
}

export function parseRetryOption(value: string): RetryOption {
  const attempts = Number.parseInt(value, 10);
  return {
    attempts,
    delayMs: attempts * 500,
  };
}
"#;

const RETRY_TEST: &str = r#"import { parseRetryOption } from "../src/retry";

export function testParseRetryOption(): void {
  const option = parseRetryOption("4");

  if (option.attempts !== 4 || option.delayMs !== 2_000) {
    throw new Error("retry option was not parsed");
  }
}
"#;

const README_RETRY: &str = "# Import Pipeline\n\nTiny TypeScript repo used by the squash-commit benchmark pilot.\n\nRetry options are documented in docs/retry-options.md.\n";
const RETRY_DOC: &str = "# Retry Options\n\nImport jobs can specify retry attempts. Each retry attempt waits 500 milliseconds longer than the previous schedule slot.\n";

const ORIGINAL: [&str; 7] = [
    "add parser token model",
    "extract parser helpers",
    "wire parser helpers",
    "add export endpoint",
    "add retry option",
    "test retry option",
    "document retry option",
];

fn pilot5() -> Fixture {
    let fixture = Fixture::with_files(&[
        (
            "README.md",
            "# Import Pipeline\n\nTiny TypeScript repo used by the squash-commit benchmark pilot.\n",
        ),
        ("package.json", "{\n  \"name\": \"import-pipeline\"\n}\n"),
    ]);
    fixture.git(&["switch", "-c", "squash-series"]);
    let steps: [&[(&str, &str)]; 7] = [
        &[
            ("src/token.ts", TOKEN_TS),
            ("tests/token.test.ts", TOKEN_TEST),
        ],
        &[("src/parser.ts", PARSER_HELPERS)],
        &[
            ("src/parser.ts", PARSER_PIPELINE),
            ("tests/parser.test.ts", PARSER_TEST),
        ],
        &[
            ("src/exporter.ts", EXPORTER_TS),
            ("tests/exporter.test.ts", EXPORTER_TEST),
        ],
        &[("src/retry.ts", RETRY_TS)],
        &[("tests/retry.test.ts", RETRY_TEST)],
        &[
            ("README.md", README_RETRY),
            ("docs/retry-options.md", RETRY_DOC),
        ],
    ];
    for (index, (subject, files)) in ORIGINAL.iter().zip(steps).enumerate() {
        for (path, content) in files {
            fixture.write(path, content);
        }
        fixture.git(&["add", "-A"]);
        let author = format!("Author {index} <author{index}@example.test>");
        fixture.git(&["commit", "-m", subject, "--author", &author]);
    }
    fixture
}

fn oldest_first(fixture: &Fixture) -> Vec<String> {
    let mut subjects = fixture.subjects("main..HEAD");
    subjects.reverse();
    subjects
}

#[test]
fn pilot5_squashes_two_groups_in_one_call() {
    let fixture = pilot5();
    let old_tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let old_subjects_tip = fixture.git(&["rev-parse", "HEAD"]);
    let wire_tree = fixture.git(&["rev-parse", "HEAD~4^{tree}"]);
    let export_tree = fixture.git(&["rev-parse", "HEAD~3^{tree}"]);
    let main = fixture.git(&["rev-parse", "main"]);
    let run = fixture.aisc(&[
        "squash",
        "--into",
        "add parser pipeline",
        "extract parser helpers",
        "wire parser helpers",
        "--into",
        "add retry support",
        "add retry option",
        "test retry option",
        "document retry option",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        oldest_first(&fixture),
        [
            "add parser token model",
            "add parser pipeline",
            "add export endpoint",
            "add retry support",
        ]
    );
    // Final tree identical; intermediate trees are the newest member's tree.
    assert_eq!(fixture.git(&["rev-parse", "HEAD^{tree}"]), old_tree);
    assert_eq!(fixture.git(&["rev-parse", "HEAD~2^{tree}"]), wire_tree);
    assert_eq!(fixture.git(&["rev-parse", "HEAD~1^{tree}"]), export_tree);
    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    // The author of the oldest member is kept; the first commit keeps its id.
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an", "HEAD~2"]),
        "Author 1"
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an", "HEAD"]),
        "Author 4"
    );
    // Old subjects are unreachable from the branch.
    let reachable = fixture.git(&["log", "--format=%s", "HEAD"]);
    for gone in [
        "extract parser helpers",
        "wire parser helpers",
        "add retry option",
        "test retry option",
        "document retry option",
    ] {
        assert!(!reachable.contains(gone), "{gone} is still reachable");
    }
    assert!(!fixture.git_ok(&["merge-base", "--is-ancestor", &old_subjects_tip, "HEAD"]));
    assert_eq!(
        fixture.git(&["status", "--porcelain", "--untracked-files=all"]),
        ""
    );
    assert!(run.stdout.starts_with("squashed 5 commits into 2 commits"));
    // Undo brings the original series back.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), old_subjects_tip);
}

#[test]
fn empty_message_joins_the_member_messages() {
    let fixture = pilot5();
    fixture
        .aisc(&[
            "squash",
            "--into",
            "",
            "test retry option",
            "document retry option",
        ])
        .ok();
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%B", "HEAD"]).trim(),
        "test retry option\n\ndocument retry option"
    );
    assert_eq!(fixture.subjects("main..HEAD").len(), 6);
}

#[test]
fn non_adjacent_group_is_an_error_and_changes_nothing() {
    let fixture = pilot5();
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "squash",
        "--into",
        "joined",
        "extract parser helpers",
        "add export endpoint",
    ]);
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.contains("not adjacent") && run.stderr.contains("wire parser helpers"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("move them together first"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn group_needs_two_distinct_commits() {
    let fixture = pilot5();
    let before = fixture.snapshot();
    let single = fixture.aisc(&["squash", "--into", "m", "add export endpoint"]);
    assert_ne!(single.code, 0);
    let twice = fixture.aisc(&[
        "squash",
        "--into",
        "m",
        "add export endpoint",
        "add export endpoint",
    ]);
    assert_eq!(twice.code, 1);
    let overlap = fixture.aisc(&[
        "squash",
        "--into",
        "m",
        "add export endpoint",
        "add retry option",
        "--into",
        "n",
        "add retry option",
        "test retry option",
    ]);
    assert_eq!(overlap.code, 1);
    let unknown = fixture.aisc(&["squash", "--into", "m", "add export endpoint", "nope"]);
    assert_eq!(unknown.code, 1);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn published_commits_are_refused() {
    let fixture = pilot5();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["squash", "--into", "m", "base", "add parser token model"]);
    assert_eq!(run.code, 1);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn branch_flag_squashes_a_branch_that_is_not_checked_out() {
    let fixture = pilot5();
    fixture.git(&["switch", "main"]);
    fixture.write("scratch.txt", "keep\n");
    let head = fixture.git(&["rev-parse", "HEAD"]);
    fixture
        .aisc(&[
            "squash",
            "--branch",
            "squash-series",
            "--into",
            "export and retry",
            "add export endpoint",
            "add retry option",
        ])
        .ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.read("scratch.txt"), "keep\n");
    assert_eq!(fixture.subjects("main..squash-series").len(), 6);
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s", "squash-series~2"]),
        "export and retry"
    );
}

#[test]
fn branches_pointing_into_a_group_follow_the_squashed_commit() {
    let fixture = pilot5();
    fixture.git(&["branch", "marker", "HEAD~5"]); // "extract parser helpers"
    fixture
        .aisc(&[
            "squash",
            "--into",
            "add parser pipeline",
            "extract parser helpers",
            "wire parser helpers",
        ])
        .ok();
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s", "marker"]),
        "add parser pipeline"
    );
}
