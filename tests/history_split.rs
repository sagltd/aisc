//! Split and uncommit tests. Pilot 3 (split a non-top commit) and pilot 10 (recover an overbroad
//! amend) port the shape and content of vcbench pilots 3 and 10 from
//! github.com/gitbutlerapp/version-control-bench (`scripts/lib/pilot3-content.mjs` and
//! `scripts/lib/scenario-recovery.mjs`, MIT, GitButler).
mod support;

use support::Fixture;

const README_BASE: &str = "# Lead Routing Service\n\nSmall TypeScript service used by the split-commit benchmark pilot.\n";
const README_TARGET: &str = "# Lead Routing Service\n\nSmall TypeScript service used by the split-commit benchmark pilot.\n\n## Lead Workflow\n\n- Validate contact information before scoring leads.\n- Score accepted leads with email-domain and message-depth signals.\n- Workflow details live in docs/lead-workflow.md.\n";
const WORKFLOW_DOC: &str = "# Lead Workflow\n\nIncoming leads are validated before scoring.\nEnterprise email domains receive an extra score boost.\n";

const CONFIG: &str = "export interface ServiceConfig {\n  region: string;\n  defaultSource: string;\n  retryLimit: number;\n}\n\nexport const config: ServiceConfig = {\n  region: \"local\",\n  defaultSource: \"web\",\n  retryLimit: 2,\n};\n";
const CONFIG_BROAD: &str = "export interface ServiceConfig {\n  region: string;\n  defaultSource: string;\n  retryLimit: number;\n  logLevel: \"debug\" | \"info\" | \"warn\";\n}\n\nexport const config: ServiceConfig = {\n  region: \"local\",\n  defaultSource: \"web\",\n  retryLimit: 2,\n  logLevel: \"debug\",\n};\n";

const LEAD_HEAD: &str = "export interface LeadPayload {\n  name: string;\n  email: string;\n  message?: string;\n}\n\nexport type ScoreBand = \"cold\" | \"warm\" | \"hot\";\n\nexport function normalizeEmail(email: string): string {\n  return email.trim().toLowerCase();\n}\n\nexport function hasContactName(payload: LeadPayload): boolean {\n  return payload.name.trim().length > 0;\n}\n\nexport function hasEmail(payload: LeadPayload): boolean {\n  return normalizeEmail(payload.email).length > 0;\n}\n\n";
const VALID_OLD: &str = "export function isValidLead(payload: LeadPayload): boolean {\n  return hasContactName(payload) && hasEmail(payload);\n}\n";
const VALID_NEW: &str = "export function isLikelyEmail(email: string): boolean {\n  const normalized = normalizeEmail(email);\n  return normalized.includes(\"@\") && normalized.includes(\".\");\n}\n\nexport function isValidLead(payload: LeadPayload): boolean {\n  return hasContactName(payload) && hasEmail(payload) && isLikelyEmail(payload.email);\n}\n";
const SCORE_HEAD: &str = "\nexport function scoreLead(payload: LeadPayload): number {\n  const email = normalizeEmail(payload.email);\n  let score = 10;\n\n  if (email.endsWith(\"@example.com\")) {\n    score += 5;\n  }\n\n";
const SCORE_OLD: &str =
    "  if (payload.message && payload.message.length > 80) {\n    score += 10;\n  }\n";
const SCORE_NEW: &str = "  if (email.endsWith(\"@enterprise.example\")) {\n    score += 25;\n  }\n\n  if (payload.message && payload.message.length > 80) {\n    score += 15;\n  }\n";
const SCORE_TAIL: &str = "\n  return Math.min(score, 100);\n}\n\nexport function scoreBand(score: number): ScoreBand {\n  if (score >= 70) return \"hot\";\n  if (score >= 30) return \"warm\";\n  return \"cold\";\n}\n";
const DEBUG: &str = "\nexport function debugLeadSummary(payload: LeadPayload): string {\n  return `${payload.name}:${normalizeEmail(payload.email)}:${payload.message ?? \"none\"}`;\n}\n";

