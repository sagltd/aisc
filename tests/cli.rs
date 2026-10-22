use serde_json::Value;
use std::process::Command;

fn invoke(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aisc"))
        .args(arguments)
        .env_remove("NO_COLOR")
        .output()
        .unwrap()
}

#[test]
fn help_and_version_are_plain_text_even_when_redirected_or_json_is_requested() {
    for arguments in [
        vec!["--help"],
        vec!["--json", "--color", "always", "--help"],
        vec!["log", "--help"],
        vec!["finish", "--json", "--help"],
        vec![],
    ] {
        let output = invoke(&arguments);
        assert!(output.status.success(), "{arguments:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
        assert!(!output.stdout.contains(&0x1b), "{arguments:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Usage: aisc"), "{arguments:?}: {text}");
        assert!(
            serde_json::from_str::<Value>(&text).is_err(),
            "{arguments:?}"
        );
    }
    for arguments in [vec!["--version"], vec!["--json", "--version"]] {
        let version = invoke(&arguments);
        assert!(version.status.success());
        assert_eq!(String::from_utf8(version.stdout).unwrap(), "aisc 0.1.0\n");
    }
}

#[test]
fn parse_errors_are_json_on_stderr_with_exit_two() {
    for arguments in [
        vec!["unknown"],
        vec!["context", "--limit", "0"],
        vec!["start", "name", "--sparse", "dir"],
        vec!["--json", "--human", "context"],
    ] {
        let output = invoke(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.contains(&0x1b));
        assert!(serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"].is_string());
    }
}

#[test]
fn human_help_supports_colors_and_no_color_override() {
    let output = invoke(&["--human", "--color", "always", "--help"]);
    assert!(output.status.success());
    assert!(output.stdout.contains(&0x1b));
    let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
        .args(["--human", "--color", "always", "--help"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    let output = invoke(&["--human", "--color", "never", "--help"]);
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn runtime_errors_keep_json_default_and_human_labels() {
    let directory = tempfile::tempdir().unwrap();
    for (flags, expects_json) in [
        (vec!["context"], true),
        (vec!["--json", "--color", "always", "context"], true),
        (vec!["--human", "--color", "never", "context"], false),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aisc"))
            .args(flags)
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.contains(&0x1b));
        if expects_json {
            assert!(serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"].is_string());
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).starts_with("Error:"));
        }
    }
}
