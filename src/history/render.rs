//! Compact text and JSON renderers for the branch view and hunks.
use super::model::{ChangeSet, FileChange, HunkInfo, Kind};
use super::view::View;
use crate::output::escape_line;
use serde_json::{Value, json};
use std::fmt::Write;

const HUNK_LINE_CAP: usize = 200;
const FILES_PER_COMMIT: usize = 4;

fn counts(hunk: &HunkInfo) -> String {
    if hunk.removed > 0 {
        format!("+{}-{}", hunk.added, hunk.removed)
    } else {
        format!("+{}", hunk.added)
    }
}

fn hunk_line(hunk: &HunkInfo) -> String {
    let mut line = format!("{} @{} {}", hunk.short, hunk.line, counts(hunk));
    if !hunk.hint.is_empty() {
        line.push(' ');
        line.push_str(&hunk.hint);
    }
    if hunk.ws {
        line.push_str(" [ws]");
    }
    line
}

fn whole_label(file: &FileChange) -> String {
    let mut label = format!("{} ({})", escape_line(&file.path), file.kind.label());
    let with_lines = matches!(file.kind, Kind::Untracked | Kind::Added) && file.lines > 0;
    if with_lines {
        let _ = write!(label, " +{}", file.lines);
    }
    label
}

pub(crate) fn header(view: &View) -> String {
    let mut line = header_line(view);
    line.push_str(" (newest first)");
    line
}

/// The header without the commit-list suffix: what a mutation prints by default.
pub(crate) fn header_line(view: &View) -> String {
    let name = escape_line(&view.label());
    let mut line = match &view.base {
        Some(base) => {
            let noun = if view.total == 1 { "commit" } else { "commits" };
            let mut line = format!(
                "{name}: {} {noun} on {}",
                view.total,
                escape_line(&base.label)
            );
            if base.behind > 0 {
                let _ = write!(
                    line,
                    ", {} +{} ahead (aisc sync)",
                    escape_line(&base.label),
                    base.behind
                );
            }
            line
        }
        None => {
            let noun = if view.commits.len() == 1 {
                "commit"
            } else {
                "commits"
            };
            format!("{name}: last {} {noun}", view.commits.len())
        }
    };
    if let Some(upstream) = &view.upstream {
        let _ = write!(
            line,
            ", ↑{} +{}/-{}",
            escape_line(&upstream.label),
            upstream.ahead,
            upstream.behind
        );
    }
    line
}

fn commit_lines(view: &View, out: &mut String) {
    for commit in &view.commits {
        let _ = write!(out, "{} {}", commit.short, escape_line(&commit.subject));
        if !commit.files.is_empty() {
            let shown: Vec<String> = commit
                .files
                .iter()
                .take(FILES_PER_COMMIT)
                .map(|file| escape_line(file))
                .collect();
            let _ = write!(out, " | {}", shown.join(" "));
            if commit.files.len() > FILES_PER_COMMIT {
                let _ = write!(out, " +{}", commit.files.len() - FILES_PER_COMMIT);
            }
        }
        out.push('\n');
    }
    if view.total > view.commits.len() && view.base.is_some() {
        let _ = writeln!(
            out,
            "… {} more (aisc log --limit N)",
            view.total - view.commits.len()
        );
    }
}

