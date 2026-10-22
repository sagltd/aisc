//! `aisc gc`: reports registered worktrees and removes the clean, finished-or-merged ones.
use super::{
    Entry, Ignored, Listed, OPERATION_MARKERS, Registration, Size, admin_dirs, has_aisc_marker,
    is_symlink, list, measure, now, registrations, remove, rescue, same_path, session_at,
    unregister, unsafe_path,
};
use crate::cli;
use crate::git::{Lock, Repo, run_git, text};
use crate::history::Outcome;
use crate::history::plumbing::Git;
use crate::task;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Remove,
    Forget,
    Keep,
    Removed,
    Failed,
}

impl Action {
    fn name(self, applied: bool) -> &'static str {
        match self {
            Self::Remove | Self::Forget if applied => "removed",
            Self::Remove | Self::Forget => "remove",
            Self::Keep => "keep",
            Self::Removed => "removed",
            Self::Failed => "failed",
        }
    }
}

struct Row {
    registration: Registration,
    admin: Option<PathBuf>,
    state: &'static str,
    head: String,
    merged: bool,
    finished: bool,
    locked: bool,
    age: u64,
    size: Size,
    ignored: Ignored,
    rescue_ref: Option<String>,
    action: Action,
    reason: String,
}

struct Scan<'a> {
    main: &'a Path,
    common: &'a Path,
    current: &'a Path,
    listed: &'a [Listed],
    admins: &'a HashMap<String, PathBuf>,
    min_age: u64,
    delete_ignored: bool,
}

