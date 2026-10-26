//! Amend verb tests. The pilot-2 fixture ports the content of vcbench pilot 2 (multi-amend) from
//! github.com/gitbutlerapp/version-control-bench `scripts/lib/pilot2-content.mjs` (MIT, GitButler):
//! a five-commit series with dirty hunks that belong to commits 1, 3 and 5, plus leftovers
//! (a config edit, a debug helper and an untracked note) that must stay uncommitted.
mod support;

use support::{Fixture, hunk_id};

const README_BASE: &str =
    "# Lead Routing Service\n\nSmall TypeScript service used by the multi-amend benchmark pilot.\n";
const README_C1: &str = "# Lead Routing Service\n\nSmall TypeScript service used by the multi-amend benchmark pilot.\n\n## Validation\n\n- Requires a contact name and an email address before a lead can be accepted.\n- Normalizes email casing before creating identifiers.\n";
const README_C5_TAIL: &str = "\n## Scoring\n\n- Assigns a score and score band to accepted leads.\n- Uses the configured source when creating handler responses.\n\n## Response Behavior\n\n- Accepted responses include a lead id, score, band, and source.\n- Rejected responses keep the id empty.\n";

fn readme_c5() -> String {
    format!("{README_C1}{README_C5_TAIL}")
}

fn readme_target() -> String {
    readme_c5().replace("an email address", "a likely email address")
        + "- Rejected responses include a reason that can be shown to operators.\n"
}

const LEAD_C1: &str = "export interface LeadPayload {\n  name: string;\n  email: string;\n  message?: string;\n}\n\nexport function normalizeEmail(email: string): string {\n  return email.trim().toLowerCase();\n}\n\nexport function hasContactName(payload: LeadPayload): boolean {\n  return payload.name.trim().length > 0;\n}\n\nexport function hasEmail(payload: LeadPayload): boolean {\n  return normalizeEmail(payload.email).length > 0;\n}\n\nexport function isValidLead(payload: LeadPayload): boolean {\n  return hasContactName(payload) && hasEmail(payload);\n}\n";

const SCORING: &str = "\nexport function scoreLead(payload: LeadPayload): number {\n  const email = normalizeEmail(payload.email);\n  let score = 10;\n\n  if (email.endsWith(\"@example.com\")) {\n    score += 5;\n  }\n\n  if (payload.message && payload.message.length > 80) {\n    score += 10;\n  }\n\n  return Math.min(score, 100);\n}\n\nexport function scoreBand(score: number): ScoreBand {\n  if (score >= 70) return \"hot\";\n  if (score >= 30) return \"warm\";\n  return \"cold\";\n}\n";

fn lead_c3() -> String {
    let mut text = LEAD_C1.replace(
        "\nexport function normalizeEmail",
        "\nexport type ScoreBand = \"cold\" | \"warm\" | \"hot\";\n\nexport function normalizeEmail",
    );
    text.push_str(SCORING);
    text
}

fn lead_target() -> String {
    lead_c3()
        .replace(
            "export function isValidLead(payload: LeadPayload): boolean {\n  return hasContactName(payload) && hasEmail(payload);\n}",
            "export function isLikelyEmail(email: string): boolean {\n  const normalized = normalizeEmail(email);\n  return normalized.includes(\"@\") && normalized.includes(\".\");\n}\n\nexport function isValidLead(payload: LeadPayload): boolean {\n  return hasContactName(payload) && hasEmail(payload) && isLikelyEmail(payload.email);\n}",
        )
        .replace(
            "  if (payload.message && payload.message.length > 80) {\n    score += 10;",
            "  if (email.endsWith(\"@enterprise.example\")) {\n    score += 25;\n  }\n\n  if (payload.message && payload.message.length > 80) {\n    score += 15;",
        )
}

fn lead_dirty() -> String {
    format!(
        "{}\nexport function debugLeadSummary(payload: LeadPayload): string {{\n  return `${{payload.name}}:${{normalizeEmail(payload.email)}}:${{payload.message ?? \"none\"}}`;\n}}\n",
        lead_target()
    )
}

const TEST_HEAD: &str = "import { isValidLead, normalizeEmail } from \"../src/lead\";\n";
const TEST_HEAD_C3: &str =
    "import { isValidLead, normalizeEmail, scoreBand, scoreLead } from \"../src/lead\";\n";
const T_NORMALIZE: &str = "\nexport function testNormalizeEmail(): void {\n  const email = normalizeEmail(\" ADA@EXAMPLE.COM \");\n\n  if (email !== \"ada@example.com\") {\n    throw new Error(`unexpected email: ${email}`);\n  }\n}\n";
const T_NAME: &str = "\nexport function testIsValidLeadRequiresName(): void {\n  const valid = isValidLead({ name: \"\", email: \"ada@example.com\" });\n\n  if (valid) {\n    throw new Error(\"missing name should be rejected\");\n  }\n}\n";
const T_EMAIL: &str = "\nexport function testIsValidLeadRequiresEmail(): void {\n  const valid = isValidLead({ name: \"Ada\", email: \"\" });\n\n  if (valid) {\n    throw new Error(\"missing email should be rejected\");\n  }\n}\n";
const T_MALFORMED: &str = "\nexport function testIsValidLeadRejectsMalformedEmail(): void {\n  const valid = isValidLead({ name: \"Ada\", email: \"not-an-email\" });\n\n  if (valid) {\n    throw new Error(\"malformed email should be rejected\");\n  }\n}\n";
const T_DETAIL: &str = "\nexport function testScoreLeadRewardsDetailedMessage(): void {\n  const score = scoreLead({\n    name: \"Ada\",\n    email: \"ada@example.com\",\n    message: \"We are evaluating the product for a larger rollout across the sales team.\",\n  });\n\n  if (score <= 10) {\n    throw new Error(`score was not increased: ${score}`);\n  }\n}\n";
const T_ENTERPRISE: &str = "\nexport function testScoreLeadRewardsEnterpriseDomain(): void {\n  const score = scoreLead({\n    name: \"Grace\",\n    email: \"grace@enterprise.example\",\n    message: \"Interested in a broader rollout.\",\n  });\n\n  if (score < 35) {\n    throw new Error(`enterprise score was too low: ${score}`);\n  }\n}\n";
const T_BAND: &str = "\nexport function testScoreBand(): void {\n  if (scoreBand(75) !== \"hot\") {\n    throw new Error(\"expected hot score band\");\n  }\n}\n";