fn lead(validated: bool, scored: bool, debug: bool) -> String {
    format!(
        "{LEAD_HEAD}{}{SCORE_HEAD}{}{SCORE_TAIL}{}",
        if validated { VALID_NEW } else { VALID_OLD },
        if scored { SCORE_NEW } else { SCORE_OLD },
        if debug { DEBUG } else { "" }
    )
}

const TEST_NORMALIZE: &str = "import { isValidLead, normalizeEmail, scoreBand, scoreLead } from \"../src/lead\";\n\nexport function testNormalizeEmail(): void {\n  const email = normalizeEmail(\" ADA@EXAMPLE.COM \");\n\n  if (email !== \"ada@example.com\") {\n    throw new Error(`unexpected email: ${email}`);\n  }\n}\n";
const TEST_EMAIL: &str = "\nexport function testIsValidLeadRequiresEmail(): void {\n  const valid = isValidLead({ name: \"Ada\", email: \"\" });\n\n  if (valid) {\n    throw new Error(\"missing email should be rejected\");\n  }\n}\n";
const TEST_MALFORMED: &str = "\nexport function testIsValidLeadRejectsMalformedEmail(): void {\n  const valid = isValidLead({ name: \"Ada\", email: \"not-an-email\" });\n\n  if (valid) {\n    throw new Error(\"malformed email should be rejected\");\n  }\n}\n";
const TEST_DETAIL: &str = "\nexport function testScoreLeadRewardsDetailedMessage(): void {\n  const score = scoreLead({\n    name: \"Ada\",\n    email: \"ada@example.com\",\n    message: \"We are evaluating the product for a larger rollout across the sales team.\",\n  });\n\n  if (score <= 10) {\n    throw new Error(`score was not increased: ${score}`);\n  }\n}\n";
const TEST_ENTERPRISE: &str = "\nexport function testScoreLeadRewardsEnterpriseDomain(): void {\n  const score = scoreLead({\n    name: \"Grace\",\n    email: \"grace@enterprise.example\",\n    message: \"Interested in a broader rollout.\",\n  });\n\n  if (score < 35) {\n    throw new Error(`enterprise score was too low: ${score}`);\n  }\n}\n";
const TEST_BAND: &str = "\nexport function testScoreBand(): void {\n  if (scoreBand(75) !== \"hot\") {\n    throw new Error(\"expected hot score band\");\n  }\n}\n";

fn lead_tests(validated: bool, scored: bool) -> String {
    format!(
        "{TEST_NORMALIZE}{TEST_EMAIL}{}{TEST_DETAIL}{}{TEST_BAND}",
        if validated { TEST_MALFORMED } else { "" },
        if scored { TEST_ENTERPRISE } else { "" }
    )
}

const HANDLER: &str = "import { config } from \"./config\";\nimport { isValidLead, LeadPayload } from \"./lead\";\n\nexport function handleLead(payload: LeadPayload): boolean {\n  return isValidLead(payload) && config.retryLimit > 0;\n}\n";
const HANDLER_ROUTED: &str = "import { config } from \"./config\";\nimport { isValidLead, LeadPayload } from \"./lead\";\n\nexport function handleLead(payload: LeadPayload): string {\n  return isValidLead(payload) ? `${config.defaultSource}:routed` : \"rejected\";\n}\n";
const HANDLER_TEST: &str = "import { handleLead } from \"../src/handler\";\n\nexport function testHandleLeadAcceptsValidPayload(): void {\n  if (!handleLead({ name: \"Ada\", email: \"ada@example.com\" })) {\n    throw new Error(\"valid payload should be accepted\");\n  }\n}\n";
const HANDLER_TEST_ROUTED: &str = "import { handleLead } from \"../src/handler\";\n\nexport function testHandleLeadAcceptsValidPayload(): void {\n  if (handleLead({ name: \"Ada\", email: \"ada@example.com\" }) === \"rejected\") {\n    throw new Error(\"valid payload should be accepted\");\n  }\n}\n\nexport function testHandleLeadIncludesRoutingMetadata(): void {\n  if (handleLead({ name: \"Ada\", email: \"ada@example.com\" }) !== \"web:routed\") {\n    throw new Error(\"routing metadata should be populated\");\n  }\n}\n";

