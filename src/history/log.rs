//! `aisc log` and `aisc show`: the read-only verbs.
use super::diff::{self, assign_ids, commit_diff_args, parse_patch};
use super::model::{ChangeSet, FileChange, Kind};
use super::plumbing::Git;
use super::selector::{self, Selection};
use super::view::{self, CommitInfo, View};
use super::{Code, Outcome, fail, fail_hint, render};
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
    super::rewrite::recover_before_read(&git);
    let branch = args.branch.as_deref().or(args.target.as_deref());
    let view = view::read_any(&git, branch, args.limit, true)?;
    let hunks = !args.no_hunks && !args.quiet;
    let text = if args.quiet {
        render::quiet_log_text(&view)
    } else {
        render::log_text(&view, hunks)
    };
    Ok(Outcome::with_text(render::log_json(&view, hunks), text))
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

/// `aisc branches [COMMIT]`: local branches, or the ones whose history contains COMMIT. COMMIT
/// is resolved on the checked-out branch first, then as a Git revision, then as an exact subject
/// on any other local branch.
pub(crate) fn branches(args: &cli::Branches) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    super::rewrite::recover_before_read(&git);
    let view = view::read_any(&git, None, 1_000, false)?;
    let mut commit = None;
    if let Some(spec) = &args.commit {
        let oid = match view.resolve_opt(&git, spec)? {
            Some(oid) => oid,
            None => {
                let holders = super::amend_elsewhere::branches_holding(&git, &view, spec)?;
                let Some(first) = holders.first() else {
                    return Err(fail_hint(
                        Code::Selector,
                        format!("unknown commit '{spec}'"),
                        "use a commit id, an exact subject on a local branch, or a revision",
                    ));
                };
                view::read(&git, Some(first), 1_000, false)?.resolve(&git, spec)?
            }
        };
        let message = git.read_commit_meta(&oid)?.message;
        let subject = String::from_utf8_lossy(&message)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned();
        commit = Some((oid, subject));
    }
    let mut argv = vec!["branch", "--format=%(refname:short)"];
    if let Some((oid, _)) = &commit {
        argv.extend(["--contains", oid.as_str()]);
    }
    let listing = String::from_utf8_lossy(&git.run(&argv)?).into_owned();
    let checked_out = view.branch.clone();
    let mut names: Vec<String> = listing
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with("(HEAD detached"))
        .map(str::to_owned)
        .collect();
    names.sort();
    let text = if names.is_empty() {
        match &commit {
            Some((oid, _)) => format!("no local branch contains {}", &oid[..oid.len().min(7)]),
            None => "no local branches".to_owned(),
        }
    } else {
        names
            .iter()
            .map(|name| escape_line(name))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let branches: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "checked_out": checked_out.as_deref() == Some(name.as_str())}))
        .collect();
    let commit = commit.map(|(oid, subject)| json!({"oid": oid, "subject": subject}));
    Ok(Outcome::with_text(
        json!({"branches": branches, "commit": commit}),
        text,
    ))
}

/// `REV:PATH` tokens name a file at a commit. `PATH:A-B` (a line range) stays a selector, so
/// `f.txt:3-5` is never read as a file; the first token decides the form.
fn file_specs(args: &[String]) -> Option<Vec<(&str, &str)>> {
    let mut specs = vec![file_spec(args.first()?)?];
    for arg in &args[1..] {
        specs.push(file_spec(arg)?);
    }
    Some(specs)
}

fn file_spec(arg: &str) -> Option<(&str, &str)> {
    let (rev, path) = arg.split_once(':')?;
    (!rev.is_empty() && !path.is_empty() && !is_line_range(path)).then_some((rev, path))
}

fn is_line_range(text: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    match text.split_once('-') {
        Some((from, to)) => digits(from) && digits(to),
        None => digits(text),
    }
}

/// The whole content of each `REV:PATH`, as an agent reviewing a commit needs it.
fn show_files(git: &Git, view: &View, specs: &[(&str, &str)]) -> Result<Outcome> {
    let mut text = String::new();
    let mut items = Vec::new();
    for (rev, path) in specs {
        let oid = view.resolve(git, rev)?;
        let short = view
            .commits
            .iter()
            .find(|commit| commit.oid == oid)
            .map_or_else(|| oid[..4].to_owned(), |commit| commit.short.clone());
        let bytes = git.blob_at(&oid, path).map_err(|_| {
            fail_hint(
                Code::Selector,
                format!("no file '{}' in {short}", escape_line(path)),
                "run aisc show COMMIT for the files it changes",
            )
        })?;
        if specs.len() > 1 {
            let _ = writeln!(text, "== {short}:{}", escape_line(path));
        }
        if bytes.contains(&0) {
            let _ = writeln!(text, "(binary, {} bytes)", bytes.len());
            items.push(
                json!({"id": short, "oid": oid, "path": path, "binary": true, "size": bytes.len()}),
            );
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        for line in content.split_inclusive('\n') {
            let _ = writeln!(
                text,
                "{}",
                escape_line(line.strip_suffix('\n').unwrap_or(line))
            );
        }
        items.push(json!({"id": short, "oid": oid, "path": path, "content": content}));
    }
    Ok(Outcome::with_text(
        json!({"items": items}),
        text.trim_end_matches('\n').to_owned(),
    ))
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
        shown.push(format!("… +{} (--full)", lines.len() - CAP));
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
    super::rewrite::recover_before_read(&git);
    let view = view::read_any(&git, args.branch.as_deref(), 1_000, true)?;
    if let Some(specs) = file_specs(&args.args) {
        return show_files(&git, &view, &specs);
    }
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