fn tests_c1() -> String {
    format!("{TEST_HEAD}{T_NORMALIZE}{T_NAME}{T_EMAIL}")
}
fn tests_c3() -> String {
    format!("{TEST_HEAD_C3}{T_NORMALIZE}{T_NAME}{T_EMAIL}{T_DETAIL}{T_BAND}")
}
fn tests_target() -> String {
    format!("{TEST_HEAD_C3}{T_NORMALIZE}{T_NAME}{T_MALFORMED}{T_DETAIL}{T_ENTERPRISE}{T_BAND}")
}

const CONFIG: &str = "export interface ServiceConfig {\n  region: string;\n  defaultSource: string;\n  retryLimit: number;\n}\n\nexport const config: ServiceConfig = {\n  region: \"local\",\n  defaultSource: \"web\",\n  retryLimit: 2,\n};\n";
const CONFIG_DIRTY: &str = "export interface ServiceConfig {\n  region: string;\n  defaultSource: string;\n  retryLimit: number;\n  logLevel: \"debug\" | \"info\" | \"warn\";\n}\n\nexport const config: ServiceConfig = {\n  region: \"local\",\n  defaultSource: \"web\",\n  retryLimit: 2,\n  logLevel: \"debug\",\n};\n";
const HANDLER: &str = "import { config } from \"./config\";\nimport { isValidLead, LeadPayload } from \"./lead\";\n\nexport function handleLead(payload: LeadPayload): boolean {\n  return isValidLead(payload) && config.retryLimit > 0;\n}\n";
const RESPONSE_DOC: &str = "# Response Behavior\n\nAccepted leads return an id, score, band, and source.\nRejected leads return an empty id and a cold score band.\n";
const RESPONSE_DOC_TARGET: &str = "# Response Behavior\n\nAccepted leads return an id, score, band, and source.\nInvalid leads return `accepted: false` with a stable rejection reason.\nRejected leads return an empty id and a cold score band.\n";

fn commit(fixture: &Fixture, subject: &str, author: &str, date: &str) {
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit",
        "-m",
        subject,
        "-m",
        "Body paragraph kept verbatim.",
        "--author",
        author,
        "--date",
        date,
    ]);
}

fn pilot2() -> Fixture {
    let fixture = Fixture::bare();
    fixture.write("README.md", README_BASE);
    fixture.write(
        "package.json",
        "{\n  \"name\": \"lead-routing-service\"\n}\n",
    );
    fixture.commit_all("base");
    fixture.git(&["switch", "-c", "amend-series"]);
    fixture.write("README.md", README_C1);
    fixture.write("src/lead.ts", LEAD_C1);
    fixture.write("tests/lead.test.ts", &tests_c1());
    commit(
        &fixture,
        "refactor validation helpers",
        "Ada Author <ada@example.invalid>",
        "2023-05-06T07:08:09+0200",
    );
    fixture.write("src/config.ts", CONFIG);
    commit(
        &fixture,
        "add app configuration",
        "Bob Builder <bob@example.invalid>",
        "2023-06-07T08:09:10+0000",
    );
    fixture.write("src/lead.ts", &lead_c3());
    fixture.write("tests/lead.test.ts", &tests_c3());
    commit(
        &fixture,
        "add lead scoring",
        "Cy Coder <cy@example.invalid>",
        "2023-07-08T09:10:11-0500",
    );
    fixture.write("src/handler.ts", HANDLER);
    commit(
        &fixture,
        "add API handler",
        "Di Dev <di@example.invalid>",
        "2023-08-09T10:11:12+0100",
    );
    fixture.write("README.md", &readme_c5());
    fixture.write("docs/response.md", RESPONSE_DOC);
    commit(
        &fixture,
        "document response behavior",
        "Ed Editor <ed@example.invalid>",
        "2023-09-10T11:12:13+0000",
    );
    // The dirty state: target content plus leftovers.
    fixture.write("README.md", &readme_target());
    fixture.write("src/lead.ts", &lead_dirty());
    fixture.write("tests/lead.test.ts", &tests_target());
    fixture.write("docs/response.md", RESPONSE_DOC_TARGET);
    fixture.write("src/config.ts", CONFIG_DIRTY);
    fixture.write(
        "notes/investigation.md",
        "# Investigation Notes\n\n- Do not amend this investigation note into the benchmark commits.\n",
    );
    fixture
}

const SUBJECTS: [&str; 5] = [
    "document response behavior",
    "add API handler",
    "add lead scoring",
    "add app configuration",
    "refactor validation helpers",
];

