//! `aisc log` and `aisc show`: the read-only verbs.
use super::diff::{self, assign_ids, commit_diff_args, parse_patch};
use super::model::{ChangeSet, FileChange, Kind};
use super::plumbing::Git;
use super::selector::{self, Selection};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, render};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::{Value, json};
use std::fmt::Write;

const CAP: usize = 12;

pub(crate) fn run(args: &cli::Log) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let view = view::read(&git, args.branch.as_deref(), args.limit, true)?;
    let hunks = !args.no_hunks;
    Ok(Outcome::with_text(
        render::log_json(&view, hunks),
        render::log_text(&view, hunks),
    ))
}

fn commit_info(git: &Git, view: &View, oid: &str) -> Result<CommitInfo> {
    if let Some(known) = view.commits.iter().find(|commit| commit.oid == oid) {
        return Ok(known.clone());
    }
    let output = git.run(&[
        "log",
        "-1",
        "--abbrev=4",
        "--format=%x01%H%x1f%h%x1f%T%x1f%P%x1f%s",
        oid,
    ])?;
    view::parse_log(&output)?
        .into_iter()
        .next()
        .ok_or_else(|| fail(Code::Git, format!("cannot read commit {oid}")))
}

fn commit_changes(git: &Git, commit: &CommitInfo) -> Result<ChangeSet> {
    let args = commit_diff_args(commit.parents.is_empty());
    let mut full: Vec<&str> = args;
    if let Some(parent) = commit.parents.first() {
        full.push(parent);
    }
    full.push(&commit.oid);
    let patch = git.run(&full)?;
    let mut files = parse_patch(&patch)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    assign_ids(&mut files);
    Ok(ChangeSet {
        files,
        nested: Vec::new(),
    })
}

enum Subject {
    Uncommitted,
    Commits(Vec<String>),
}

fn subject(
    git: &Git,
    view: &View,
    changes: Option<&ChangeSet>,
    args: &[String],
) -> Result<(Subject, usize)> {
    let Some(first) = args.first() else {
        return Ok((Subject::Uncommitted, 0));
    };
    if changes.is_some_and(|changes| changes.index_of(first).is_some()) {
        return Ok((Subject::Uncommitted, 0));
    }
    if let Some((left, right)) = first.split_once("..")
        && !left.is_empty()
        && !right.is_empty()
    {
        let from = view.resolve(git, left)?;
        let to = view.resolve(git, right)?;
        // View lists newest first; the span is reported oldest first.
        let position = |oid: &str| view.commits.iter().position(|commit| commit.oid == oid);
        let (Some(a), Some(b)) = (position(&from), position(&to)) else {
            return Err(fail(
                Code::Selector,
                format!("'{first}': both ends must be commits of this branch's range"),
            ));
        };
        let (newest, oldest) = (a.min(b), a.max(b));
        let span = view.commits[newest..=oldest]
            .iter()
            .rev()
            .map(|commit| commit.oid.clone())
            .collect();
        return Ok((Subject::Commits(span), 1));
    }
    match view.resolve_opt(git, first)? {
        Some(oid) => Ok((Subject::Commits(vec![oid]), 1)),
        None => Ok((Subject::Uncommitted, 0)),
    }
}

fn shown_hunks<'a>(
    file_index: usize,
    file: &'a FileChange,
    selection: Option<&Selection>,
) -> Vec<&'a super::model::HunkInfo> {
    file.hunks
        .iter()
        .filter(|hunk| match selection {
            None => true,
            Some(selection) => {
                if file.whole() || file.kind == Kind::Untracked {
                    selection.files.contains_key(&file_index)
                } else {
                    hunk.blocks
                        .clone()
                        .any(|block| selection.has_block(file_index, block))
                }
            }
        })
        .collect()
}

