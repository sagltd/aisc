//! Move verb tests. The pilot-4 fixture is ported from vcbench `scripts/lib/pilot4-content.mjs`
//! (github.com/gitbutlerapp/version-control-bench, MIT): a six-commit series that must be reordered.
mod support;

use support::Fixture;

const CONFIG_TS: &str = r#"export interface AppConfig {
  region: string;
  senderAddress: string;
  retryLimit: number;
}

export const config: AppConfig = {
  region: "local",
  senderAddress: "alerts@example.com",
  retryLimit: 3,
};
"#;

const CUSTOMER_TS: &str = r#"export interface Customer {
  id: string;
  firstName: string;
  lastName: string;
  email: string;
}

export function customerDisplayName(customer: Customer): string {
  return `${customer.firstName} ${customer.lastName}`.trim();
}
"#;

const CUSTOMER_TEST: &str = r#"import { customerDisplayName } from "../src/customer";

export function testCustomerDisplayName(): void {
  const name = customerDisplayName({
    id: "cus_123",
    firstName: "Ada",
    lastName: "Lovelace",
    email: "ada@example.com",
  });

  if (name !== "Ada Lovelace") {
    throw new Error(`unexpected name: ${name}`);
  }
}
"#;

const EMAIL_TS: &str = r#"export interface NotificationEmail {
  to: string;
  subject: string;
  body: string;
}

export function formatNotificationEmail(to: string, subject: string, body: string): NotificationEmail {
  return {
    to: to.trim().toLowerCase(),
    subject: subject.trim(),
    body: body.trim(),
  };
}
"#;

const EMAIL_TEST: &str = r#"import { formatNotificationEmail } from "../src/email";

export function testFormatNotificationEmail(): void {
  const email = formatNotificationEmail(" ADA@EXAMPLE.COM ", " Welcome ", " Hello ");

  if (email.to !== "ada@example.com" || email.subject !== "Welcome" || email.body !== "Hello") {
    throw new Error("email was not normalized");
  }
}
"#;

const RETRY_TS: &str = r#"export function nextRetryDelay(attempt: number): number {
  if (attempt <= 0) return 0;
  return Math.min(30_000, attempt * 1_000);
}
"#;

const RETRY_TEST: &str = r#"import { nextRetryDelay } from "../src/retry";

export function testNextRetryDelay(): void {
  if (nextRetryDelay(3) !== 3_000) {
    throw new Error("unexpected retry delay");
  }
}
"#;

const NOTIFICATION_TS: &str = r#"export interface NotificationPayload {
  customerId: string;
  channel: "email" | "sms";
  message: string;
}

export function buildNotificationPayload(customerId: string, message: string): NotificationPayload {
  return {
    customerId,
    channel: "email",
    message: message.trim(),
  };
}
"#;

const NOTIFICATION_TEST: &str = r#"import { buildNotificationPayload } from "../src/notification";

export function testBuildNotificationPayload(): void {
  const payload = buildNotificationPayload("cus_123", " Your invoice is ready ");

  if (payload.customerId !== "cus_123" || payload.message !== "Your invoice is ready") {
    throw new Error("payload was not normalized");
  }
}
"#;

const README_DOC: &str = "# Notification Service\n\nTiny TypeScript repo used by the reorder-commit benchmark pilot.\n\nOperational notes live in docs/notification-flow.md.\n";
const FLOW_DOC: &str = "# Notification Flow\n\nNotifications are prepared from customer records, formatted for the selected channel, and retried when delivery fails.\n";

const ORIGINAL: [&str; 6] = [
    "add app configuration",
    "add customer model",
    "add email formatter",
    "add retry policy",
    "add notification sender",
    "document notification flow",
];