fn metadata(fixture: &Fixture, revision: &str) -> String {
    // Author line and raw message, exactly as stored.
    let raw = fixture.git(&["cat-file", "commit", revision]);
    let author = raw
        .lines()
        .find(|line| line.starts_with("author "))
        .unwrap()
        .to_owned();
    let message = raw.split_once("\n\n").unwrap().1.to_owned();
    format!("{author}\n--\n{message}")
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

#[test]
fn pilot2_multi_amend_takes_two_aisc_calls() {
    let fixture = pilot2();
    let main_before = fixture.git(&["rev-parse", "main"]);
    let old: Vec<String> = (0..5)
        .map(|n| fixture.git(&["rev-parse", &format!("HEAD~{n}")]))
        .collect();
    let metadata_before: Vec<String> = old.iter().map(|oid| metadata(&fixture, oid)).collect();
    let log = fixture.aisc(&["log", "--json"]);
    log.ok();
    let log = log.json();
    let id = |path: &str, needle: &str| hunk_id(&log, path, needle);
    let (likely_readme, likely_fn, malformed) = (
        id("README.md", "likely email"),
        id("src/lead.ts", "isLikelyEmail"),
        id("tests/lead.test.ts", "Malformed"),
    );
    let (enterprise, enterprise_test) = (
        id("src/lead.ts", "enterprise"),
        id("tests/lead.test.ts", "Enterprise"),
    );
    let reason = id("README.md", "reason");
    let amend = fixture.aisc(&[
        "amend",
        "--into",
        "refactor validation helpers",
        &likely_readme,
        &likely_fn,
        &malformed,
        "--into",
        "add lead scoring",
        &enterprise,
        &enterprise_test,
        "--into",
        "document response behavior",
        &reason,
        "docs/response.md",
    ]);
    amend.ok();
    assert_eq!(fixture.calls(), 2);
    assert!(
        amend.stdout.len() < 2_000,
        "compact output: {}",
        amend.stdout
    );
    let first = amend.stdout.lines().next().unwrap();
    assert!(
        first.starts_with("amended ")
            && first.contains("+3 hunks")
            && first.contains("rewrote 5; op "),
        "{first}"
    );

    // Subjects and order are unchanged; main is untouched.
    assert_eq!(fixture.subjects("main..HEAD"), SUBJECTS);
    assert_eq!(fixture.git(&["rev-parse", "main"]), main_before);
    // Per-commit content, vcbench style.
    let at = |n: usize, path: &str| fixture.tree_blob(&format!("HEAD~{n}"), path);
    assert!(at(4, "src/lead.ts").unwrap().contains("isLikelyEmail"));
    assert!(!at(4, "src/lead.ts").unwrap().contains("enterprise"));
    assert!(at(4, "README.md").unwrap().contains("likely email"));
    assert!(at(4, "tests/lead.test.ts").unwrap().contains("Malformed"));
    assert!(!at(4, "tests/lead.test.ts").unwrap().contains("Enterprise"));
    assert!(
        at(3, "src/lead.ts").unwrap().contains("isLikelyEmail"),
        "carried through commit 2"
    );
    assert!(!at(3, "src/lead.ts").unwrap().contains("enterprise"));
    assert!(
        at(2, "src/lead.ts")
            .unwrap()
            .contains("@enterprise.example")
    );
    assert!(at(2, "tests/lead.test.ts").unwrap().contains("Enterprise"));
    assert!(!at(1, "README.md").unwrap().contains("reason"));
    assert!(
        at(0, "README.md")
            .unwrap()
            .contains("reason that can be shown")
    );
    assert!(
        at(0, "docs/response.md")
            .unwrap()
            .contains("stable rejection reason")
    );
    assert!(at(1, "docs/response.md").is_none());
    // Leftovers are still dirty, absent from history; the note is still untracked.
    assert_eq!(at(0, "src/config.ts").unwrap(), CONFIG);
    assert_eq!(fixture.read("src/config.ts"), CONFIG_DIRTY);
    assert!(!at(0, "src/lead.ts").unwrap().contains("debugLeadSummary"));
    assert!(fixture.read("src/lead.ts").contains("debugLeadSummary"));
    assert_eq!(
        fixture.git(&["ls-files", "--others", "--exclude-standard"]),
        "notes/investigation.md"
    );
    let leftover = fixture.git(&["diff", "HEAD", "-U0"]);
    assert!(!leftover.contains("isLikelyEmail") && !leftover.contains("Malformed"));
    assert!(leftover.contains("debugLeadSummary") && leftover.contains("logLevel"));
    // The worktree is untouched and the tip holds exactly the routed changes.
    assert_eq!(fixture.read("README.md"), readme_target());
    assert_eq!(
        fixture.tree_blob("HEAD", "README.md").unwrap(),
        readme_target()
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "tests/lead.test.ts").unwrap(),
        tests_target()
    );
    // Author, date and message are preserved; the committer is refreshed.
    let new: Vec<String> = (0..5)
        .map(|n| fixture.git(&["rev-parse", &format!("HEAD~{n}")]))
        .collect();
    for (n, oid) in new.iter().enumerate() {
        assert_eq!(metadata(&fixture, oid), metadata_before[n], "commit {n}");
        assert_ne!(*oid, old[n], "commit {n} was rewritten");
    }
    assert_ne!(
        fixture.git(&["log", "-1", "--format=%cI", "HEAD"]),
        "2024-01-02T03:04:05+00:00"
    );
    assert!(fixture.git(&["diff", "--cached", "--name-only"]).is_empty());
    assert_no_residue(&fixture);
}

#[test]
fn content_selectors_need_one_call() {
    let fixture = pilot2();
    fixture
        .aisc(&[
            "amend",
            "--into",
            "refactor validation helpers",
            "src/lead.ts~isLikelyEmail",
            "--into",
            "add lead scoring",
            "src/lead.ts~enterprise",
        ])
        .ok();
    assert_eq!(fixture.calls(), 1);
    assert!(
        fixture
            .tree_blob("HEAD~4", "src/lead.ts")
            .unwrap()
            .contains("isLikelyEmail")
    );
}

