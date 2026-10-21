//! Operation log: one commit per history operation under `refs/aisc/oplog/<worktree>`.
//! Each op commit keeps old and new commits reachable (extra parents) and stores pre-op file bytes.
#![allow(dead_code)] // worktree restore data is consumed by later slices
use super::plumbing::{Git, TreeEdit};
use super::rewrite::Plan;
use super::{Code, Outcome, fail, fail_hint};
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpRef {
    #[serde(rename = "ref")]
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpHead {
    pub from: Option<String>,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpFile {
    pub path: String,
    /// Index into `wt/<n>` in the op tree holding the pre-operation bytes.
    pub before: Option<usize>,
    /// Blob id (no filters) of the bytes the operation wrote; `None` when deleted.
    pub after: Option<String>,
    pub mode: String,
    /// Mode before the operation (absent in records written before modes were tracked).
    #[serde(default)]
    pub before_mode: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpIndex {
    pub path: String,
    pub from: Option<(String, String)>,
    pub to: Option<(String, String)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OpData {
    pub v: u32,
    pub verb: String,
    pub argv: Vec<String>,
    pub summary: String,
    pub time: u64,
    pub refs: Vec<OpRef>,
    pub head: Option<OpHead>,
    pub worktree: Vec<OpFile>,
    pub index: Vec<OpIndex>,
    pub index_before_tree: Option<String>,
    pub undo_of: Option<String>,
}

/// An op ready to be written, with the pre-op bytes of the files it will write.
pub(crate) struct OpRecord {
    pub data: OpData,
    pub files: Vec<Vec<u8>>,
    pub after: Vec<Option<Vec<u8>>>,
}

impl OpRecord {
    pub fn from_plan(plan: &Plan) -> Self {
        let mut files = Vec::new();
        let mut worktree = Vec::new();
        let after = plan.worktree.iter().map(|w| w.after.clone()).collect();
        for write in &plan.worktree {
            let before = write.before.as_ref().map(|bytes| {
                files.push(bytes.clone());
                files.len() - 1
            });
            worktree.push(OpFile {
                path: write.path.clone(),
                before,
                after: None, // filled by `build` once the blob id is known
                mode: write.mode.clone(),
                before_mode: write.before_mode.clone(),
            });
        }
        let data = OpData {
            v: 1,
            verb: plan.verb.clone(),
            argv: plan.argv.clone(),
            summary: plan.summary.clone(),
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
            refs: plan
                .refs
                .iter()
                .map(|movement| OpRef {
                    name: movement.name.clone(),
                    old: movement.old.clone(),
                    new: movement.new.clone(),
                })
                .collect(),
            head: plan.head.as_ref().map(|head| OpHead {
                from: head.from.clone(),
                to: head.to.clone(),
            }),
            worktree,
            index: plan
                .index
                .iter()
                .map(|change| OpIndex {
                    path: change.path.clone(),
                    from: change.from.clone(),
                    to: change.to.clone(),
                })
                .collect(),
            index_before_tree: None,
            undo_of: plan.undo_of.clone(),
        };
        Self { data, files, after }
    }
}

const GENESIS: &str = "aisc oplog";

/// `refs/aisc/oplog/main` for the main worktree, `refs/aisc/oplog/wt-<id>` for linked ones.
pub(crate) fn log_ref(git: &Git) -> Result<String> {
    let common = std::fs::canonicalize(git.common_dir()?)?;
    if std::fs::canonicalize(&git.dir)? == common {
        Ok("refs/aisc/oplog/main".to_owned())
    } else {
        let id = git
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .context("worktree directory has no name")?;
        Ok(format!("refs/aisc/oplog/wt-{id}"))
    }
}

pub(crate) fn short(oid: &str) -> String {
    oid.chars().take(4).collect()
}

pub(crate) fn tip(git: &Git) -> Result<Option<String>> {
    git.ref_oid(&log_ref(git)?)
}

/// Writes the op commit. Returns it with the previous op tip (the CAS value for the oplog ref).
pub(crate) fn build(git: &Git, record: &OpRecord) -> Result<(String, Option<String>)> {
    let previous = tip(git)?;
    let mut data = record.data.clone();
    let empty = git.empty_tree()?;
    let mut edits = Vec::new();
    for (index, bytes) in record.files.iter().enumerate() {
        let oid = git.write_blob(bytes, true)?;
        edits.push(TreeEdit {
            path: format!("wt/{index}"),
            entry: Some(("100644".to_owned(), oid)),
        });
    }
    for (file, bytes) in data.worktree.iter_mut().zip(&record.after) {
        file.after = match bytes {
            Some(bytes) => Some(git.write_blob(bytes, true)?),
            None => None,
        };
    }
    let json = serde_json::to_vec_pretty(&data)?;
    edits.push(TreeEdit {
        path: "op.json".to_owned(),
        entry: Some(("100644".to_owned(), git.write_blob(&json, true)?)),
    });
    let tree = git.tree_with(&empty, &edits)?;
    let mut parents: Vec<String> = Vec::new();
    let mut push = |oid: &Option<String>| {
        if let Some(oid) = oid
            && !parents.contains(oid)
        {
            parents.push(oid.clone());
        }
    };
    // The first parent always continues the oplog chain; the first op hangs off a genesis commit
    // so that a first-parent walk never leaks into branch history.
    let root = match &previous {
        Some(_) => None,
        None => Some(git.commit_tree(&empty, &[], format!("{GENESIS}\n").as_bytes(), None)?),
    };
    push(&previous.clone().or(root));
    for movement in &data.refs {
        if !movement.name.starts_with("refs/aisc/") {
            push(&movement.old);
            push(&movement.new);
        }
    }
    let message = format!("aisc {}: {}", data.verb, data.summary);
    let oid = git.commit_tree(&tree, &parents, format!("{message}\n").as_bytes(), None)?;
    Ok((oid, previous))
}

pub(crate) struct OpSummary {
    pub oid: String,
    pub time: u64,
    pub verb: String,
    pub summary: String,
}

pub(crate) fn list(git: &Git, limit: usize) -> Result<Vec<OpSummary>> {
    let reference = log_ref(git)?;
    if git.ref_oid(&reference)?.is_none() {
        return Ok(Vec::new());
    }
    let limit_argument = format!("-n{}", limit + 1); // one extra for the genesis commit
    let output = git.run(&[
        "log",
        "--first-parent",
        "--format=%H%x1f%ct%x1f%s",
        &limit_argument,
        &reference,
    ])?;
    let mut operations = Vec::new();
    for line in String::from_utf8_lossy(&output).lines() {
        let mut fields = line.split('\u{1f}');
        let oid = fields.next().unwrap_or_default().to_owned();
        let time = fields.next().unwrap_or("0").parse().unwrap_or(0);
        let subject = fields.next().unwrap_or_default();
        if subject == GENESIS {
            continue;
        }
        let subject = subject.strip_prefix("aisc ").unwrap_or(subject);
        let (verb, summary) = subject.split_once(": ").unwrap_or((subject, ""));
        operations.push(OpSummary {
            oid,
            time,
            verb: verb.to_owned(),
            summary: summary.to_owned(),
        });
    }
    operations.truncate(limit);
    Ok(operations)
}

/// Loads an op by id prefix (4+ hex characters); `None` means the latest.
pub(crate) fn load(git: &Git, spec: Option<&str>) -> Result<(String, OpData)> {
    let oid = match spec {
        None => tip(git)?.ok_or_else(|| {
            fail_hint(
                Code::State,
                "there are no recorded operations",
                "nothing to undo",
            )
        })?,
        Some(spec) => {
            let all = list(git, 1_000)?;
            let lower = spec.to_ascii_lowercase();
            let matches: Vec<&OpSummary> = all
                .iter()
                .filter(|op| spec.len() >= 4 && op.oid.starts_with(&lower))
                .collect();
            match matches.as_slice() {
                [only] => only.oid.clone(),
                [] => {
                    return Err(fail_hint(
                        Code::Selector,
                        format!("unknown operation '{spec}'"),
                        "run aisc oplog for ids",
                    ));
                }
                _ => {
                    return Err(fail(
                        Code::Selector,
                        format!("operation id '{spec}' is ambiguous; use more characters"),
                    ));
                }
            }
        }
    };
    let blob = git.run(&["cat-file", "blob", &format!("{oid}:op.json")])?;
    let data = serde_json::from_slice(&blob).context("unreadable operation record")?;
    Ok((oid, data))
}

pub(crate) fn age(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

pub(crate) fn run(limit: usize) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let operations = list(&git, limit)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut text = String::new();
    let mut items = Vec::new();
    for op in &operations {
        let line = format!(
            "{} {} {} {}",
            short(&op.oid),
            age(now.saturating_sub(op.time)),
            op.verb,
            escape_line(&op.summary)
        );
        text.push_str(line.trim_end());
        text.push('\n');
        items.push(json!({"op": short(&op.oid), "oid": op.oid, "time": op.time, "verb": op.verb, "summary": op.summary}));
    }
    if operations.is_empty() {
        text.push_str("no operations recorded\n");
    }
    Ok(Outcome::with_text(
        json!({"operations": items}),
        text.trim_end().to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_are_compact() {
        assert_eq!(age(5), "5s");
        assert_eq!(age(125), "2m");
        assert_eq!(age(7_300), "2h");
        assert_eq!(age(200_000), "2d");
    }
}