impl Scan<'_> {
    fn inspect(&self, entry: Entry) -> Row {
        let mut row = Row {
            admin: None,
            registration: entry.registration,
            state: "unverified",
            head: String::new(),
            merged: false,
            finished: false,
            locked: false,
            age: 0,
            size: Size::default(),
            ignored: Ignored::default(),
            rescue_ref: None,
            action: Action::Keep,
            reason: String::new(),
        };
        let invalid = entry.invalid.or_else(|| {
            unsafe_path(&row.registration.path, self.main, self.common).map(str::to_owned)
        });
        if let Some(reason) = invalid {
            row.state = "invalid";
            // Only safe paths get "delete by hand" advice; for these, forgetting is the fix.
            row.reason = format!(
                "{reason}; invalid registry entry; remove {} to forget it",
                entry.file.display()
            );
            return row;
        }
        let path = PathBuf::from(&row.registration.path);
        if is_symlink(&path) {
            row.reason = "registered path is a symlink; inspect it manually".into();
            return row;
        }
        row.admin = self.admins.get(&row.registration.path).cloned();
        let exists = fs::symlink_metadata(&path).is_ok();
        let Some(listed) = self
            .listed
            .iter()
            .find(|entry| same_path(&entry.path, &path))
        else {
            if exists && !has_aisc_marker(&row.registration, self.common) {
                row.state = "unknown";
                row.reason = format!(
                    "not a Git worktree and no AISC marker; never sized or removed; remove {} to forget it",
                    entry.file.display()
                );
            } else if exists {
                // Git drops its metadata even when deleting the files fails partway.
                row.state = "leftover";
                row.size = measure(&path);
                row.reason = format!(
                    "Git no longer tracks this directory (interrupted removal?); branch {} keeps the commits; delete the directory by hand after checking it",
                    row.registration.branch
                );
            } else {
                row.state = "stale";
                row.action = Action::Forget;
                row.reason = "Git already pruned it; forget the registry entry".into();
            }
            return row;
        };
        let session = row.admin.as_deref().and_then(session_at);
        let Some(session) = session
            .filter(|session| session.name == row.registration.name && session.mode == "worktree")
        else {
            row.reason = "no matching AISC session marker in the Git admin dir".into();
            return row;
        };
        let admin = row.admin.clone().unwrap_or_default();
        row.locked = listed.locked;
        row.head = listed.head.clone().unwrap_or_default();
        row.age = now().saturating_sub(last_activity(&row.registration, &admin));
        let young = row.age < self.min_age.saturating_mul(3_600);
        if !exists {
            row.state = "missing";
            row.reason = if listed.locked {
                "locked (`git worktree lock`)".into()
            } else if young {
                format!(
                    "directory missing but active {} ago (< min-age {}h)",
                    age(row.age),
                    self.min_age
                )
            } else {
                row.action = Action::Remove;
                "directory is gone; prune its metadata (an unreferenced HEAD is saved first)".into()
            };
            return row;
        }
        row.size = measure(&path);
        let status = run_git(
            &path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            None,
        );
        row.state = match &status {
            Ok(output) if output.is_empty() => "clean",
            Ok(_) => "dirty",
            Err(_) => "unreadable",
        };
        let ignored = Ignored::scan(&path);
        let counts = ignored.as_ref().map_or((0, 0, false), |ignored| {
            (ignored.files, ignored.bytes, ignored.truncated)
        });
        let evidence = task::read_json::<Value>(&admin.join("aisc-evidence.json")).ok();
        row.finished = session.finished
            && evidence.is_some_and(|evidence| evidence["head"] == row.head.as_str());
        row.merged = self.merged(&row.registration, &row.head);
        row.reason = if listed.locked {
            "locked (`git worktree lock`)".into()
        } else if same_path(&row.registration.path, self.current) {
            "current worktree; run gc from another checkout".into()
        } else if crate::history::rewrite::operation_lock_held(&Git {
            root: path.clone(),
            dir: admin.clone(),
        }) {
            "busy: an aisc operation holds the worktree lock".into()
        } else if let Err(error) = status {
            format!("status failed: {error:#}")
        } else if row.state == "dirty" {
            "uncommitted or untracked changes".into()
        } else if OPERATION_MARKERS
            .iter()
            .any(|marker| admin.join(marker).exists())
        {
            "a Git merge/rebase/sequencer is in progress".into()
        } else if !(row.merged || row.finished && listed.branch.is_some()) {
            "commits are neither finished nor merged".into()
        } else if young {
            format!("active {} ago (< min-age {}h)", age(row.age), self.min_age)
        } else {
            match ignored {
                Err(error) => format!("could not list ignored files: {error:#}"),
                Ok(ignored) => {
                    let refusal = ignored.refusal(self.delete_ignored);
                    row.ignored = ignored;
                    refusal.unwrap_or_else(|| {
                        row.action = Action::Remove;
                        deleted_ignored(&row.ignored)
                    })
                }
            }
        };
        (row.ignored.files, row.ignored.bytes, row.ignored.truncated) = counts;
        row
    }

    /// Tip has no commits beyond the recorded base or is contained in the base branch.
    fn merged(&self, registration: &Registration, head: &str) -> bool {
        if head.is_empty() {
            return false;
        }
        if head == registration.base {
            return true;
        }
        let base = registration
            .base_branch
            .as_ref()
            .and_then(|branch| {
                let spec = format!("refs/heads/{branch}^{{commit}}");
                run_git(
                    self.main,
                    &["rev-parse", "--verify", "--quiet", &spec],
                    None,
                )
                .ok()
            })
            .and_then(|output| text(&output).ok().map(|tip| tip.trim().to_owned()))
            .unwrap_or_else(|| registration.base.clone());
        run_git(
            self.main,
            &["merge-base", "--is-ancestor", head, &base],
            None,
        )
        .is_ok()
    }
}

/// What removing a worktree costs beyond its tracked files.
fn deleted_ignored(ignored: &Ignored) -> String {
    match ignored.files {
        0 => String::new(),
        1 => format!("deletes 1 ignored file ({})", bytes(ignored.bytes, false)),
        count => format!(
            "deletes {count} ignored files ({})",
            bytes(ignored.bytes, false)
        ),
    }
}