fn commit_as(fixture: &Fixture, subject: &str, body: &str, author: &str, date: &str) {
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit", "-m", subject, "-m", body, "--author", author, "--date", date,
    ]);
}

fn pilot3() -> Fixture {
    let fixture = Fixture::bare();
    fixture.write("README.md", README_BASE);
    fixture.write(
        "package.json",
        "{\n  \"name\": \"lead-routing-service\"\n}\n",
    );
    fixture.commit_all("base");
    fixture.git(&["switch", "-c", "split-workflow"]);
    fixture.write("src/config.ts", CONFIG);
    commit_as(
        &fixture,
        "add app configuration",
        "Config body.",
        "Ann <ann@example.invalid>",
        "2023-01-02T03:04:05+0000",
    );
    fixture.write("src/lead.ts", &lead(false, false, false));
    fixture.write("tests/lead.test.ts", &lead_tests(false, false));
    fixture.write("src/handler.ts", HANDLER);
    fixture.write("tests/handler.test.ts", HANDLER_TEST);
    commit_as(
        &fixture,
        "add API handler",
        "Handler body.",
        "Ben <ben@example.invalid>",
        "2023-02-03T04:05:06+0100",
    );
    // The broad commit: validation, scoring and docs, plus leftovers that do not belong.
    fixture.write("src/lead.ts", &lead(true, true, true));
    fixture.write("tests/lead.test.ts", &lead_tests(true, true));
    fixture.write("README.md", README_TARGET);
    fixture.write("docs/lead-workflow.md", WORKFLOW_DOC);
    fixture.write("src/config.ts", CONFIG_BROAD);
    fixture.write(
        "notes/investigation.md",
        "# Investigation Notes\n\n- Do not keep this investigation note in the split commits.\n",
    );
    commit_as(
        &fixture,
        "add lead workflow",
        "Broad commit body.",
        "Cat <cat@example.invalid>",
        "2023-03-04T05:06:07-0500",
    );
    fixture.write("src/handler.ts", HANDLER_ROUTED);
    fixture.write("tests/handler.test.ts", HANDLER_TEST_ROUTED);
    commit_as(
        &fixture,
        "add handler routing metadata",
        "Top body.",
        "Dee <dee@example.invalid>",
        "2023-04-05T06:07:08+0000",
    );
    fixture
}

fn author_and_message(fixture: &Fixture, revision: &str) -> (String, String) {
    let raw = fixture.git(&["cat-file", "commit", revision]);
    let author = raw
        .lines()
        .find(|line| line.starts_with("author "))
        .unwrap()
        .to_owned();
    (author, raw.split_once("\n\n").unwrap().1.to_owned())
}

fn assert_no_residue(fixture: &Fixture) {
    assert!(fixture.git(&["stash", "list"]).is_empty());
    for marker in [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
    ] {
        assert!(!fixture.exists(&format!(".git/{marker}")));
    }
    assert!(fixture.git(&["ls-files", "-u"]).is_empty());
}

fn split_groups() -> Vec<&'static str> {
    vec![
        "--into",
        "refactor validation helpers",
        "src/lead.ts~isLikelyEmail",
        "tests/lead.test.ts~Malformed",
        "--into",
        "tune lead scoring",
        "src/lead.ts~enterprise",
        "tests/lead.test.ts~Enterprise",
        "--into",
        "document lead workflow",
        "README.md",
        "docs/lead-workflow.md",
    ]
}