fn state_lines(view: &View, out: &mut String) {
    if let Some(changes) = &view.changes
        && !changes.nested.is_empty()
    {
        let _ = writeln!(
            out,
            "ignored nested repositories: {}",
            changes
                .nested
                .iter()
                .map(|path| escape_line(path))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    if let Some(status) = &view.status
        && !status.unmerged.is_empty()
    {
        let _ = writeln!(
            out,
            "unmerged: {}",
            status
                .unmerged
                .iter()
                .map(|path| escape_line(path))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    if let Some(conflicts) = view.sync_paused {
        let _ = writeln!(
            out,
            "sync paused: {conflicts} conflicts (aisc sync --continue | --abort)"
        );
    }
}

fn changes_with_hunks(changes: &ChangeSet, out: &mut String) {
    let mut remaining = HUNK_LINE_CAP;
    let mut omitted = 0;
    for file in &changes.files {
        let path = escape_line(&file.path);
        if file.whole() || file.kind == Kind::Untracked {
            let _ = writeln!(out, "{}", whole_label(file));
            continue;
        }
        let ws = if file.ws { " [ws]" } else { "" };
        if file.hunks.len() == 1 {
            if remaining == 0 {
                omitted += 1;
                continue;
            }
            remaining -= 1;
            let _ = writeln!(out, "{path} {}", hunk_line(&file.hunks[0]));
        } else {
            let _ = writeln!(out, "{path}{ws}");
            for hunk in &file.hunks {
                if remaining == 0 {
                    omitted += 1;
                    continue;
                }
                remaining -= 1;
                let _ = writeln!(out, " {}", hunk_line(hunk));
            }
        }
    }
    if omitted > 0 {
        let _ = writeln!(out, "… {omitted} more hunks (aisc show)");
    }
}

fn path_line(changes: &ChangeSet) -> String {
    changes
        .files
        .iter()
        .map(|file| escape_line(&file.path))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Full log: header, commits, then uncommitted changes with hunk ids.
pub(crate) fn log_text(view: &View, hunks: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", header(view));
    commit_lines(view, &mut out);
    if let Some(changes) = &view.changes
        && !changes.files.is_empty()
    {
        if hunks {
            out.push_str("uncommitted:\n");
            changes_with_hunks(changes, &mut out);
        } else {
            let _ = writeln!(out, "uncommitted: {}", path_line(changes));
        }
    }
    state_or_clean(view, &mut out);
    out.trim_end().to_owned()
}

/// The state lines, or `clean` when the worktree has no changes and nothing is paused, so an
/// agent need not run `git status` to confirm.
fn state_or_clean(view: &View, out: &mut String) {
    let before = out.len();
    state_lines(view, out);
    if out.len() == before && view.changes.as_ref().is_some_and(|c| c.files.is_empty()) {
        out.push_str("clean: nothing uncommitted, no paused sync\n");
    }
}

/// Compact log printed after a mutation: uncommitted changes as one line of paths.
pub(crate) fn compact_text(view: &View) -> String {
    log_text(view, false)
}

/// `aisc log -q`: the header, one `ID SUBJECT` line per commit and the uncommitted paths.
pub(crate) fn quiet_log_text(view: &View) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", header(view));
    for commit in &view.commits {
        let _ = writeln!(out, "{} {}", commit.short, escape_line(&commit.subject));
    }
    if view.total > view.commits.len() && view.base.is_some() {
        let _ = writeln!(
            out,
            "… {} more (aisc log --limit N)",
            view.total - view.commits.len()
        );
    }
    out.push_str(&changes_and_state(view));
    out.trim_end().to_owned()
}

/// The uncommitted paths (one line) and the state lines (nested repositories, unmerged paths,
/// a paused sync).
pub(crate) fn changes_and_state(view: &View) -> String {
    let mut out = String::new();
    if let Some(changes) = &view.changes
        && !changes.files.is_empty()
    {
        let _ = writeln!(out, "uncommitted: {}", path_line(changes));
    }
    state_or_clean(view, &mut out);
    out.trim_end().to_owned()
}

fn file_json(file: &FileChange, hunks: bool) -> Value {
    let mut value = json!({"path": file.path, "status": file.status(), "ws": file.ws});
    if file.whole() || file.kind == Kind::Untracked {
        value["kind"] = json!(file.kind.label());
        if file.lines > 0 {
            value["lines"] = json!(file.lines);
        }
    }
    if hunks {
        value["hunks"] = Value::Array(
            file.hunks
                .iter()
                .map(|hunk| {
                    json!({"id": hunk.short, "line": hunk.line, "added": hunk.added,
                        "removed": hunk.removed, "hint": hunk.hint, "ws": hunk.ws})
                })
                .collect(),
        );
    }
    value
}

pub(crate) fn log_json(view: &View, hunks: bool) -> Value {
    let commits: Vec<Value> = view
        .commits
        .iter()
        .map(|commit| {
            let omitted = commit.files.len().saturating_sub(FILES_PER_COMMIT);
            json!({"id": commit.short, "oid": commit.oid, "subject": commit.subject,
                "files": commit.files.iter().take(FILES_PER_COMMIT).collect::<Vec<_>>(),
                "files_omitted": omitted})
        })
        .collect();
    let changes: Vec<Value> = view
        .changes
        .as_ref()
        .map(|changes| {
            changes
                .files
                .iter()
                .map(|file| file_json(file, hunks))
                .collect()
        })
        .unwrap_or_default();
    let hunk_total: usize = view.changes.as_ref().map_or(0, |changes| {
        changes.files.iter().map(|file| file.hunks.len()).sum()
    });
    json!({
        "branch": view.branch,
        "head": view.tip,
        "base": view.base.as_ref().map(|base| json!({"ref": base.label, "oid": base.oid,
            "ahead": base.ahead, "behind": base.behind})),
        "upstream": view.upstream.as_ref().map(|up| json!({"ref": up.label, "ahead": up.ahead, "behind": up.behind})),
        "commits": commits,
        "total": view.total,
        "changes": changes,
        "omitted_hunks": if hunks { hunk_total.saturating_sub(HUNK_LINE_CAP) } else { 0 },
        "state": view.sync_paused.map(|conflicts| json!({"sync": {"conflicts": conflicts}})),
    })
}

fn display_line(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    escape_line(text.strip_suffix('\n').unwrap_or(&text))
}

/// Changed lines of one hunk: `-old`/`+new`, capped unless `full` (which adds context).
pub(crate) fn hunk_body(file: &FileChange, hunk: &HunkInfo, full: bool, cap: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut printed_until = 0usize;
    let blocks = &file.blocks[hunk.blocks.clone()];
    for block in blocks {
        if full {
            for (offset, line) in block.before.iter().enumerate() {
                let index = block.old_pos - block.before.len() + offset;
                if index >= printed_until {
                    lines.push(format!(" {}", display_line(line)));
                }
            }
        }
        lines.extend(
            block
                .removed
                .iter()
                .map(|line| format!("-{}", display_line(line))),
        );
        lines.extend(
            block
                .added
                .iter()
                .map(|line| format!("+{}", display_line(line))),
        );
        printed_until = block.old_pos + block.removed.len();
        if full {
            for (offset, line) in block.after.iter().enumerate() {
                lines.push(format!(" {}", display_line(line)));
                printed_until = block.old_pos + block.removed.len() + offset + 1;
            }
        }
    }
    if !full && lines.len() > cap {
        let extra = lines.len() - cap;
        lines.truncate(cap);
        lines.push(format!("… +{extra} (--full)"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::diff::{assign_ids, parse_patch};

    #[test]
    fn hunk_body_caps_changed_lines_and_full_adds_context() {
        let mut patch = String::from(
            "diff --git a/f b/f\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,18 @@\n k1\n",
        );
        for number in 0..15 {
            patch.push_str(&format!("+line {number}\n"));
        }
        patch.push_str(" k2\n k3\n");
        let mut files = parse_patch(patch.as_bytes()).unwrap();
        assign_ids(&mut files);
        let capped = hunk_body(&files[0], &files[0].hunks[0], false, 12);
        assert_eq!(capped.len(), 13);
        assert_eq!(capped.last().unwrap(), "… +3 (--full)");
        let full = hunk_body(&files[0], &files[0].hunks[0], true, 12);
        assert_eq!(full.len(), 15 + 3);
        assert_eq!(full[0], " k1");
    }
}
