#![cfg(unix)]

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn fixture(branch: &str, script: &str, payload: &Value) -> (tempfile::TempDir, Command) {
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
    assert!(
        Command::new("git")
            .args(["config", "user.name", "AISC Test"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "user.email", "aisc-test@example.invalid"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join("fixture.txt"), "fixture\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "fixture.txt"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-qm", "fixture"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["checkout", "-qb", branch])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(bin.join("glab"), script).unwrap();
    fs::set_permissions(bin.join("glab"), fs::Permissions::from_mode(0o755)).unwrap();
    let data = directory.path().join("fixture.json");
    fs::write(&data, payload.to_string()).unwrap();
    let args = directory.path().join("argv.txt");
    let original = std::env::var_os("PATH").unwrap();
    let paths = std::iter::once(bin).chain(std::env::split_paths(&original));
    let mut command = Command::new(env!("CARGO_BIN_EXE_aisc"));
    command
        .current_dir(&root)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("AISC_GLAB_FIXTURE", data)
        .env("AISC_GLAB_ARGS", args);
    (directory, command)
}

fn request(iid: u64, branch: &str, state: &str) -> Value {
    json!({
        "iid": iid,
        "title": format!("Change {iid}"),
        "web_url": format!("https://gitlab.example/group/project/-/merge_requests/{iid}"),
        "source_branch": branch,
        "state": "opened",
        "draft": false,
        "detailed_merge_status": state,
        "head_pipeline": {"status": state, "web_url": "https://gitlab.example/pipelines/1"}
    })
}

const SCRIPT: &str =
    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AISC_GLAB_ARGS\"\ncat \"$AISC_GLAB_FIXTURE\"\n";

#[test]
fn gitlab_compacts_current_branch_pipeline_and_reports_observed_omissions() {
    let data = json!([
        request(12, "feature", "future_pipeline_state"),
        request(13, "feature", "failed")
    ]);
    let (directory, mut command) = fixture("feature", SCRIPT, &data);
    let result = command
        .args(["--json", "gitlab", "--limit", "1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["queried"], true);
    assert_eq!(value["current_branch"], "feature");
    assert_eq!(value["merge_requests"]["items"][0]["number"], 12);
    assert_eq!(
        value["merge_requests"]["items"][0]["mergeability"],
        "future_pipeline_state"
    );
    assert_eq!(
        value["merge_requests"]["items"][0]["pipeline"]["status"],
        "future_pipeline_state"
    );
    assert_eq!(value["merge_requests"]["omitted"], 1);
    assert_eq!(value["merge_requests"]["may_have_more"], true);
    let args = fs::read_to_string(directory.path().join("argv.txt")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "mr",
            "list",
            "--source-branch",
            "feature",
            "--per-page",
            "2",
            "--output",
            "json"
        ]
    );
}

#[test]
fn gitlab_empty_open_list_is_a_success_and_optional_fields_stay_unknown() {
    let (directory, mut command) = fixture("no-mr", SCRIPT, &json!([]));
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["merge_requests"]["items"], json!([]));
    assert_eq!(value["merge_requests"]["may_have_more"], false);
    let args = fs::read_to_string(directory.path().join("argv.txt")).unwrap();
    assert!(args.contains("no-mr"));

    let mut item = request(14, "no-mr", "new_merge_state");
    item.as_object_mut().unwrap().remove("draft");
    item.as_object_mut()
        .unwrap()
        .remove("detailed_merge_status");
    item.as_object_mut().unwrap().remove("head_pipeline");
    let (_directory, mut command) = fixture("no-mr", SCRIPT, &json!([item]));
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(value["merge_requests"]["items"][0]["draft"].is_null());
    assert!(value["merge_requests"]["items"][0]["mergeability"].is_null());
    assert!(value["merge_requests"]["items"][0]["pipeline"].is_null());
}

#[test]
fn gitlab_uses_branch_as_a_literal_argument_and_detached_head_skips_network() {
    let branch = "feature;echo-marker";
    let (directory, mut command) = fixture(branch, SCRIPT, &json!([]));
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let args = fs::read_to_string(directory.path().join("argv.txt")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "mr",
            "list",
            "--source-branch",
            branch,
            "--per-page",
            "11",
            "--output",
            "json"
        ]
    );

    let (directory, mut command) = fixture(
        "detached",
        "#!/bin/sh\ntouch \"$AISC_GLAB_ARGS\"\ncat \"$AISC_GLAB_FIXTURE\"\n",
        &json!([]),
    );
    assert!(
        Command::new("git")
            .args(["checkout", "-q", "--detach"])
            .current_dir(directory.path().join("repo"))
            .status()
            .unwrap()
            .success()
    );
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["queried"], false);
    assert!(value["current_branch"].is_null());
    assert!(value["merge_requests"].is_null());
    assert!(!directory.path().join("argv.txt").exists());
}

#[test]
fn gitlab_errors_are_bounded_and_never_echo_credentials() {
    let (_directory, mut command) = fixture(
        "feature",
        "#!/bin/sh\nprintf 'authentication failed token=secret-value' >&2\nexit 1\n",
        &json!([]),
    );
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(!result.status.success());
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(error.contains("authentication"));
    assert!(!error.contains("secret-value"));

    let (_directory, mut command) = fixture(
        "feature",
        "#!/bin/sh\nprintf 'connection failed' >&2\nexit 1\n",
        &json!([]),
    );
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("network/API"));

    let (_directory, mut command) = fixture("feature", "#!/bin/sh\nprintf '{broken'\n", &json!([]));
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("invalid glab JSON"));

    let (_directory, mut command) = fixture(
        "feature",
        "#!/bin/sh\nhead -c 4194305 /dev/zero\n",
        &json!([]),
    );
    let result = command.args(["--json", "gitlab"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("4 MiB safety limit"));

    let (directory, mut command) = fixture(
        "feature",
        "#!/bin/sh\ntouch \"$AISC_GLAB_ARGS\"\nprintf '[]'\n",
        &json!([]),
    );
    let result = command
        .args(["--json", "gitlab", "--limit", "101"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!directory.path().join("argv.txt").exists());
}