#[test]
fn pilot3_split_a_non_top_commit_takes_two_aisc_calls() {
    let fixture = pilot3();
    let main_before = fixture.git(&["rev-parse", "main"]);
    let old_top = author_and_message(&fixture, "HEAD");
    let old_broad = author_and_message(&fixture, "HEAD~1");
    let shown = fixture.aisc(&["show", "add lead workflow"]);
    shown.ok();
    assert!(
        shown.stdout.contains("docs/lead-workflow.md"),
        "{}",
        shown.stdout
    );
    let mut args = vec!["split", "add lead workflow"];
    args.extend(split_groups());
    let split = fixture.aisc(&args);
    split.ok();
    assert_eq!(fixture.calls(), 2);
    let first = split.stdout.lines().next().unwrap();
    assert!(
        first.starts_with("split ") && first.contains(" into 3; rest uncommitted: "),
        "{first}"
    );
    assert!(
        first.contains("notes/investigation.md") && first.contains("src/config.ts"),
        "{first}"
    );

    assert_eq!(
        fixture.subjects("main..HEAD"),
        [
            "add handler routing metadata",
            "document lead workflow",
            "tune lead scoring",
            "refactor validation helpers",
            "add API handler",
            "add app configuration",
        ]
    );
    assert_eq!(fixture.git(&["rev-parse", "main"]), main_before);
    let at = |n: usize, path: &str| fixture.tree_blob(&format!("HEAD~{n}"), path);
    // Validation part.
    let validation = at(3, "src/lead.ts").unwrap();
    assert!(validation.contains("isLikelyEmail") && !validation.contains("enterprise"));
    assert!(at(3, "tests/lead.test.ts").unwrap().contains("Malformed"));
    assert!(!at(3, "tests/lead.test.ts").unwrap().contains("Enterprise"));
    assert!(!at(3, "README.md").unwrap().contains("Lead Workflow"));
    // Scoring part.
    assert!(
        at(2, "src/lead.ts")
            .unwrap()
            .contains("@enterprise.example")
    );
    assert!(at(2, "tests/lead.test.ts").unwrap().contains("Enterprise"));
    assert!(!at(2, "src/lead.ts").unwrap().contains("debugLeadSummary"));
    // Docs part.
    assert!(at(1, "README.md").unwrap().contains("Lead Workflow"));
    assert_eq!(at(1, "docs/lead-workflow.md").unwrap(), WORKFLOW_DOC);
    // The top commit is intact and the leftovers are out of every commit.
    assert_eq!(at(0, "src/handler.ts").unwrap(), HANDLER_ROUTED);
    assert_eq!(at(0, "tests/handler.test.ts").unwrap(), HANDLER_TEST_ROUTED);
    assert_eq!(at(0, "src/config.ts").unwrap(), CONFIG);
    assert!(!at(0, "src/lead.ts").unwrap().contains("debugLeadSummary"));
    assert!(at(0, "notes/investigation.md").is_none());
    // Leftovers are on disk: modified in the worktree, the note untracked.
    assert_eq!(fixture.read("src/config.ts"), CONFIG_BROAD);
    assert!(fixture.read("src/lead.ts").contains("debugLeadSummary"));
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "notes/investigation.md"
    );
    let leftover = fixture.git(&["diff", "HEAD", "-U0"]);
    assert!(leftover.contains("debugLeadSummary") && leftover.contains("logLevel"));
    assert!(!leftover.contains("isLikelyEmail") && !leftover.contains("Malformed"));
    assert!(
        fixture.git(&["diff", "--cached", "--name-only"]).is_empty(),
        "the index follows the new tip"
    );
    // Parts keep the author of the split commit; the descendant keeps its metadata.
    for n in 1..=3 {
        let (author, message) = author_and_message(&fixture, &format!("HEAD~{n}"));
        assert_eq!(author, old_broad.0, "part {n}");
        assert!(message.starts_with(
            [
                "",
                "document lead workflow",
                "tune lead scoring",
                "refactor validation helpers"
            ][n]
        ));
    }
    assert_eq!(author_and_message(&fixture, "HEAD"), old_top);
    assert_no_residue(&fixture);
}

#[test]
fn rest_keep_appends_a_commit_with_the_original_tree_and_message() {
    let fixture = pilot3();
    let tip_tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let broad_tree = fixture.git(&["rev-parse", "HEAD~1^{tree}"]);
    let broad_message = author_and_message(&fixture, "HEAD~1");
    let snapshot_files = fixture.snapshot().files;
    let run = fixture.aisc(&[
        "split",
        "add lead workflow",
        "--rest",
        "keep",
        "--into",
        "refactor validation helpers",
        "src/lead.ts~isLikelyEmail",
        "tests/lead.test.ts~Malformed",
    ]);
    run.ok();
    assert_eq!(
        fixture.subjects("main..HEAD"),
        [
            "add handler routing metadata",
            "add lead workflow",
            "refactor validation helpers",
            "add API handler",
            "add app configuration",
        ]
    );
    assert_eq!(
        fixture.git(&["rev-parse", "HEAD^{tree}"]),
        tip_tree,
        "tip tree unchanged"
    );
    assert_eq!(fixture.git(&["rev-parse", "HEAD~1^{tree}"]), broad_tree);
    assert_eq!(author_and_message(&fixture, "HEAD~1"), broad_message);
    assert!(
        fixture.git(&["status", "--porcelain"]).is_empty(),
        "worktree and index clean"
    );
    assert_eq!(fixture.snapshot().files, snapshot_files);
    assert!(run.stdout.lines().next().unwrap().contains("rest kept"));
}

