#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn fixture(script: &str, payload: &Value) -> (tempfile::TempDir, Command) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    let bin = directory.path().join("bin");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&bin).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(bin.join("gh"), script).unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let data = directory.path().join("fixture.json");
    fs::write(&data, payload.to_string()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_aisc"));
    let original = std::env::var_os("PATH").unwrap();
    let paths = std::iter::once(bin).chain(std::env::split_paths(&original));
    command
        .current_dir(&root)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("AISC_GH_FIXTURE", data);
    (directory, command)
}

#[test]
fn github_compacts_review_checks_and_reports_local_bounds() {
    let pull = json!({"number":12,"title":"Useful change","url":"https://github.com/example/repo/pull/12","headRefName":"feature","state":"OPEN","isDraft":false,"reviewDecision":"CHANGES_REQUESTED","statusCheckRollup":[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://example.invalid/check"},{"__typename":"StatusContext","context":"build","state":"PENDING","targetUrl":"https://example.invalid/build"}]});
    let data = json!({"currentBranch":pull,"createdBy":[pull,pull],"needsReview":[]});
    let (_directory, mut command) = fixture(
        "#!/bin/sh\n[ \"$1 $2\" = 'pr status' ] || exit 9\ncat \"$AISC_GH_FIXTURE\"\n",
        &data,
    );
    let result = command.args(["github", "--limit", "1"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["current_branch"]["review"], "CHANGES_REQUESTED");
    assert_eq!(value["current_branch"]["checks"][0]["state"], "FAILURE");
    assert_eq!(value["current_branch"]["checks_omitted"], 1);
    assert_eq!(value["created_by"]["omitted"], 1);
    assert_eq!(
        value["needs_review"]["pull_requests"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn github_errors_are_not_empty_success_or_credential_disclosure() {
    let (_directory, mut command) = fixture(
        "#!/bin/sh\nprintf 'authentication failed secret-token-123' >&2\nexit 1\n",
        &json!({}),
    );
    let result = command.arg("github").output().unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("authentication"));
    assert!(!error.contains("secret-token"));
    let (_directory, mut command) = fixture("#!/bin/sh\nprintf '{}'\n", &json!({}));
    assert!(!command.arg("github").output().unwrap().status.success());
}