/// Newest of creation and the admin dir's HEAD, index and reflog mtimes.
fn last_activity(registration: &Registration, admin: &Path) -> u64 {
    ["HEAD", "index", "logs/HEAD"]
        .iter()
        .filter_map(|name| fs::metadata(admin.join(name)).ok()?.modified().ok())
        .filter_map(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .fold(registration.created_at, u64::max)
}

pub(crate) fn gc(args: &cli::Gc) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let common = repo.common_dir()?;
    let _guard = if args.apply {
        fs::create_dir_all(common.join("aisc"))?;
        Some(Lock::acquire(common.join("aisc").join("gc.lock"))?)
    } else {
        None
    };
    let listed = list(&repo.root)?;
    let main = PathBuf::from(&listed.first().context("Git listed no worktrees")?.path);
    let admins = admin_dirs(&common);
    let scan = Scan {
        main: &main,
        common: &common,
        current: &repo.root,
        listed: &listed,
        admins: &admins,
        min_age: args.min_age,
        delete_ignored: args.delete_ignored,
    };
    let mut rows = registrations(&common)
        .into_iter()
        .map(|entry| scan.inspect(entry))
        .collect::<Vec<_>>();
    let verified = rows
        .iter()
        .filter(|row| !matches!(row.state, "unverified" | "stale" | "leftover" | "invalid"))
        .map(|row| row.registration.path.clone())
        .collect::<HashSet<_>>();
    let foreign = listed
        .iter()
        .skip(1)
        .filter(|entry| !verified.contains(&entry.path))
        .collect::<Vec<_>>();
    let mut pruned = false;
    if args.apply {
        for row in &mut rows {
            apply(row, &main, &common);
        }
        // `git worktree prune` would also drop metadata of missing foreign worktrees.
        let removed_any = rows.iter().any(|row| row.action == Action::Removed);
        if removed_any && !foreign.iter().any(|entry| entry.prunable) {
            pruned = run_git(&main, &["worktree", "prune"], None).is_ok();
        }
    }
    let failed = rows.iter().any(|row| row.action == Action::Failed);
    let value = report(&rows, foreign.len(), args.apply, pruned);
    let text = render(&rows, foreign.len(), args.apply, &std::env::current_dir()?);
    let mut outcome = Outcome::with_text(value, text);
    outcome.exit = i32::from(failed);
    Ok(outcome)
}

fn apply(row: &mut Row, main: &Path, common: &Path) {
    let result = match row.action {
        Action::Forget => unregister(common, &row.registration.name),
        Action::Remove => {
            let admin = row.admin.clone().unwrap_or_default();
            // Holding the worktree's operation lock (the same flock-tagged lock its own aisc
            // commands take) keeps aisc writes out while Git removes it.
            let worktree = Git {
                root: PathBuf::from(&row.registration.path),
                dir: admin,
            };
            let _lock = match crate::history::rewrite::task_lock(&worktree) {
                Ok(lock) => lock,
                Err(error) => {
                    row.action = Action::Keep;
                    row.reason = format!("busy: {error:#}");
                    return;
                }
            };
            match rescue(main, &row.registration.name, &row.head) {
                Ok(reference) => {
                    row.rescue_ref = reference;
                    remove(main, common, &row.registration)
                }
                Err(error) => Err(error),
            }
        }
        _ => return,
    };
    match result {
        Ok(()) => row.action = Action::Removed,
        Err(error) => {
            row.action = Action::Failed;
            row.reason = format!("{error:#}");
        }
    }
}