fn untracked_lines(git: &Git, file: &FileChange, full: bool) -> Vec<String> {
    if file.binary {
        return Vec::new();
    }
    let bytes = std::fs::read(git.root.join(&file.path)).unwrap_or_default();
    let lines = super::apply::split_lines(&bytes);
    let mut shown: Vec<String> = lines
        .iter()
        .take(if full { usize::MAX } else { CAP })
        .map(|line| {
            let text = String::from_utf8_lossy(line);
            format!("+{}", escape_line(text.strip_suffix('\n').unwrap_or(&text)))
        })
        .collect();
    if !full && lines.len() > CAP {
        shown.push(format!("… +{}", lines.len() - CAP));
    }
    shown
}

fn render_changes(
    git: &Git,
    changes: &ChangeSet,
    selection: Option<&Selection>,
    full: bool,
) -> (String, Value) {
    let mut text = String::new();
    let mut files_json = Vec::new();
    for (file_index, file) in changes.files.iter().enumerate() {
        let hunks = shown_hunks(file_index, file, selection);
        if hunks.is_empty() {
            continue;
        }
        let _ = writeln!(text, "{}", escape_line(&file.path));
        let mut hunks_json = Vec::new();
        for hunk in hunks {
            let body = if file.kind == Kind::Untracked {
                untracked_lines(git, file, full)
            } else {
                render::hunk_body(file, hunk, full, CAP)
            };
            let heading = if file.whole() || file.kind == Kind::Untracked {
                format!(
                    "({}){}",
                    file.kind.label(),
                    if file.lines > 0 {
                        format!(" +{}", file.lines)
                    } else {
                        String::new()
                    }
                )
            } else {
                let counts = if hunk.removed > 0 {
                    format!("+{}-{}", hunk.added, hunk.removed)
                } else {
                    format!("+{}", hunk.added)
                };
                format!("{} @{} {counts}", hunk.short, hunk.line)
            };
            let _ = writeln!(text, " {heading}");
            for line in &body {
                let _ = writeln!(text, "{line}");
            }
            hunks_json.push(
                json!({"id": hunk.short, "line": hunk.line, "added": hunk.added,
                "removed": hunk.removed, "lines": body}),
            );
        }
        files_json.push(json!({"path": file.path, "status": file.status(), "hunks": hunks_json}));
    }
    (text, Value::Array(files_json))
}

pub(crate) fn show(args: &cli::Show) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let view = view::read(&git, args.branch.as_deref(), 1_000, true)?;
    let (target, skip) = subject(&git, &view, view.changes.as_ref(), &args.args)?;
    let tokens = &args.args[skip..];
    let mut text = String::new();
    let mut json_items = Vec::new();
    match target {
        Subject::Uncommitted => {
            let Some(changes) = &view.changes else {
                return Err(fail(
                    Code::Usage,
                    "uncommitted changes are only available for the checked-out branch",
                ));
            };
            let selection = if tokens.is_empty() {
                None
            } else {
                Some(selector::resolve(&git.root, changes, tokens)?)
            };
            let (body, files) = render_changes(&git, changes, selection.as_ref(), args.full);
            if body.is_empty() {
                text.push_str("no uncommitted changes\n");
            } else {
                text.push_str("uncommitted:\n");
                text.push_str(&body);
            }
            json_items.push(json!({"uncommitted": true, "files": files}));
        }
        Subject::Commits(oids) => {
            for oid in oids {
                let commit = commit_info(&git, &view, &oid)?;
                let changes = commit_changes(&git, &commit)?;
                let selection = if tokens.is_empty() {
                    None
                } else {
                    Some(selector::resolve(&git.root, &changes, tokens)?)
                };
                let (body, files) = render_changes(&git, &changes, selection.as_ref(), args.full);
                let _ = writeln!(text, "{} {}", commit.short, escape_line(&commit.subject));
                text.push_str(&body);
                json_items.push(json!({"id": commit.short, "oid": commit.oid,
                    "subject": commit.subject, "files": files}));
            }
        }
    }
    let _ = diff::DIFF_FLAGS;
    Ok(Outcome::with_text(
        json!({"items": json_items}),
        text.trim_end().to_owned(),
    ))
}
