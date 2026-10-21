use crate::cli::Color;
use serde_json::{Value, json};
use std::fmt::Write;
use std::io::IsTerminal;

pub(crate) struct Presentation {
    pub json: bool,
    pub color: Color,
    no_color: bool,
}

impl Presentation {
    pub fn from_arguments(arguments: &[std::ffi::OsString]) -> Self {
        let mut json = !std::io::stdout().is_terminal();
        let mut color = Color::Auto;
        let mut values = arguments.iter().skip(1);
        while let Some(value) = values.next() {
            if value == "--" {
                break;
            }
            if value == "--json" {
                json = true;
            }
            if value == "--human" {
                json = false;
            }
            let selected = if value == "--color" {
                values.next().and_then(|value| value.to_str())
            } else {
                value
                    .to_str()
                    .and_then(|value| value.strip_prefix("--color="))
            };
            if let Some(selected) = selected {
                color = match selected {
                    "always" => Color::Always,
                    "never" => Color::Never,
                    _ => Color::Auto,
                };
            }
        }
        // Explicit JSON wins for malformed conflicting flags, so parser errors stay machine-readable.
        if arguments
            .iter()
            .skip(1)
            .take_while(|value| *value != "--")
            .any(|value| value == "--json")
        {
            json = true;
        }
        Self {
            json,
            color,
            no_color: std::env::var_os("NO_COLOR").is_some(),
        }
    }

    pub fn colors(&self, stderr: bool) -> bool {
        if self.json || self.no_color {
            return false;
        }
        match self.color {
            Color::Always => true,
            Color::Never => false,
            Color::Auto => {
                if stderr {
                    std::io::stderr().is_terminal()
                } else {
                    std::io::stdout().is_terminal()
                }
            }
        }
    }

    pub fn clap_color(&self) -> clap::ColorChoice {
        if self.colors(false) {
            clap::ColorChoice::Always
        } else {
            clap::ColorChoice::Never
        }
    }

    pub fn success(&self, operation: &str, value: &Value) -> String {
        if self.json {
            return value.to_string();
        }
        render(operation, value, self.colors(false))
    }

    pub fn error(&self, message: &str) -> String {
        if self.json {
            return json!({"error": message}).to_string();
        }
        format!(
            "{} {}",
            paint("Error:", "31;1", self.colors(true)),
            safe_block(message)
        )
    }

    pub fn help(&self, help: &str, styled: &str) -> String {
        if self.json {
            json!({"help": help}).to_string()
        } else if self.colors(false) {
            styled.to_owned()
        } else {
            help.to_owned()
        }
    }
}