#[test]
fn split_of_the_top_commit_uncommits_the_rest_and_undo_restores_it() {
    let fixture = pilot3();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let run = fixture.aisc(&[
        "split",
        "add handler routing metadata",
        "--into",
        "route in the handler",
        "src/handler.ts",
    ]);
    run.ok();
    assert_eq!(fixture.subjects("HEAD~1..HEAD"), ["route in the handler"]);
    assert_eq!(
        fixture.tree_blob("HEAD", "src/handler.ts").unwrap(),
        HANDLER_ROUTED
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "tests/handler.test.ts").unwrap(),
        HANDLER_TEST
    );
    assert_eq!(fixture.read("tests/handler.test.ts"), HANDLER_TEST_ROUTED);
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M tests/handler.test.ts"
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn split_failures_change_nothing() {
    let fixture = pilot3();
    let before = fixture.snapshot();
    let unknown = fixture.aisc(&["split", "add lead workflow", "--into", "x", "nope.txt"]);
    assert_eq!(unknown.code, 1);
    let overlap = fixture.aisc(&[
        "split",
        "add lead workflow",
        "--into",
        "one",
        "README.md",
        "--into",
        "two",
        "README.md",
    ]);
    assert_eq!(overlap.code, 1);
    assert!(
        overlap.stderr.contains("selected twice"),
        "{}",
        overlap.stderr
    );
    let everything = fixture.aisc(&[
        "split",
        "add handler routing metadata",
        "--into",
        "same",
        "src/handler.ts",
        "tests/handler.test.ts",
    ]);
    assert_eq!(everything.code, 1, "no-op split: {}", everything.stdout);
    let base = fixture.aisc(&["split", "main", "--into", "x", "README.md"]);
    assert_eq!(base.code, 1);
    let empty_message = fixture.aisc(&["split", "add lead workflow", "--into", "", "README.md"]);
    assert_ne!(empty_message.code, 0);
    assert_eq!(before, fixture.snapshot());
}

fn numbered(second: &str, eleventh: &str) -> String {
    let mut text = String::new();
    for n in 1..=12 {
        let word = match n {
            2 => second,
            11 => eleventh,
            _ => "alpha",
        };
        text.push_str(&format!("l{n:02} {word}\n"));
    }
    text
}

