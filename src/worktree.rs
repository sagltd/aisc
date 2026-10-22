//! Worktrees created by `aisc start --path`: the registry under the Git common dir, removal after
//! `finish`, and `aisc gc`. Only registered worktrees whose Git admin dir carries a matching AISC
//! session marker are ever removed, always with `git worktree remove` (never forced), and the
//! task branch is kept so no commit is lost.
use crate::git::{Repo, run_git, text};
use crate::task::{self, Session};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod gc;
pub(crate) use gc::gc;

/// Wall-clock budget for one size walk; larger trees are reported as a lower bound.
const SIZE_BUDGET: Duration = Duration::from_secs(10);
const OPERATION_MARKERS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Registration {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    pub base_branch: Option<String>,
    pub created_at: u64,
    pub aisc_version: String,
    pub command: Vec<String>,
    pub sparse: Option<Vec<String>>,
}

pub(crate) fn registry_path(common: &Path, name: &str) -> PathBuf {
    common
        .join("aisc")
        .join("worktrees")
        .join(format!("{name}.json"))
}

pub(crate) fn register(common: &Path, registration: &Registration) -> Result<()> {
    let path = registry_path(common, &registration.name);
    fs::create_dir_all(path.parent().context("invalid registry path")?)?;
    task::write_json(&path, registration)
}

pub(crate) fn unregister(common: &Path, name: &str) -> Result<()> {
    match fs::remove_file(registry_path(common, name)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Readable registry entries; unreadable files are never acted on.
fn registrations(common: &Path) -> Vec<Registration> {
    let Ok(entries) = fs::read_dir(common.join("aisc").join("worktrees")) else {
        return Vec::new();
    };
    let mut registrations = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| task::read_json::<Registration>(&path).ok())
        .collect::<Vec<_>>();
    registrations.sort_by(|left, right| left.name.cmp(&right.name));
    registrations
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// One record of `git worktree list --porcelain -z`.
struct Listed {
    path: String,
    head: Option<String>,
    branch: Option<String>,
    locked: bool,
    prunable: bool,
}

fn list(root: &Path) -> Result<Vec<Listed>> {
    let output = run_git(root, &["worktree", "list", "--porcelain", "-z"], None)?;
    let mut records = Vec::new();
    let mut current: Option<Listed> = None;
    for field in output.split(|byte| *byte == 0) {
        let field = text(field)?;
        if field.is_empty() {
            records.extend(current.take());
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            records.extend(current.take());
            current = Some(Listed {
                path: value.to_owned(),
                head: None,
                branch: None,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(record) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => record.head = Some(value.to_owned()),
            "branch" => {
                record.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                )
            }
            "locked" => record.locked = true,
            "prunable" => record.prunable = true,
            _ => {}
        }
    }
    records.extend(current);
    Ok(records)
}

/// Linked worktree path → its admin dir (`<common>/worktrees/<id>`), from each `gitdir` file.
fn admin_dirs(common: &Path) -> HashMap<String, PathBuf> {
    let Ok(entries) = fs::read_dir(common.join("worktrees")) else {
        return HashMap::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|admin| {
            let gitdir = fs::read_to_string(admin.join("gitdir")).ok()?;
            let path = gitdir
                .trim_end_matches('\n')
                .strip_suffix("/.git")?
                .to_owned();
            Some((path, admin))
        })
        .collect()
}

fn same_path(left: &str, right: &Path) -> bool {
    Path::new(left) == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn session_at(admin: &Path) -> Option<Session> {
    task::read_json::<Session>(&admin.join("aisc-session.json")).ok()
}

#[derive(Clone, Copy, Default)]
struct Size {
    apparent: u64,
    allocated: u64,
    partial: bool,
}

/// Bytes under `root` without following symlinks: apparent length and allocated blocks.
fn measure(root: &Path) -> Size {
    use std::os::unix::fs::MetadataExt;
    let started = Instant::now();
    let mut size = Size::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if started.elapsed() > SIZE_BUDGET {
            size.partial = true;
            break;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            size.partial |= path != root;
            continue;
        };
        size.apparent += metadata.len();
        size.allocated += metadata.blocks() * 512;
        if !metadata.is_dir() {
            continue;
        }
        match fs::read_dir(&path) {
            Ok(entries) => pending.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path()))),
            Err(_) => size.partial = true,
        }
    }
    size
}

/// Removes a verified AISC worktree (Git refuses dirty or locked ones), then its registry entry.
fn remove(main: &Path, common: &Path, registration: &Registration) -> Result<()> {
    run_git(
        main,
        &["worktree", "remove", "--", &registration.path],
        None,
    )?;
    unregister(common, &registration.name).context(
        "worktree removed but its registry entry could not be deleted; `aisc gc --apply` forgets it",
    )
}

fn current_directory_inside(root: &Path) -> bool {
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    std::env::current_dir()
        .ok()
        .and_then(|cwd| fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd.starts_with(&canonical))
}

/// After a successful `finish` in a worktree session: removes the worktree unless it is not
/// AISC's, locked, or Git refuses. Never fails the finish; problems become `kept_reason` or
/// `warning` fields.
pub(crate) fn remove_after_finish(repo: &Repo, name: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("removed_worktree".into(), Value::Null);
    match try_remove_after_finish(repo, name, &mut fields) {
        Ok(Some(reason)) => {
            fields.insert("kept_reason".into(), json!(reason));
        }
        Ok(None) => {}
        Err(error) => {
            fields.insert(
                "warning".into(),
                json!(format!(
                    "finished, but the worktree was not removed: {error:#}; it is kept and `aisc gc` lists it"
                )),
            );
        }
    }
    fields
}

fn try_remove_after_finish(
    repo: &Repo,
    name: &str,
    fields: &mut Map<String, Value>,
) -> Result<Option<String>> {
    let common = repo.common_dir()?;
    let Ok(registration) = task::read_json::<Registration>(&registry_path(&common, name)) else {
        return Ok(Some(
            "worktree not registered by `aisc start --path`; kept".into(),
        ));
    };
    if !same_path(&registration.path, &repo.root) {
        return Ok(Some(
            "registry entry points at a different path; kept".into(),
        ));
    }
    let listed = list(&repo.root)?;
    let main = PathBuf::from(&listed.first().context("Git listed no worktrees")?.path);
    fields.insert("main_checkout".into(), json!(main));
    if listed
        .iter()
        .any(|entry| entry.locked && same_path(&entry.path, &repo.root))
    {
        return Ok(Some(
            "worktree is locked (`git worktree lock`); kept".into(),
        ));
    }
    let size = measure(&repo.root);
    let inside = current_directory_inside(&repo.root);
    if let Err(error) = remove(&main, &common, &registration) {
        let still_listed = list(&main)
            .map(|listed| {
                listed
                    .iter()
                    .any(|entry| same_path(&entry.path, &repo.root))
            })
            .unwrap_or(true);
        if still_listed {
            return Err(error);
        }
        return Err(error.context(format!(
            "Git dropped the worktree metadata but files remain in {}; branch {} keeps the commits",
            registration.path, registration.branch
        )));
    }
    fields.insert("removed_worktree".into(), json!(registration.path));
    fields.insert("freed_bytes".into(), json!(size.allocated));
    if inside {
        fields.insert("cd".into(), json!(main));
    }
    Ok(None)
}