fn render(operation: &str, value: &Value, colors: bool) -> String {
    let mut result = String::new();
    let label = |name: &str| paint(name, "36;1", colors);
    match operation {
        "context" => {
            let _ = writeln!(
                result,
                "{} {}  {} {}",
                label("Branch"),
                display(&value["branch"]),
                label("HEAD"),
                display(&value["head"])
            );
            let _ = writeln!(
                result,
                "{} staged {} · unstaged {} · untracked {} · conflicts {}",
                label("Changes"),
                value["staged"],
                value["unstaged"],
                value["untracked"],
                value["conflicts"]
            );
            if let Some(files) = value["files"].as_array() {
                for file in files {
                    let status = file["status"].as_str().unwrap_or("??");
                    let status = paint(
                        status,
                        if status.contains('U') || status == "AA" || status == "DD" {
                            "31;1"
                        } else {
                            "33"
                        },
                        colors,
                    );
                    let original = file
                        .get("original_path")
                        .map(|path| format!("{} → ", path))
                        .unwrap_or_default();
                    let _ = writeln!(result, "  {status} {original}{}", file["path"]);
                }
            }
            omissions(&mut result, "files", &value["omitted"]);
            for side in ["staged", "unstaged"] {
                if let Some(files) = value["diffstat"][side]["files"].as_array() {
                    for file in files {
                        let _ = writeln!(
                            result,
                            "  {side}: +{} -{} {}",
                            display(&file["added"]),
                            display(&file["deleted"]),
                            file["path"]
                        );
                    }
                }
                omissions(
                    &mut result,
                    &format!("{side} stats"),
                    &value["diffstat"][side]["omitted"],
                );
            }
        }
        "diff" => {
            result.push_str(&safe_block(value["diff"].as_str().unwrap_or_default()));
            omissions(&mut result, "diff bytes", &value["omitted_bytes"]);
        }
        "start" => {
            let _ = writeln!(
                result,
                "{} {} ({})\nBranch {}\nPath {}\nHEAD {}",
                label("Started"),
                display(&value["task"]),
                display(&value["mode"]),
                display(&value["branch"]),
                display(&value["path"]),
                display(&value["head"])
            );
        }
        "checkpoint" => {
            let _ = writeln!(
                result,
                "{} {}\nBranch {}\nPaths {}\nVerification required",
                label("Checkpoint"),
                display(&value["commit"]),
                display(&value["branch"]),
                value["paths"]
            );
        }
        "verify" | "finish" => {
            let title = if operation == "finish" {
                "Ready for review"
            } else {
                "Verified"
            };
            let _ = writeln!(
                result,
                "{} {}\nCommand {}",
                paint(title, "32;1", colors),
                display(&value["commit"]),
                value["command"]
            );
            if let Some(branch) = value.get("branch") {
                let _ = writeln!(result, "Branch {}", display(branch));
            }
            for stream in ["stdout", "stderr"] {
                if let Some(text) = value[stream].as_str().filter(|text| !text.is_empty()) {
                    let _ = writeln!(result, "{stream}:\n{}", safe_block(text));
                }
                omissions(
                    &mut result,
                    &format!("{stream} bytes"),
                    &value[format!("{stream}_omitted_bytes")],
                );
            }
        }
        "github" => {
            let _ = writeln!(result, "{}", label("GitHub overview"));
            let _ = writeln!(result, "Current branch:");
            if value["current_branch"].is_null() {
                let _ = writeln!(result, "  No pull request returned");
            } else {
                pull_request(&mut result, &value["current_branch"], colors);
            }
            for (key, title) in [
                ("created_by", "Authored"),
                ("needs_review", "Review requested"),
            ] {
                let _ = writeln!(result, "{title}:");
                if let Some(pulls) = value[key]["pull_requests"].as_array() {
                    if pulls.is_empty() {
                        let _ = writeln!(result, "  None returned");
                    }
                    for pull in pulls {
                        pull_request(&mut result, pull, colors);
                    }
                }
                omissions(&mut result, "pull requests", &value[key]["omitted"]);
            }
            let _ = writeln!(result, "Scope: {}", display(&value["scope"]));
        }
        "version" => {
            let _ = writeln!(result, "aisc {}", display(&value["version"]));
        }
        "gitlab" => {
            let _ = writeln!(
                result,
                "{}\nBranch {}",
                label("GitLab overview"),
                display(&value["current_branch"])
            );
            if let Some(requests) = value["merge_requests"]["items"].as_array() {
                for request in requests {
                    let _ = writeln!(
                        result,
                        "  !{} {} [{}] mergeability: {}\n    pipeline: {}\n    {}",
                        request["number"],
                        display(&request["title"]),
                        display(&request["state"]),
                        display(&request["mergeability"]),
                        display(&request["pipeline"]["status"]),
                        display(&request["url"])
                    );
                }
                if requests.is_empty() {
                    let _ = writeln!(result, "  No merge requests returned");
                }
                omissions(
                    &mut result,
                    "merge requests",
                    &value["merge_requests"]["omitted"],
                );
                if value["merge_requests"]["may_have_more"] == true {
                    let _ = writeln!(result, "  More results may exist upstream");
                }
            }
            let _ = writeln!(result, "Scope: {}", display(&value["scope"]));
        }
        _ => {
            let _ = writeln!(
                result,
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_default()
            );
        }
    }
    result.trim_end_matches('\n').to_owned()
}

fn pull_request(result: &mut String, pull: &Value, colors: bool) {
    let _ = writeln!(
        result,
        "  #{} {} [{}] review: {}{}",
        pull["number"],
        display(&pull["title"]),
        display(&pull["state"]),
        display(&pull["review"]),
        if pull["draft"] == true {
            " · draft"
        } else {
            ""
        }
    );
    if let Some(checks) = pull["checks"].as_array() {
        for check in checks {
            let state = display(&check["state"]);
            let code = if state == "SUCCESS" {
                "32"
            } else if state == "FAILURE" {
                "31"
            } else {
                "33"
            };
            let _ = writeln!(
                result,
                "    {}: {}",
                display(&check["name"]),
                paint(&state, code, colors)
            );
        }
    }
    omissions(result, "checks", &pull["checks_omitted"]);
    let _ = writeln!(result, "    {}", display(&pull["url"]));
}

fn omissions(result: &mut String, name: &str, count: &Value) {
    if let Some(count) = count.as_u64().filter(|count| *count > 0) {
        let _ = writeln!(result, "  … {count} {name} omitted");
    }
}
fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text
            .chars()
            .flat_map(|character| {
                if character.is_control() {
                    character.escape_default().collect::<Vec<_>>()
                } else {
                    vec![character]
                }
            })
            .collect(),
        Value::Null => "(detached/none)".to_owned(),
        _ => value.to_string(),
    }
}
fn paint(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}
fn safe_block(text: &str) -> String {
    text.chars()
        .flat_map(|character| {
            if character.is_control() && character != '\n' && character != '\t' {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_status_keeps_text_labels_and_escapes_untrusted_terminal_controls() {
        let value = json!({"branch":"main","head":"abc","staged":1,"unstaged":0,"untracked":0,"conflicts":1,"files":[{"path":"bad\u{1b}[31m\nname","status":"UU"}],"omitted":2,"diffstat":{}});
        let plain = render("context", &value, false);
        assert!(plain.contains("conflicts 1"));
        assert!(plain.contains("2 files omitted"));
        assert!(!plain.contains('\x1b'));
        let colored = render("context", &value, true);
        assert!(colored.contains("\x1b[31;1mUU"));
    }
}
