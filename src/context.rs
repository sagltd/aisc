use crate::git::{Entry, is_conflict, run_git, text, validate_environment};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;

const STATUS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "--branch",
    "-z",
    "--renames",
    "--untracked-files=all",
];

pub(crate) fn read(root: &Path, limit: usize) -> Result<Value> {
    validate_environment()?;
    // Native Git resolves the checkout; independent reads overlap without touching its index.
    let (status, staged, unstaged) = std::thread::scope(|scope| {
        let status = scope.spawn(|| run_git(root, STATUS, None));
        let staged = scope.spawn(|| numstat(root, true, limit));
        let unstaged = scope.spawn(|| numstat(root, false, limit));
        (status.join(), staged.join(), unstaged.join())
    });
    let status = status.map_err(|_| anyhow::anyhow!("Git status reader panicked"))??;
    let staged_stat = staged.map_err(|_| anyhow::anyhow!("staged diff reader panicked"))??;
    let unstaged_stat =
        unstaged.map_err(|_| anyhow::anyhow!("unstaged diff reader panicked"))??;
    if run_git(root, STATUS, None)? != status {
        bail!("repository changed while reading context; retry");
    }
    let (head, branch, entries) = parse_status(&status)?;
    let staged = entries
        .iter()
        .filter(|entry| !matches!(entry.status.as_bytes()[0], b' ' | b'?'))
        .count();
    let unstaged = entries
        .iter()
        .filter(|entry| !matches!(entry.status.as_bytes()[1], b' ' | b'?'))
        .count();
    let untracked = entries.iter().filter(|entry| entry.status == "??").count();
    let conflicts = entries
        .iter()
        .filter(|entry| is_conflict(&entry.status))
        .count();
    Ok(
        json!({"branch": branch, "head": head, "staged": staged, "unstaged": unstaged, "untracked": untracked, "conflicts": conflicts,
        "files": entries.iter().take(limit).collect::<Vec<_>>(), "omitted": entries.len().saturating_sub(limit),
        "diffstat": {"staged": staged_stat, "unstaged": unstaged_stat}}),
    )
}

fn parse_status(bytes: &[u8]) -> Result<(String, Option<String>, Vec<Entry>)> {
    let mut parts = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    let mut head = None;
    let mut branch = None;
    let mut entries = Vec::new();
    while let Some(record) = parts.next() {
        if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            let value = text(value)?;
            if value == "(initial)" {
                bail!("context requires an initial Git commit");
            }
            head = Some(value.to_owned());
        } else if let Some(value) = record.strip_prefix(b"# branch.head ") {
            let value = text(value)?;
            branch = Some((value != "(detached)").then(|| value.to_owned()));
        } else if record.starts_with(b"# ") {
            continue;
        } else if record.starts_with(b"? ") {
            entries.push(Entry {
                status: "??".to_owned(),
                path: text(&record[2..])?.to_owned(),
                original_path: None,
            });
        } else {
            let count = match record.first() {
                Some(b'1') => 9,
                Some(b'2') => 10,
                Some(b'u') => 11,
                _ => bail!("unsupported Git porcelain v2 record"),
            };
            let fields = record
                .splitn(count, |byte| *byte == b' ')
                .collect::<Vec<_>>();
            if fields.len() != count {
                bail!("incomplete Git status record");
            }
            let status = fields.get(1).context("status missing change flags")?;
            if status.len() != 2 {
                bail!("invalid Git status flags");
            }
            let status = text(status)?.replace('.', " ");
            let path = text(fields.last().context("status missing path")?)?.to_owned();
            let original_path = if record[0] == b'2' {
                Some(text(parts.next().context("rename missing original path")?)?.to_owned())
            } else {
                None
            };
            entries.push(Entry {
                status,
                path,
                original_path,
            });
        }
    }
    Ok((
        head.context("Git status missing HEAD")?,
        branch.context("Git status missing branch")?,
        entries,
    ))
}

fn numstat(root: &Path, cached: bool, limit: usize) -> Result<Value> {
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--find-renames",
        "--numstat",
        "-z",
    ];
    if cached {
        args.push("--cached");
    }
    let output = run_git(root, &args, None)?;
    let mut records = Vec::new();
    let mut parts = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = parts.next() {
        let fields = record.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            bail!("invalid Git numstat record");
        }
        if fields[2].is_empty() {
            let original = parts
                .next()
                .context("numstat rename missing original path")?;
            let path = parts.next().context("numstat rename missing destination")?;
            records.push(json!({"path": text(path)?, "original_path": text(original)?, "added": text(fields[0])?, "deleted": text(fields[1])?}));
        } else {
            records.push(json!({"path": text(fields[2])?, "added": text(fields[0])?, "deleted": text(fields[1])?}));
        }
    }
    Ok(
        json!({"files": records.iter().take(limit).collect::<Vec<_>>(), "omitted": records.len().saturating_sub(limit)}),
    )
}