#[test]
fn a_rest_that_a_later_commit_changes_again_is_refused() {
    let fixture = Fixture::with_files(&[("seed.txt", "seed\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("a.txt", &numbered("alpha", "alpha"));
    fixture.commit_all("seed lines");
    fixture.write("a.txt", &numbered("beta", "beta"));
    fixture.commit_all("edit two spots");
    fixture.write("a.txt", &numbered("beta", "gamma"));
    fixture.commit_all("edit again");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["split", "edit two spots", "--into", "early", "a.txt~l02"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains("edit again"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    // Keeping the rest does not need to edit later commits.
    fixture
        .aisc(&[
            "split",
            "edit two spots",
            "--rest",
            "keep",
            "--into",
            "early",
            "a.txt~l02",
        ])
        .ok();
    assert_eq!(
        fixture.subjects("main..HEAD"),
        ["edit again", "edit two spots", "early", "seed lines"]
    );
    assert_eq!(
        fixture.tree_blob("HEAD~2", "a.txt").unwrap(),
        numbered("beta", "alpha")
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "a.txt").unwrap(),
        numbered("beta", "gamma")
    );
}

#[test]
fn a_hunk_of_a_middle_commit_can_be_uncommitted_through_later_commits() {
    let fixture = Fixture::with_files(&[("seed.txt", "seed\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("a.txt", &numbered("alpha", "alpha"));
    fixture.commit_all("seed lines");
    fixture.write("a.txt", &numbered("beta", "beta"));
    fixture.commit_all("edit two spots");
    fixture.write("b.txt", "later\n");
    fixture.commit_all("later work");
    fixture
        .aisc(&["uncommit", "edit two spots", "a.txt~l11"])
        .ok();
    assert_eq!(
        fixture.subjects("main..HEAD"),
        ["later work", "edit two spots", "seed lines"]
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "a.txt").unwrap(),
        numbered("beta", "alpha")
    );
    assert_eq!(fixture.read("a.txt"), numbered("beta", "beta"));
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M a.txt");
}

#[test]
fn uncommitting_everything_drops_the_commit() {
    let fixture = Fixture::with_files(&[("seed.txt", "seed\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("a.txt", "a\n");
    fixture.commit_all("adds a");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("adds b");
    let run = fixture.aisc(&["uncommit", "adds a", "a.txt"]);
    run.ok();
    assert_eq!(fixture.subjects("main..HEAD"), ["adds b"]);
    assert_eq!(fixture.read("a.txt"), "a\n");
    assert!(run.stdout.contains("commit dropped"), "{}", run.stdout);
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "a.txt"
    );
}

#[test]
fn dependent_branches_follow_a_split() {
    let fixture = pilot3();
    fixture.git(&["branch", "stacked", "HEAD~1"]);
    fixture
        .aisc(&[
            "split",
            "add lead workflow",
            "--into",
            "workflow docs",
            "README.md",
        ])
        .ok();
    assert_eq!(
        fixture.git(&["rev-parse", "stacked"]),
        fixture.git(&["rev-parse", "HEAD~1"])
    );
}

#[test]
fn uncommit_removes_the_rest_where_it_was_made_not_from_a_look_alike() {
    let alpha = "fn alpha() {\n    let total = 1;\n    println!(\"{}\", total);\n}\n";
    let alpha_logged =
        "fn alpha() {\n    let total = 1;\n    log(total);\n    println!(\"{}\", total);\n}\n";
    let beta_logged =
        "\nfn beta() {\n    let total = 1;\n    log(total);\n    println!(\"{}\", total);\n}\n";
    let fixture = Fixture::with_files(&[("f.rs", alpha)]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.rs", alpha_logged);
    fixture.commit_all("log in alpha");
    fixture.write("f.rs", &format!("{alpha_logged}{beta_logged}"));
    fixture.commit_all("add beta");
    let run = fixture.aisc(&["uncommit", "log in alpha", "f.rs"]);
    run.ok();
    assert_eq!(
        fixture.tree_blob("HEAD", "f.rs").unwrap(),
        format!("{alpha}{beta_logged}"),
        "beta keeps its line; alpha loses its own"
    );
    assert!(
        fixture
            .git(&["diff", "HEAD", "-U0"])
            .contains("@@ -2,0 +3 @@")
    );
    assert_eq!(fixture.read("f.rs"), format!("{alpha_logged}{beta_logged}"));
}

// ---- pilot 10 -------------------------------------------------------------------------------

const DRAFT: &str = "Draft: do not publish.\nKeep this line verbatim.\n";

#[test]
fn pilot10_uncommit_recovers_an_overbroad_amend_in_one_call() {
    let fixture = Fixture::bare();
    fixture.write("src/retry.ts", "export const retryLimit = 1;\n");
    fixture.write("src/log.ts", "export const verbose = false;\n");
    fixture.write("docs/timeout.md", "Timeout notes.\n");
    fixture.commit_all("Initial retry example");
    fixture.git(&["switch", "-c", "other-topic"]);
    fixture.write("docs/timeout.md", "Timeout notes.\nUse 10 seconds.\n");
    fixture.commit_all("Document timeout option");
    fixture.git(&["switch", "main"]);
    fixture.git(&["switch", "-c", "review-fix"]);
    fixture.write("src/retry.ts", "export const retryLimit = 2;\n");
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit",
        "-m",
        "Add retry configuration",
        "-m",
        "Review body with trailing   spaces  \n\nand a second paragraph.",
        "--author",
        "Rae Reviewer <rae@example.invalid>",
        "--date",
        "2022-12-11T10:09:08+0300",
    ]);
    // The mistake: stage the draft with the fixes and amend.
    fixture.write("src/retry.ts", "export const retryLimit = 3;\n");
    fixture.write("src/log.ts", "export const verbose = true;\n");
    fixture.write("notes/private.md", DRAFT);
    fixture.git(&["add", "src/retry.ts", "src/log.ts", "notes/private.md"]);
    fixture.git(&["commit", "--amend", "--no-edit"]);
    let refs_before = fixture.git(&["for-each-ref", "refs/heads/main", "refs/heads/other-topic"]);
    let original = author_and_message(&fixture, "review-fix");
    assert!(
        fixture
            .tree_blob("review-fix", "notes/private.md")
            .is_some()
    );

    let run = fixture.aisc(&["uncommit", "review-fix", "notes/private.md"]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        fixture.git(&["rev-list", "--count", "main..review-fix"]),
        "1"
    );
    assert_eq!(
        fixture.git(&["rev-parse", "review-fix^"]),
        fixture.git(&["rev-parse", "main"])
    );
    assert_eq!(
        author_and_message(&fixture, "review-fix"),
        original,
        "byte-exact"
    );
    assert_eq!(
        fixture.tree_blob("review-fix", "src/retry.ts").unwrap(),
        "export const retryLimit = 3;\n"
    );
    assert_eq!(
        fixture.tree_blob("review-fix", "src/log.ts").unwrap(),
        "export const verbose = true;\n"
    );
    assert!(
        fixture
            .tree_blob("review-fix", "notes/private.md")
            .is_none()
    );
    // The draft is intact on disk and untracked; the index holds no trace of it.
    assert_eq!(fixture.read("notes/private.md"), DRAFT);
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "notes/private.md"
    );
    assert!(
        fixture
            .git(&["ls-files", "--", "notes/private.md"])
            .is_empty()
    );
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(
        fixture.git(&["for-each-ref", "refs/heads/main", "refs/heads/other-topic"]),
        refs_before
    );
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "review-fix"
    );
    assert_no_residue(&fixture);
    // Undo brings the overbroad commit back.
    fixture.aisc(&["undo"]).ok();
    assert!(
        fixture
            .tree_blob("review-fix", "notes/private.md")
            .is_some()
    );
    assert_eq!(
        fixture.git(&["ls-files", "--", "notes/private.md"]),
        "notes/private.md"
    );
}

/// One line per word.
fn lines(words: &str) -> String {
    words.split(' ').map(|word| format!("{word}\n")).collect()
}

#[test]
fn uncommit_never_takes_the_rest_from_a_look_alike_line_at_the_tip() {
    // "append two" appends after the rest's line; `git merge-file` aligns the run of `a` lines the
    // other way and takes the rest from the file's last line, which the line counts cannot tell.
    let fixture =
        Fixture::with_files(&[("f.txt", &lines("top f1 f2 f3 f4 f5 f6 b b b b a a end"))]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.txt", &lines("TOP f1 f2 f3 f4 f5 f6 b b b b a a a"));
    fixture.commit_all("edit both ends");
    fixture.write("f.txt", &lines("TOP f1 f2 f3 f4 f5 f6 b b b a a a"));
    fixture.commit_all("drop a b");
    fixture.write("f.txt", &lines("TOP f1 f2 f3 f4 f5 f6 b b b a a a a a"));
    fixture.commit_all("append two");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["uncommit", "edit both ends", "f.txt~end"]);
    if run.code == 0 {
        assert_eq!(
            fixture.tree_blob("HEAD", "f.txt").unwrap(),
            lines("TOP f1 f2 f3 f4 f5 f6 b b b a a end a a"),
            "{}",
            run.stdout
        );
        assert_eq!(
            fixture.tree_blob("HEAD~1", "f.txt").unwrap(),
            lines("TOP f1 f2 f3 f4 f5 f6 b b b a a end")
        );
    } else {
        assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
        assert!(run.stderr.contains("f.txt"), "{}", run.stderr);
        assert_eq!(before, fixture.snapshot());
    }
}