#[test]
fn dependency_error_names_the_later_commit_and_changes_nothing() {
    let fixture = pilot2();
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "amend",
        "--into",
        "refactor validation helpers",
        "src/lead.ts~enterprise",
    ]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stderr.contains("add lead scoring") && run.stderr.contains("target"),
        "{}",
        run.stderr
    );
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn file_added_later_is_named_in_the_error() {
    let fixture = pilot2();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "add lead scoring", "docs/response.md"]);
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.contains("document response behavior"),
        "{}",
        run.stderr
    );
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn untracked_file_is_refused_where_the_path_exists_later() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("one.txt", "one\n");
    fixture.commit_all("first");
    fixture.write("note.txt", "note\n");
    fixture.commit_all("adds note");
    fixture.git(&["rm", "-q", "note.txt"]);
    fixture.commit_all("removes note");
    fixture.write("note.txt", "note again\n");
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "first", "note.txt"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains("adds note"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn untracked_file_goes_into_a_commit_that_can_hold_it() {
    let fixture = pilot2();
    fixture
        .aisc(&[
            "amend",
            "--into",
            "add API handler",
            "notes/investigation.md",
        ])
        .ok();
    assert!(
        fixture
            .tree_blob("HEAD~1", "notes/investigation.md")
            .is_some()
    );
    assert!(
        fixture
            .tree_blob("HEAD~2", "notes/investigation.md")
            .is_none()
    );
    assert!(
        fixture
            .tree_blob("HEAD", "notes/investigation.md")
            .is_some()
    );
    assert!(
        fixture
            .git(&["ls-files", "--others", "--exclude-standard"])
            .is_empty()
    );
}

#[test]
fn dependent_stacked_branch_moves_with_its_commit() {
    let fixture = pilot2();
    fixture.git(&["branch", "stacked", "HEAD~2"]);
    let stacked_before = fixture.git(&["rev-parse", "stacked"]);
    let run = fixture.aisc(&[
        "amend",
        "--into",
        "refactor validation helpers",
        "src/lead.ts~isLikelyEmail",
    ]);
    run.ok();
    let stacked = fixture.git(&["rev-parse", "stacked"]);
    assert_ne!(stacked, stacked_before);
    assert_eq!(stacked, fixture.git(&["rev-parse", "HEAD~2"]));
    assert!(
        run.stdout.contains("moved branch stacked:"),
        "{}",
        run.stdout
    );
}

#[test]
fn unrelated_staged_entries_are_preserved() {
    let fixture = pilot2();
    fixture.git(&["add", "src/config.ts"]);
    fixture
        .aisc(&[
            "amend",
            "--into",
            "add lead scoring",
            "src/lead.ts~enterprise",
        ])
        .ok();
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        "src/config.ts"
    );
    assert_eq!(
        fixture.git(&["show", ":src/config.ts"]),
        CONFIG_DIRTY.trim_end()
    );
    let status = fixture.git(&["status", "--porcelain"]);
    assert!(status.contains("M  src/config.ts"), "{status}");
    assert!(status.contains(" M src/lead.ts"), "{status}");
}

