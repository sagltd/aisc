use crate::git::Repo;
use crate::process;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::process::Command;
use std::time::Duration;

const RESPONSE_LIMIT: usize = 4 * 1_024 * 1_024;
const MAX_LIMIT: usize = 100;

pub(crate) fn overview(repo: &Repo, limit: usize) -> Result<Value> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        bail!("--limit must be between 1 and {MAX_LIMIT}");
    }
    let Some(branch) = repo.branch()? else {
        return Ok(json!({
            "current_branch": null,
            "queried": false,
            "merge_requests": null,
            "scope": "detached HEAD; no GitLab request made"
        }));
    };

    let page_size = limit.saturating_add(1).min(MAX_LIMIT);
    let mut command = Command::new("glab");
    command
        .args(["mr", "list", "--source-branch"])
        .arg(&branch)
        .args(["--per-page"])
        .arg(page_size.to_string())
        .args(["--output", "json"])
        .current_dir(&repo.root)
        .env("GLAB_NO_PROMPT", "true")
        .env("GLAB_CHECK_UPDATE", "false")
        .env("GLAB_PAGER", "cat")
        .env("NO_COLOR", "1")
        .env_remove("GLAB_DEBUG")
        .env_remove("GLAB_DEBUG_HTTP")
        .env_remove("DEBUG");
    let output = process::execute(&mut command, Duration::from_secs(30), RESPONSE_LIMIT)
        .context("could not run glab; install and authenticate the GitLab CLI first")?;
    if output.timed_out {
        bail!("glab mr list timed out after 30 seconds");
    }
    if !output.success() {
        let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        let category = if error.contains("auth")
            || error.contains("token")
            || error.contains("login")
            || error.contains("unauthorized")
            || error.contains("forbidden")
        {
            "authentication"
        } else if error.contains("network")
            || error.contains("resolve")
            || error.contains("connection")
            || error.contains("api")
            || error.contains("http")
            || error.contains("tls")
            || error.contains("dns")
        {
            "network/API"
        } else {
            "GitLab CLI"
        };
        bail!(
            "glab mr list {category} failure (exit {:?}); run glab mr list directly to diagnose",
            output.code
        );
    }
    if output.stdout_omitted != 0 || output.stderr_omitted != 0 {
        bail!("GitLab response exceeds the 4 MiB safety limit");
    }
    let requests: Value =
        serde_json::from_slice(&output.stdout).context("invalid glab JSON response")?;
    let requests = requests
        .as_array()
        .context("glab response must be a JSON array")?;
    let items = requests
        .iter()
        .take(limit)
        .map(compact)
        .collect::<Result<Vec<_>>>()?;
    let more = requests.len() == page_size;

    Ok(json!({
        "current_branch": bounded(&branch, 512),
        "queried": true,
        "merge_requests": {
            "items": items,
            "omitted": requests.len().saturating_sub(limit),
            "may_have_more": more
        },
        "scope": "open merge requests with this exact source branch; GitLab may omit pipeline or mergeability fields based on visibility and freshness; review inbox is not included"
    }))
}

fn compact(request: &Value) -> Result<Value> {
    let number = request
        .get("iid")
        .and_then(Value::as_u64)
        .context("merge request missing valid iid")?;
    let title = required_string(request, "title")?;
    let url = required_string(request, "web_url")?;
    let branch = required_string(request, "source_branch")?;
    let state = required_string(request, "state")?;
    let draft = request
        .get("draft")
        .map(|value| {
            value
                .as_bool()
                .context("merge request has invalid draft flag")
        })
        .transpose()?;
    let mergeability = optional_string(request, "detailed_merge_status")?;
    let pipeline = match request.get("head_pipeline") {
        None | Some(Value::Null) => Value::Null,
        Some(value) => {
            if !value.is_object() {
                bail!("GitLab response has invalid head_pipeline");
            }
            let status = optional_string(value, "status")?;
            let url = optional_string(value, "web_url")?;
            json!({"status": status.map(|value| bounded(value, 80)), "url": url.map(|value| bounded(value, 2_048))})
        }
    };

    Ok(json!({
        "number": number,
        "title": bounded(title, 512),
        "url": bounded(url, 2_048),
        "branch": bounded(branch, 512),
        "state": bounded(state, 80),
        "draft": draft,
        "mergeability": mergeability.map(|value| bounded(value, 80)),
        "pipeline": pipeline
    }))
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("merge request missing {key}"))
}

fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .with_context(|| format!("GitLab response has invalid {key}")),
    }
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