fn pilot4() -> Fixture {
    let fixture = Fixture::with_files(&[
        (
            "README.md",
            "# Notification Service\n\nTiny TypeScript repo used by the reorder-commit benchmark pilot.\n",
        ),
        (
            "package.json",
            "{\n  \"name\": \"notification-service\"\n}\n",
        ),
    ]);
    fixture.git(&["switch", "-c", "reorder-series"]);
    let steps: [&[(&str, &str)]; 6] = [
        &[("src/config.ts", CONFIG_TS)],
        &[
            ("src/customer.ts", CUSTOMER_TS),
            ("tests/customer.test.ts", CUSTOMER_TEST),
        ],
        &[
            ("src/email.ts", EMAIL_TS),
            ("tests/email.test.ts", EMAIL_TEST),
        ],
        &[
            ("src/retry.ts", RETRY_TS),
            ("tests/retry.test.ts", RETRY_TEST),
        ],
        &[
            ("src/notification.ts", NOTIFICATION_TS),
            ("tests/notification.test.ts", NOTIFICATION_TEST),
        ],
        &[
            ("README.md", README_DOC),
            ("docs/notification-flow.md", FLOW_DOC),
        ],
    ];
    for (subject, files) in ORIGINAL.iter().zip(steps) {
        for (path, content) in files {
            fixture.write(path, content);
        }
        fixture.commit_all(subject);
    }
    fixture
}

fn oldest_first(fixture: &Fixture) -> Vec<String> {
    let mut subjects = fixture.subjects("main..HEAD");
    subjects.reverse();
    subjects
}

#[test]
fn pilot4_reorders_in_one_call() {
    let fixture = pilot4();
    let main = fixture.git(&["rev-parse", "main"]);
    let old_tip = fixture.git(&["rev-parse", "HEAD"]);
    let old_tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    let old_config = fixture.git(&["rev-parse", "HEAD~5"]);
    let run = fixture.aisc(&[
        "move",
        "add customer model",
        "add email formatter",
        "--after",
        "add notification sender",
    ]);
    run.ok();
    assert_eq!(fixture.calls(), 1);
    assert_eq!(
        oldest_first(&fixture),
        [
            "add app configuration",
            "add retry policy",
            "add notification sender",
            "add customer model",
            "add email formatter",
            "document notification flow",
        ]
    );
    assert_eq!(fixture.git(&["rev-parse", "HEAD^{tree}"]), old_tree);
    assert_eq!(fixture.git(&["rev-parse", "main"]), main);
    // The unaffected first commit keeps its id; the moved ones are new.
    assert_eq!(fixture.git(&["rev-parse", "HEAD~5"]), old_config);
    assert_ne!(fixture.git(&["rev-parse", "HEAD"]), old_tip);
    // Each commit still carries exactly its own files.
    assert_eq!(
        fixture.commit_files("HEAD~2"),
        ["src/customer.ts", "tests/customer.test.ts"]
    );
    assert_eq!(
        fixture.commit_files("HEAD~3"),
        ["src/notification.ts", "tests/notification.test.ts"]
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain", "--untracked-files=all"]),
        ""
    );
    assert!(run.stdout.starts_with("moved 5 commits"), "{}", run.stdout);
    // Undo restores the original series.
    fixture.aisc(&["undo"]).ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), old_tip);
}

#[test]
fn before_places_commits_ahead_of_the_anchor() {
    let fixture = pilot4();
    fixture
        .aisc(&["move", "add retry policy", "--before", "add customer model"])
        .ok();
    assert_eq!(
        oldest_first(&fixture),
        [
            "add app configuration",
            "add retry policy",
            "add customer model",
            "add email formatter",
            "add notification sender",
            "document notification flow",
        ]
    );
}

#[test]
fn order_flag_takes_a_full_permutation() {
    let fixture = pilot4();
    let wanted = [
        "add app configuration",
        "add retry policy",
        "add notification sender",
        "add customer model",
        "add email formatter",
        "document notification flow",
    ];
    let mut arguments = vec!["move", "--order"];
    arguments.extend(wanted);
    let run = fixture.aisc(&arguments);
    run.ok();
    assert_eq!(oldest_first(&fixture), wanted);
}

#[test]
fn order_validation_rejects_bad_permutations_without_changes() {
    let fixture = pilot4();
    let before = fixture.snapshot();
    // Missing commit.
    let missing = fixture.aisc(&[
        "move",
        "--order",
        "add customer model",
        "add app configuration",
    ]);
    assert_eq!(missing.code, 1, "{}", missing.stderr);
    assert!(
        missing.stderr.contains("must list all 6 commits") && missing.stderr.contains("missing"),
        "{}",
        missing.stderr
    );
    // Duplicate.
    let mut duplicate = vec!["move", "--order"];
    duplicate.extend(ORIGINAL);
    duplicate[2] = "add customer model";
    let duplicated = fixture.aisc(&duplicate);
    assert_eq!(duplicated.code, 1);
    assert!(duplicated.stderr.contains("twice"), "{}", duplicated.stderr);
    // Unknown commit.
    let mut unknown = vec!["move", "--order"];
    unknown.extend(ORIGINAL);
    unknown[2] = "no such commit";
    assert_eq!(fixture.aisc(&unknown).code, 1);
    // Identity order changes nothing.
    let mut same = vec!["move", "--order"];
    same.extend(ORIGINAL);
    let same = fixture.aisc(&same);
    assert_eq!(same.code, 1);
    assert!(same.stderr.contains("nothing changed"), "{}", same.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn anchor_cannot_be_a_moved_commit() {
    let fixture = pilot4();
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "move",
        "add customer model",
        "--after",
        "add customer model",
    ]);
    assert_eq!(run.code, 1);
    assert_eq!(fixture.snapshot(), before);
}