fn report(rows: &[Row], foreign: usize, applied: bool, pruned: bool) -> Value {
    let removable = |row: &&Row| matches!(row.action, Action::Remove | Action::Forget);
    let removed = |row: &&Row| row.action == Action::Removed;
    let bytes = |predicate: &dyn Fn(&&Row) -> bool| {
        rows.iter()
            .filter(predicate)
            .map(|row| row.size.allocated)
            .sum::<u64>()
    };
    let worktrees = rows
        .iter()
        .map(|row| {
            json!({
                "name": row.registration.name,
                "path": row.registration.path,
                "branch": row.registration.branch,
                "sparse": row.registration.sparse,
                "state": row.state,
                "merged": row.merged,
                "finished": row.finished,
                "locked": row.locked,
                "age_seconds": row.age,
                "bytes": row.size.apparent,
                "allocated_bytes": row.size.allocated,
                "size_partial": row.size.partial,
                "action": row.action.name(false),
                "reason": row.reason,
                "ignored_files": row.ignored.files,
                "ignored_bytes": row.ignored.bytes,
                "ignored_scan_truncated": row.ignored.truncated,
                "rescue_ref": row.rescue_ref,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "applied": applied,
        "worktrees": worktrees,
        "total_bytes": bytes(&|_| true),
        "reclaimable": rows.iter().filter(removable).count(),
        "reclaimable_bytes": bytes(&removable),
        "removed": rows.iter().filter(removed).count(),
        "freed_bytes": bytes(&removed),
        "failed": rows.iter().filter(|row| row.action == Action::Failed).count(),
        "foreign_ignored": foreign,
        "pruned": pruned,
    })
}

fn render(rows: &[Row], foreign: usize, applied: bool, cwd: &Path) -> String {
    let total = rows.iter().map(|row| row.size.allocated).sum::<u64>();
    let mut out = format!("aisc worktrees: {} ({})", rows.len(), bytes(total, false));
    let selected = |action: Action| {
        let rows = rows
            .iter()
            .filter(|row| row.action.name(false) == action.name(false));
        (
            rows.clone().count(),
            rows.map(|row| row.size.allocated).sum::<u64>(),
        )
    };
    if applied {
        let (count, freed) = selected(Action::Removed);
        let _ = write!(out, "; removed {count} ({} freed)", bytes(freed, false));
        let (failed, _) = selected(Action::Failed);
        if failed > 0 {
            let _ = write!(out, "; {failed} failed");
        }
    } else {
        let (count, reclaimable) = selected(Action::Remove);
        let _ = write!(
            out,
            "; reclaimable {count} ({}) (dry run; pass --apply)",
            bytes(reclaimable, false)
        );
    }
    let cells = rows
        .iter()
        .map(|row| {
            let mut state = row.state.to_owned();
            for (flag, label) in [(row.merged, " merged"), (row.finished, " finished")] {
                if flag && row.state == "clean" {
                    state.push_str(label);
                }
            }
            let mut action = row.action.name(applied).to_owned();
            if !row.reason.is_empty() && row.action != Action::Removed {
                let _ = write!(action, ": {}", row.reason);
            }
            if let Some(reference) = &row.rescue_ref {
                let _ = write!(action, " (HEAD saved as {reference})");
            }
            [
                row.registration.name.clone(),
                relative(cwd, Path::new(&row.registration.path)),
                bytes(row.size.allocated, row.size.partial),
                age(row.age),
                state,
                action,
            ]
        })
        .collect::<Vec<_>>();
    let widths = (0..5)
        .map(|column| {
            cells
                .iter()
                .map(|cell| cell[column].len())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    for cell in &cells {
        out.push('\n');
        for (column, width) in widths.iter().enumerate() {
            let _ = write!(out, "{:width$}  ", cell[column]);
        }
        out.push_str(&cell[5]);
    }
    if foreign > 0 {
        let plural = if foreign == 1 { "" } else { "s" };
        let _ = write!(out, "\n{foreign} foreign worktree{plural} ignored");
    }
    out
}

fn bytes(count: u64, partial: bool) -> String {
    let prefix = if partial { ">=" } else { "" };
    let mut value = count as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if value < 1_024.0 {
            break;
        }
        value /= 1_024.0;
        unit = next;
    }
    if unit == "B" || value >= 10.0 {
        format!("{prefix}{value:.0} {unit}")
    } else {
        format!("{prefix}{value:.1} {unit}")
    }
}

fn age(seconds: u64) -> String {
    match seconds {
        0..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `to` relative to `from` (both absolute), e.g. `../fix-a`.
fn relative(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<Component>>();
    let to = to.components().collect::<Vec<Component>>();
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec!["..".to_owned(); from.len() - shared];
    parts.extend(
        to[shared..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_walk_up_to_the_shared_ancestor() {
        assert_eq!(
            relative(Path::new("/a/repo"), Path::new("/a/fix")),
            "../fix"
        );
        assert_eq!(relative(Path::new("/a/repo"), Path::new("/a/repo/w")), "w");
        assert_eq!(relative(Path::new("/a/repo"), Path::new("/a/repo")), ".");
    }

    #[test]
    fn sizes_and_ages_are_compact() {
        assert_eq!(bytes(512, false), "512 B");
        assert_eq!(bytes(1_288_490_189, false), "1.2 GiB");
        assert_eq!(bytes(812 * 1_048_576, true), ">=812 MiB");
        assert_eq!(age(7_200), "2h");
        assert_eq!(age(3 * 86_400), "3d");
        assert_eq!(age(59), "0m");
    }
}
