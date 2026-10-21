use crate::git::Repo;
use crate::process;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::process::Command;
use std::time::Duration;

pub(crate) fn overview(repo: &Repo, limit: usize) -> Result<Value> {
    let mut command = Command::new("gh");
    command
        .args([
            "pr",
            "status",
            "--json",
            "number,title,url,headRefName,state,isDraft,reviewDecision,statusCheckRollup",
        ])
        .current_dir(&repo.root)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env_remove("GH_DEBUG")
        .env_remove("DEBUG");
    let output = process::execute(&mut command, Duration::from_secs(30), 4 * 1_024 * 1_024)
        .context("could not run gh; install/authenticate GitHub CLI first")?;
    if !output.success() {
        // gh can be configured for debug output: never echo its stderr or credentials.
        let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        let category =
            if error.contains("auth") || error.contains("token") || error.contains("login") {
                "authentication"
            } else if error.contains("network")
                || error.contains("resolve")
                || error.contains("connection")
                || error.contains("api")
                || error.contains("http")
                || error.contains("graphql")
            {
                "network/API"
            } else {
                "GitHub CLI"
            };
        bail!(
            "gh pr status {category} failure (exit {:?}, timeout={}); run gh pr status directly to diagnose",
            output.code,
            output.timed_out
        );
    }
    if output.stdout_omitted != 0 || output.stderr_omitted != 0 {
        bail!("GitHub response exceeds the 4 MiB safety limit");
    }
    let data: Value = serde_json::from_slice(&output.stdout).context("invalid gh JSON response")?;
    // gh omits this optional key when the current branch has no pull request.
    let current = data.get("currentBranch").unwrap_or(&Value::Null);
    let created = data
        .get("createdBy")
        .and_then(Value::as_array)
        .context("gh response missing createdBy array")?;
    let review = data
        .get("needsReview")
        .and_then(Value::as_array)
        .context("gh response missing needsReview array")?;
    Ok(
        json!({"current_branch": if current.is_null() { Value::Null } else { compact(current, limit)? },
        "created_by": category(created, limit)?, "needs_review": category(review, limit)?,
        "scope": "gh pr status categories; upstream results may be partial; omitted counts apply to returned records"}),
    )
}

fn category(pulls: &[Value], limit: usize) -> Result<Value> {
    let selected = pulls
        .iter()
        .take(limit)
        .map(|pull| compact(pull, limit))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"pull_requests": selected, "omitted": pulls.len().saturating_sub(limit)}))
}

fn compact(pull: &Value, limit: usize) -> Result<Value> {
    let number = pull
        .get("number")
        .and_then(Value::as_u64)
        .context("invalid PR number")?;
    let mut checks = Vec::new();
    let rollup = pull
        .get("statusCheckRollup")
        .context("PR missing statusCheckRollup")?;
    let rollup = if rollup.is_null() {
        &[][..]
    } else {
        rollup.as_array().context("invalid statusCheckRollup")?
    };
    for check in rollup.iter().take(limit) {
        let name = check
            .get("name")
            .or_else(|| check.get("context"))
            .and_then(Value::as_str)
            .context("check missing name")?;
        let state = check
            .get("conclusion")
            .and_then(Value::as_str)
            .filter(|state| !state.is_empty())
            .or_else(|| check.get("state").and_then(Value::as_str))
            .or_else(|| check.get("status").and_then(Value::as_str))
            .context("check missing state")?;
        checks.push(json!({"name": bounded(name, 256), "state": bounded(state, 80)}));
    }
    Ok(
        json!({"number": number, "title": bounded(required_string(pull, "title")?, 512),
        "url": bounded(required_string(pull, "url")?, 2_048), "branch": bounded(required_string(pull, "headRefName")?, 512),
        "state": required_string(pull, "state")?, "draft": pull.get("isDraft").and_then(Value::as_bool).context("invalid draft flag")?,
        "review": required_string(pull, "reviewDecision")?, "checks": checks, "checks_omitted": rollup.len().saturating_sub(limit)}),
    )
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("PR missing {key}"))
}

fn bounded(value: &str, maximum: usize) -> Value {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    if end == value.len() {
        json!(value)
    } else {
        json!({"text": &value[..end], "omitted_bytes": value.len() - end})
    }
}