fn conflicting() -> Fixture {
    let fixture = Fixture::with_files(&[("f.txt", "one\ntwo\nthree\n")]);
    fixture.git(&["switch", "-c", "work"]);
    fixture.write("f.txt", "one\nTWO\nthree\n");
    fixture.commit_all("shout two");
    fixture.write("f.txt", "one\nTWO\nthree\nfour\n");
    fixture.commit_all("add four");
    fixture.write("f.txt", "one\nTWO!\nthree\nfour\n");
    fixture.commit_all("emphasise two");
    fixture
}

#[test]
fn conflicting_move_aborts_atomically_with_a_report() {
    let fixture = conflicting();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "emphasise two", "--before", "shout two"]);
    assert_eq!(run.code, 1, "{}", run.stdout);
    assert!(
        run.stderr.contains("f.txt conflicts") && run.stderr.contains("emphasise two"),
        "{}",
        run.stderr
    );
    assert_eq!(fixture.snapshot(), before);
    assert!(!fixture.exists(".git/MERGE_HEAD"));
    assert!(
        !fixture.git_ok(&["rev-parse", "--verify", "-q", "refs/aisc/oplog/main"]),
        "no op is recorded for a failed move"
    );
}

#[test]
fn branch_flag_rewrites_another_branch_without_touching_the_checkout() {
    let fixture = pilot4();
    fixture.git(&["switch", "main"]);
    fixture.write("scratch.txt", "keep me\n");
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let tree = fixture.git(&["rev-parse", "reorder-series^{tree}"]);
    fixture
        .aisc(&[
            "move",
            "--branch",
            "reorder-series",
            "add retry policy",
            "--before",
            "add customer model",
        ])
        .ok();
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fixture.read("scratch.txt"), "keep me\n");
    assert_eq!(fixture.git(&["rev-parse", "reorder-series^{tree}"]), tree);
    let subjects = fixture.subjects("main..reorder-series");
    assert_eq!(subjects[4], "add retry policy");
    assert_eq!(subjects[3], "add customer model");
}

#[test]
fn branch_checked_out_in_another_worktree_is_refused() {
    let fixture = pilot4();
    fixture.git(&["switch", "main"]);
    let elsewhere = fixture.directory.path().join("elsewhere");
    fixture.git(&[
        "worktree",
        "add",
        elsewhere.to_str().unwrap(),
        "reorder-series",
    ]);
    let before = fixture.snapshot();
    let run = fixture.aisc(&[
        "move",
        "--branch",
        "reorder-series",
        "add retry policy",
        "--before",
        "add customer model",
    ]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("checked out"), "{}", run.stderr);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn branches_pointing_at_rewritten_commits_follow() {
    let fixture = pilot4();
    fixture.git(&["branch", "marker", "HEAD~1"]); // at "add notification sender"
    let old = fixture.git(&["rev-parse", "marker"]);
    fixture
        .aisc(&[
            "move",
            "add retry policy",
            "--after",
            "add notification sender",
        ])
        .ok();
    // The marker moved along with the rewritten commit it pointed at.
    assert_ne!(fixture.git(&["rev-parse", "marker"]), old);
    assert_eq!(
        fixture.git(&["rev-parse", "marker"]),
        fixture.git(&["rev-parse", "HEAD~2"])
    );
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s", "marker"]),
        "add notification sender"
    );
}

#[test]
fn published_commits_are_refused() {
    let fixture = pilot4();
    let before = fixture.snapshot();
    let run = fixture.aisc(&["move", "base", "--after", "add retry policy"]);
    assert_eq!(run.code, 1);
    assert_eq!(fixture.snapshot(), before);
}