#[test]
fn amending_into_a_base_commit_is_refused() {
    let fixture = pilot2();
    let before = fixture.snapshot();
    let main_oid = fixture.git(&["rev-parse", "main"]);
    for target in ["main", main_oid.as_str()] {
        let run = fixture.aisc(&["amend", "--into", target, "README.md"]);
        assert_eq!(run.code, 1, "{}", run.stdout);
        assert!(run.stderr.contains("main"), "{}", run.stderr);
    }
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn bad_selectors_and_overlapping_groups_change_nothing() {
    let fixture = pilot2();
    let before = fixture.snapshot();
    let unknown = fixture.aisc(&["amend", "--into", "add lead scoring", "nope.txt"]);
    assert_eq!(unknown.code, 1);
    let twice = fixture.aisc(&[
        "amend",
        "--into",
        "add lead scoring",
        "src/lead.ts~enterprise",
        "--into",
        "add API handler",
        "src/lead.ts~enterprise",
    ]);
    assert_eq!(twice.code, 1);
    assert!(twice.stderr.contains("selected twice"), "{}", twice.stderr);
    let missing = fixture.aisc(&["amend", "--into", "no such commit", "README.md"]);
    assert_eq!(missing.code, 1);
    let other = fixture.aisc(&[
        "amend",
        "--branch",
        "main",
        "--into",
        "add lead scoring",
        "README.md",
    ]);
    assert_eq!(other.code, 1);
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn undo_restores_the_branch_and_the_index() {
    let fixture = pilot2();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let status = fixture.git(&["status", "--porcelain"]);
    fixture
        .aisc(&[
            "amend",
            "--into",
            "add lead scoring",
            "src/lead.ts~enterprise",
        ])
        .ok();
    assert_ne!(fixture.git(&["rev-parse", "HEAD"]), head);
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.git(&["status", "--porcelain"]), status);
}

#[test]
fn a_moved_branch_between_read_and_apply_is_a_stale_error() {
    let fixture = pilot2();
    let hook = "$REAL_GIT update-ref refs/heads/amend-series HEAD~1";
    let env = fixture.fake_git("hash-object", hook);
    let run = fixture.aisc_env(
        &[
            "amend",
            "--into",
            "add lead scoring",
            "src/lead.ts~enterprise",
        ],
        &support::env_refs(&env),
    );
    assert_ne!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert!(
        fixture.git(&["for-each-ref", "refs/aisc"]).is_empty(),
        "no operation was recorded"
    );
}

const LOOKALIKE_HEAD: &str = "// head\n";
const ALPHA: &str = "\nfn alpha() {\n    let total = 1;\n    println!(\"{}\", total);\n}\n";
const BETA: &str = "\nfn beta() {\n    let total = 1;\n    println!(\"{}\", total);\n}\n";
const BETA_LOGGED: &str =
    "\nfn beta() {\n    let total = 1;\n    log(total);\n    println!(\"{}\", total);\n}\n";

#[test]
fn a_hunk_in_a_look_alike_function_is_not_placed_in_the_older_one() {
    let fixture = Fixture::with_files(&[("f.rs", LOOKALIKE_HEAD)]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.rs", &format!("{LOOKALIKE_HEAD}{ALPHA}"));
    fixture.commit_all("add alpha");
    fixture.write("f.rs", &format!("{LOOKALIKE_HEAD}{ALPHA}{BETA}"));
    fixture.commit_all("add beta");
    fixture.write("f.rs", &format!("{LOOKALIKE_HEAD}{ALPHA}{BETA_LOGGED}"));
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "add alpha", "f.rs"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(run.stderr.contains("add beta"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    // Into the commit that has the lines it works.
    fixture.aisc(&["amend", "--into", "add beta", "f.rs"]).ok();
    assert_eq!(
        fixture.tree_blob("HEAD", "f.rs").unwrap(),
        format!("{LOOKALIKE_HEAD}{ALPHA}{BETA_LOGGED}")
    );
    assert_eq!(
        fixture.tree_blob("HEAD~1", "f.rs").unwrap(),
        format!("{LOOKALIKE_HEAD}{ALPHA}")
    );
}

/// One line per word.
fn lines(words: &str) -> String {
    words.split(' ').map(|word| format!("{word}\n")).collect()
}

const FILLER: &str = "head f1 f2 f3 f4 f5 f6";

/// Runs of identical lines that Git's 3-way merge aligns differently from the commits' own diffs:
/// "trim the first run" keeps the second run of `a` lines intact, so the dirty `X` (the last `a`
/// of the second run) belongs there in every commit. `git merge-file` puts it in the first run of
/// the oldest commit, and the later commit's line counts hide the move.
fn repeated_runs(created: bool) -> Fixture {
    let first = lines(&format!("{FILLER} a a a b a a a b c b a c tail"));
    let fixture = if created {
        Fixture::with_files(&[("other.txt", "other\n")])
    } else {
        Fixture::with_files(&[(
            "f.txt",
            &lines(&format!("{FILLER} a a a b a a a b c b a c")),
        )])
    };
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.txt", &first);
    fixture.commit_all("add tail");
    fixture.write(
        "f.txt",
        &lines(&format!("{FILLER} a b a a a b c b a c tail")),
    );
    fixture.commit_all("trim the first run");
    fixture.write(
        "f.txt",
        &lines(&format!("{FILLER} a b a a a b a c a c b a c tail")),
    );
    fixture.commit_all("insert a pair");
    fixture.write(
        "f.txt",
        &lines("HEAD f1 f2 f3 f4 f5 f6 a b a a X b a c a c b a c tail"),
    );
    fixture
}

fn assert_placed_in_the_right_run(created: bool) {
    let fixture = repeated_runs(created);
    let run = fixture.aisc(&[
        "amend",
        "--into",
        "add tail",
        "f.txt~X",
        "--into",
        "trim the first run",
        "f.txt~HEAD",
    ]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    // Exactly where the commits' own diffs put that line, in every commit.
    assert_eq!(
        fixture.tree_blob("HEAD~2", "f.txt").unwrap(),
        lines(&format!("{FILLER} a a a b a a X b c b a c tail"))
    );
    assert_eq!(
        fixture.tree_blob("HEAD~1", "f.txt").unwrap(),
        lines("HEAD f1 f2 f3 f4 f5 f6 a b a a X b c b a c tail")
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "f.txt").unwrap(),
        fixture.read("f.txt")
    );
    // Each later commit still makes only its own change.
    assert_eq!(
        fixture
            .git(&["diff", "-U0", "HEAD~2", "HEAD~1"])
            .matches("\n-a")
            .count(),
        2
    );
}

#[test]
fn a_hunk_among_repeated_lines_lands_in_its_own_run() {
    assert_placed_in_the_right_run(false);
}

#[test]
fn a_hunk_into_the_commit_that_creates_the_file_lands_in_its_own_run() {
    assert_placed_in_the_right_run(true);
}

#[test]
fn a_fix_next_to_a_line_a_later_commit_inserted_goes_into_the_older_commit() {
    // `git merge-file` calls adjacent changes a conflict; the line itself is the older commit's.
    let fixture = Fixture::with_files(&[("other.txt", "other\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.rs", &lines("fn a() { one } fn b() { two }"));
    fixture.commit_all("add a and b");
    fixture.write("f.rs", &lines("fn a() { one } inserted fn b() { two }"));
    fixture.commit_all("insert a line");
    fixture.write("f.rs", &lines("fn a() { one } inserted fn b() { TWO }"));
    fixture.commit_all("shout two");
    fixture.write("f.rs", &lines("fn a() { one }; inserted fn b() { TWO }"));
    let run = fixture.aisc(&["amend", "--into", "add a and b", "f.rs"]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(
        fixture.tree_blob("HEAD~2", "f.rs").unwrap(),
        lines("fn a() { one }; fn b() { two }")
    );
    assert_eq!(
        fixture.tree_blob("HEAD~1", "f.rs").unwrap(),
        lines("fn a() { one }; inserted fn b() { two }")
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "f.rs").unwrap(),
        fixture.read("f.rs")
    );
}

#[test]
fn a_fix_to_a_later_commits_line_names_the_one_call_that_works() {
    let fixture = Fixture::with_files(&[("other.txt", "other\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f.rs", &lines("one two three four five six seven eight"));
    fixture.commit_all("add f");
    fixture.write("f.rs", &lines("one two three four five SIX seven eight"));
    fixture.commit_all("shout six");
    fixture.write("f.rs", &lines("one two three four five SIX seven EIGHT"));
    fixture.commit_all("shout eight");
    fixture.write("f.rs", &lines("one two three four five SIX! seven EIGHT"));
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "add f", "f.rs"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("shout six"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    // The hint is the exact call that works.
    let hint = run
        .stderr
        .lines()
        .find_map(|line| line.split_once(" instead: ").map(|(_, call)| call))
        .unwrap_or_else(|| panic!("{}", run.stderr));
    let args: Vec<&str> = hint.split(' ').collect();
    assert_eq!(&args[..2], ["aisc", "amend"], "{hint}");
    fixture.aisc(&args[1..]).ok();
    assert_eq!(
        fixture.tree_blob("HEAD~1", "f.rs").unwrap(),
        lines("one two three four five SIX! seven eight")
    );
}

#[test]
fn a_hunk_into_the_commit_that_creates_the_file_keeps_the_later_changes() {
    let fixture = Fixture::with_files(&[("other.txt", "other\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write(
        "f.txt",
        &lines("one two three four five six seven eight nine"),
    );
    fixture.commit_all("add f");
    fixture.write(
        "f.txt",
        &lines("one two three four five six seven eight NINE"),
    );
    fixture.commit_all("shout nine");
    fixture.write(
        "f.txt",
        &lines("ONE two three four five six seven eight NINE"),
    );
    fixture.aisc(&["amend", "--into", "add f", "f.txt"]).ok();
    assert_eq!(
        fixture.tree_blob("HEAD~1", "f.txt").unwrap(),
        lines("ONE two three four five six seven eight nine")
    );
    assert_eq!(
        fixture.tree_blob("HEAD", "f.txt").unwrap(),
        fixture.read("f.txt")
    );
    assert_eq!(fixture.subjects("main..HEAD"), ["shout nine", "add f"]);
}

/// `l1`..`l30`, with `edit` applied to the 1-based line numbers it maps.
fn numbered(edit: &dyn Fn(usize) -> Vec<String>) -> String {
    (1..=30)
        .flat_map(edit)
        .map(|line| format!("{line}\n"))
        .collect()
}

fn plain(number: usize) -> Vec<String> {
    vec![format!("l{number}")]
}

/// C2 rewrites lines 13-14; the worktree turns line 10 into 11 lines (new lines 10-20) and
/// deletes C2's two lines (at new line 23).
fn widened_and_deleted() -> Fixture {
    let fixture = Fixture::with_files(&[("base.txt", "b\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f", &numbered(&plain));
    fixture.commit_all("T");
    let c2 = |n: usize| match n {
        13 | 14 => vec![format!("c2-{n}")],
        _ => plain(n),
    };
    fixture.write("f", &numbered(&c2));
    fixture.commit_all("C2");
    fixture.write(
        "f",
        &numbered(&|n| match n {
            10 => (1..=11).map(|x| format!("X{x}")).collect(),
            13 | 14 => Vec::new(),
            _ => plain(n),
        }),
    );
    fixture
}

#[test]
fn a_line_range_counts_new_side_lines_only() {
    // New lines 13-14 are inside the widened block; the deletion sits at new line 23.
    let fixture = widened_and_deleted();
    fixture.aisc(&["amend", "--into", "C2", "f:13-14"]).ok();
    let committed = fixture.tree_blob("HEAD", "f").unwrap();
    assert!(committed.contains("X11\n"), "{committed}");
    assert!(
        committed.contains("c2-13\n"),
        "the deletion stays out: {committed}"
    );
    let fixture = widened_and_deleted();
    fixture.aisc(&["amend", "--into", "C2", "f:23"]).ok();
    let committed = fixture.tree_blob("HEAD", "f").unwrap();
    assert!(!committed.contains("c2-13\n"), "{committed}");
    assert!(
        !committed.contains("X1\n"),
        "the widened block stays out: {committed}"
    );
}

#[test]
fn the_one_call_hint_resolves_to_exactly_the_blocks_it_moves() {
    let fixture = widened_and_deleted();
    let worktree = fixture.read("f");
    let run = fixture.aisc(&["amend", "--into", "T", "f"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    let hint = run
        .stderr
        .lines()
        .find_map(|line| line.split_once(" instead: ").map(|(_, call)| call))
        .unwrap_or_else(|| panic!("{}", run.stderr));
    let args: Vec<&str> = hint.split(' ').collect();
    let again = fixture.aisc(&args[1..]);
    assert_eq!(again.code, 0, "{hint}: {} {}", again.stdout, again.stderr);
    // The widened block goes into T, the deletion of C2's lines into C2; nothing stays out.
    let t = fixture.tree_blob("HEAD~1", "f").unwrap();
    assert!(t.contains("X11\n") && t.contains("l13\n"), "{t}");
    assert_eq!(fixture.tree_blob("HEAD", "f").unwrap(), worktree);
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

fn mode_of(fixture: &Fixture, revision: &str, path: &str) -> String {
    fixture.git(&["ls-tree", revision, path])[..6].to_owned()
}

fn executable(fixture: &Fixture, path: &str, on: bool) {
    use std::os::unix::fs::PermissionsExt;
    let bits = if on { 0o755 } else { 0o644 };
    std::fs::set_permissions(fixture.path(path), std::fs::Permissions::from_mode(bits)).unwrap();
}

#[test]
fn a_mode_change_never_erases_a_later_commits_own_mode_change() {
    let fixture = Fixture::with_files(&[("base.txt", "b\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f", &numbered(&plain));
    fixture.commit_all("T");
    executable(&fixture, "f", true);
    fixture.write(
        "f",
        &numbered(&|n| {
            if n == 30 {
                vec!["L30".into()]
            } else {
                plain(n)
            }
        }),
    );
    fixture.commit_all("C2");
    executable(&fixture, "f", false);
    fixture.write(
        "f",
        &numbered(&|n| match n {
            2 => vec!["FIX2".into()],
            30 => vec!["L30".into()],
            _ => plain(n),
        }),
    );
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "T", "f"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("C2"), "{}", run.stderr);
    assert_eq!(before, fixture.snapshot());
    let hint = run
        .stderr
        .lines()
        .find_map(|line| line.split_once(" instead: ").map(|(_, call)| call))
        .unwrap_or_else(|| panic!("{}", run.stderr));
    let args: Vec<&str> = hint.split(' ').collect();
    fixture.aisc(&args[1..]).ok();
    assert_eq!(mode_of(&fixture, "HEAD~1", "f"), "100644");
    assert_eq!(mode_of(&fixture, "HEAD", "f"), "100644");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_mode_change_goes_into_the_target_with_its_content() {
    let fixture = Fixture::with_files(&[("base.txt", "b\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f", &numbered(&plain));
    fixture.commit_all("T");
    fixture.write(
        "f",
        &numbered(&|n| {
            if n == 30 {
                vec!["L30".into()]
            } else {
                plain(n)
            }
        }),
    );
    fixture.commit_all("C2");
    executable(&fixture, "f", true);
    fixture.write(
        "f",
        &numbered(&|n| match n {
            2 => vec!["FIX2".into()],
            30 => vec!["L30".into()],
            _ => plain(n),
        }),
    );
    fixture.aisc(&["amend", "--into", "T", "f"]).ok();
    assert_eq!(mode_of(&fixture, "HEAD~1", "f"), "100755");
    assert_eq!(mode_of(&fixture, "HEAD", "f"), "100755");
    assert!(fixture.tree_blob("HEAD~1", "f").unwrap().contains("FIX2\n"));
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

/// Commits a..h then a..i, and returns the fixture with `setup` applied before the commits.
fn filtered(setup: &dyn Fn(&Fixture)) -> Fixture {
    let fixture = Fixture::bare();
    setup(&fixture);
    fixture.write("base.txt", "b\n");
    fixture.commit_all("base");
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f", "a\nb\nc\nd\ne\nf\ng\nh\n");
    fixture.commit_all("v0");
    fixture.write("f", "a\nb\nc\nd\ne\nf\ng\nh\ni\n");
    fixture.commit_all("v1");
    fixture
}

fn assert_whole_file_amend_with_filters(fixture: &Fixture, worktree: &str) {
    fixture.write("f", worktree);
    let run = fixture.aisc(&["amend", "--into", "v0", "f"]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(
        fixture.git_bytes(&["show", "HEAD~1:f"]),
        b"A\nb\nc\nd\ne\nf\ng\nh\n"
    );
    assert_eq!(
        fixture.git_bytes(&["show", "HEAD:f"]),
        b"A\nb\nc\nd\ne\nf\ng\nh\ni\n"
    );
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_whole_file_amend_with_eol_crlf_commits_normalized_lines() {
    let fixture = filtered(&|fixture| fixture.write(".gitattributes", "f text eol=crlf\n"));
    assert_whole_file_amend_with_filters(&fixture, "A\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\n");
}

#[test]
fn a_whole_file_amend_with_autocrlf_commits_normalized_lines() {
    let fixture = filtered(&|fixture| {
        fixture.git(&["config", "core.autocrlf", "true"]);
    });
    assert_whole_file_amend_with_filters(&fixture, "A\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\n");
}

#[test]
fn a_whole_file_amend_through_a_clean_filter_commits_the_cleaned_lines() {
    let fixture = filtered(&|fixture| {
        fixture.write(".gitattributes", "f filter=up\n");
        fixture.git(&["config", "filter.up.clean", "sed s/secret//"]);
        fixture.git(&["config", "filter.up.smudge", "cat"]);
    });
    assert_whole_file_amend_with_filters(&fixture, "Asecret\nb\nc\nd\ne\nf\ng\nh\ni\n");
}

#[test]
fn autocrlf_on_a_file_committed_with_crlf_is_not_a_concurrent_change() {
    // Git keeps CRLF for a file whose committed version has CRLF; so must the change check.
    let fixture = Fixture::with_files(&[("base.txt", "b\n")]);
    fixture.git(&["switch", "-c", "topic"]);
    fixture.write("f", "a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\n");
    fixture.commit_all("v0");
    fixture.write("f", "a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\n");
    fixture.commit_all("v1");
    fixture.git(&["config", "core.autocrlf", "true"]);
    fixture.write(".git/info/attributes", "* text=auto\n");
    fixture.write("f", "A\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\n");
    let run = fixture.aisc(&["amend", "--into", "v0", "f"]);
    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(
        fixture.git_bytes(&["show", "HEAD~1:f"]),
        b"A\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\n"
    );
    // Git sees the worktree as the new tip.
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn a_file_changed_between_read_and_amend_is_a_stale_error() {
    let fixture = Fixture::with_files(&[("a.txt", "a\n")]);
    fixture.write("b.txt", "b\n");
    fixture.commit_all("add b");
    fixture.write("a.txt", "a2\n");
    let head = fixture.git(&["rev-parse", "HEAD"]);
    // The edit lands after aisc read the diff and before it hashes the whole file.
    let env = fixture.fake_git_after("diff", "printf 'sneaky\\n' >> a.txt");
    let run = fixture.aisc_env(
        &["amend", "--into", "base", "a.txt"],
        &support::env_refs(&env),
    );
    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert_eq!(fixture.read("a.txt"), "a2\nsneaky\n");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(fixture.git(&["for-each-ref", "refs/aisc"]).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Amending a commit on another local branch from this worktree.

const PORTS: &str = "export const port = 8080;\nexport const host = \"localhost\";\nexport const MAX_RETRIES = 3;\nexport const timeoutMs = 5000;\nexport const verbose = false;\nexport const region = \"eu\";\n";

/// main: config.ts; feature-config: "Add config loader" (changes the host line, appends a loader),
/// then "Add config docs"; feature-ui (checked out): "Add theme toggle". Dirty here: the port fix
/// in config.ts and a ui.ts edit. Returns the fixture and the loader file with the port fix.
fn elsewhere() -> (Fixture, String) {
    let loader = PORTS.replace("\"localhost\"", "\"0.0.0.0\"")
        + "\nexport function load(path: string) {\n  return path;\n}\n";
    let fixture = Fixture::with_files(&[("config.ts", PORTS), ("README.md", "readme\n")]);
    fixture.git(&["switch", "-q", "-c", "feature-config"]);
    fixture.write("config.ts", &loader);
    fixture.commit_all("Add config loader");
    fixture.write("docs.md", "loader docs\n");
    fixture.commit_all("Add config docs");
    fixture.git(&["switch", "-q", "-c", "feature-ui", "main"]);
    fixture.write("ui.ts", "export const theme = \"dark\";\n");
    fixture.commit_all("Add theme toggle");
    fixture.write("config.ts", &PORTS.replace("port = 8080", "port = 3000"));
    fixture.write(
        "ui.ts",
        "export const theme = \"dark\";\nexport const compact = true;\n",
    );
    (fixture, loader.replace("port = 8080", "port = 3000"))
}

#[test]
fn amend_into_a_commit_on_another_branch_in_one_call() {
    let (fixture, fixed_loader) = elsewhere();
    let tip_before = fixture.git(&["rev-parse", "feature-config"]);
    let author = fixture.git(&["log", "-1", "--format=%an <%ae> %ad", "feature-config~1"]);
    // No --branch: the commit is found on the one other branch that has it.
    let run = fixture.aisc(&["amend", "--into", "Add config loader", "config.ts"]);
    run.ok();
    assert!(run.stdout.contains("feature-config"), "{}", run.stdout);
    assert_eq!(
        fixture.subjects("main..feature-config"),
        ["Add config docs", "Add config loader"]
    );
    assert_eq!(
        fixture
            .tree_blob("feature-config~1", "config.ts")
            .as_deref(),
        Some(fixed_loader.as_str())
    );
    assert_eq!(
        fixture.tree_blob("feature-config", "config.ts").as_deref(),
        Some(fixed_loader.as_str())
    );
    assert_eq!(
        fixture.tree_blob("feature-config", "docs.md").as_deref(),
        Some("loader docs\n")
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%an <%ae> %ad", "feature-config~1"]),
        author
    );
    // This worktree: the fix left, the ui edit stayed, HEAD did not move.
    assert_eq!(fixture.read("config.ts"), PORTS);
    assert_eq!(
        fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature-ui"
    );
    assert_eq!(fixture.git(&["status", "--porcelain"]), " M ui.ts");
    assert_eq!(fixture.subjects("main..feature-ui"), ["Add theme toggle"]);
    // Undo restores the branch and brings the fix back here.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "feature-config"]), tip_before);
    assert_eq!(
        fixture.read("config.ts"),
        PORTS.replace("port = 8080", "port = 3000")
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M config.ts\n M ui.ts"
    );
}

#[test]
fn a_target_branch_moved_between_read_and_apply_is_refused_elsewhere() {
    let (fixture, _) = elsewhere();
    // The first Git process of the operation; the branch moves under it, like another agent.
    let hook = "$REAL_GIT update-ref refs/heads/feature-config feature-config~1";
    let env = fixture.fake_git("hash-object", hook);
    let moved = fixture.git(&["rev-parse", "feature-config~1"]);
    let run = fixture.aisc_env(
        &["amend", "--into", "Add config loader", "config.ts"],
        &support::env_refs(&env),
    );
    assert_ne!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert!(
        fixture.git(&["for-each-ref", "refs/aisc"]).is_empty(),
        "no operation was recorded"
    );
    assert_eq!(fixture.git(&["rev-parse", "feature-config"]), moved);
    assert_eq!(
        fixture.read("config.ts"),
        PORTS.replace("port = 8080", "port = 3000")
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain"]),
        " M config.ts\n M ui.ts"
    );
}

#[test]
fn amend_elsewhere_refusals_change_nothing() {
    let (fixture, _) = elsewhere();
    // The subject exists on two branches: name one.
    fixture.git(&["branch", "config-copy", "feature-config"]);
    let heads = |fixture: &Fixture| {
        fixture.git(&["for-each-ref", "refs/heads"])
            + "\n"
            + &fixture.git(&["status", "--porcelain"])
    };
    let heads_before = heads(&fixture);
    let before = fixture.snapshot();
    let run = fixture.aisc(&["amend", "--into", "Add config loader", "config.ts"]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("config-copy") && run.stderr.contains("feature-config"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("--branch"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    let run = fixture.aisc(&[
        "amend",
        "--branch",
        "feature-config",
        "--into",
        "Add config loader",
        "config.ts",
    ]);
    run.ok();
    assert_eq!(
        fixture.subjects("main..config-copy"),
        ["Add config docs", "Add config loader"]
    );
    // A branch at a rewritten commit follows it, as with every rewrite verb.
    assert_eq!(
        fixture.git(&["rev-parse", "config-copy"]),
        fixture.git(&["rev-parse", "feature-config"])
    );
    fixture.aisc(&["undo"]).ok();
    assert_eq!(heads(&fixture), heads_before);
    fixture.git(&["branch", "-D", "config-copy"]);
    // A hunk on lines the target commit changed cannot be placed: refused, nothing written.
    fixture.write(
        "config.ts",
        &PORTS
            .replace("port = 8080", "port = 3000")
            .replace("\"localhost\"", "\"127.0.0.1\""),
    );
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "amend",
        "--branch",
        "feature-config",
        "--into",
        "Add config loader",
        "config.ts~127.0.0.1",
    ]);
    assert_ne!(run.code, 0, "{}", run.stdout);
    assert!(run.stderr.contains("nothing was changed"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    // The target branch is checked out in another worktree.
    fixture.write("config.ts", &PORTS.replace("port = 8080", "port = 3000"));
    let linked = fixture.root.parent().unwrap().join("config-wt");
    fixture.git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().unwrap(),
        "feature-config",
    ]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "amend",
        "--branch",
        "feature-config",
        "--into",
        "Add config loader",
        "config.ts~3000",
    ]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("another worktree"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    fixture.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
    // Several --into groups: one branch per call.
    let run = fixture.aisc(&[
        "amend",
        "--branch",
        "feature-config",
        "--into",
        "Add config loader",
        "config.ts~3000",
        "--into",
        "Add config docs",
        "ui.ts",
    ]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("one --into"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
    // A foreign index lock: refused before anything is read.
    fixture.write(".git/index.lock", "");
    let run = fixture.aisc(&[
        "amend",
        "--branch",
        "feature-config",
        "--into",
        "Add config loader",
        "config.ts~3000",
    ]);
    assert_eq!(run.code, 1, "{} {}", run.stdout, run.stderr);
    assert!(run.stderr.contains("locked"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}
