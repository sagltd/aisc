//! The state printed after a mutation, built from what the verb already knows.
//!
//! Default text output is short: the result line, the branch header (one `git for-each-ref`
//! reads tips, ahead/behind and the upstream after the reference transaction), the commits this
//! operation created, and the uncommitted paths. Those paths are derived from the change set the
//! verb read before it changed anything plus the plan it applied; any path whose state is not
//! certain from that sends the whole read back to Git. JSON output and `--stack` keep the full
//! view, read exactly as `aisc log` reads it.
use super::model::{ChangeSet, FileChange, HunkInfo, Kind};
use super::plumbing::{Entry, Git, created};
use super::render;
use super::rewrite::Plan;
use super::view::{self, BaseInfo, UpstreamInfo, View};
use crate::output::escape_line;
use crate::store;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static FULL: AtomicBool = AtomicBool::new(true);
static STACK: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

/// Called once by `main`: `json` output needs the complete view; `stack` (`--stack`/`-v`) asks
/// for the commit list in text output; `quiet` (`-q`, text) prints the result line only, so no
/// view is read at all.
pub(crate) fn configure(json: bool, stack: bool, quiet: bool) {
    FULL.store(json || stack, Ordering::SeqCst);
    STACK.store(stack, Ordering::SeqCst);
    QUIET.store(quiet && !json, Ordering::SeqCst);
}

/// Whether the output needs the complete view after a mutation (JSON or `--stack`).
pub(crate) fn full() -> bool {
    FULL.load(Ordering::SeqCst)
}

/// The change set this process last read, and the HEAD it was read against.
struct Snapshot {
    head: String,
    changes: ChangeSet,
}

fn snapshots() -> &'static Mutex<HashMap<PathBuf, Snapshot>> {
    static SNAPSHOTS: OnceLock<Mutex<HashMap<PathBuf, Snapshot>>> = OnceLock::new();
    SNAPSHOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remembers the uncommitted changes read against `head` (status and diff of the same moment).
pub(crate) fn note_changes(git: &Git, head: &str, changes: &ChangeSet) {
    snapshots().lock().expect("snapshots").insert(
        git.dir.clone(),
        Snapshot {
            head: head.to_owned(),
            changes: changes.clone(),
        },
    );
}

fn applied() -> &'static Mutex<HashMap<PathBuf, Plan>> {
    static APPLIED: OnceLock<Mutex<HashMap<PathBuf, Plan>>> = OnceLock::new();
    APPLIED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Called by `Session::apply` once a plan is fully applied. `preserved` (a concurrently changed
/// file was saved aside) means the worktree holds a file no plan describes: read it from Git.
pub(crate) fn note_applied(git: &Git, plan: &Plan, preserved: bool) {
    let mut applied = applied().lock().expect("applied plans");
    if preserved {
        applied.remove(&git.dir);
    } else {
        applied.insert(git.dir.clone(), plan.clone());
    }
}

/// The view to print after a mutation; `branch`/`with_changes` are as for `view::read`.
pub(crate) fn after(git: &Git, branch: Option<&str>, with_changes: bool) -> Result<View> {
    if QUIET.load(Ordering::SeqCst) {
        // Only the result line is printed; the JSON this would feed is not.
        return Ok(View {
            branch: branch.map(str::to_owned),
            tip: String::new(),
            is_head: branch.is_none(),
            base: None,
            upstream: None,
            commits: Vec::new(),
            total: 0,
            status: None,
            changes: None,
            sync_paused: None,
            locals: Vec::new(),
            created: Default::default(),
            moves: Vec::new(),
        });
    }
    if FULL.load(Ordering::SeqCst) {
        return view::read(git, branch, 20, with_changes);
    }
    let plan = applied()
        .lock()
        .expect("applied plans")
        .get(&git.dir)
        .cloned();
    let mut view = match light(git, branch, with_changes, plan.as_ref()) {
        Ok(Some(view)) => view,
        _ => view::read(git, branch, 20, with_changes)?,
    };
    view.moves = plan
        .as_ref()
        .map(|plan| moves(git, plan))
        .unwrap_or_default();
    Ok(view)
}

/// A branch tip an operation moved, or the branch HEAD switched to.
#[derive(Clone, Debug)]
pub(crate) struct Move {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub subject: Option<String>,
    /// HEAD now names this branch; its tip did not move.
    pub switched: bool,
}

/// Every branch tip the plan moved, and the branch HEAD now names when only HEAD moved.
fn moves(git: &Git, plan: &Plan) -> Vec<Move> {
    let subject = |oid: &Option<String>| -> Option<String> {
        let oid = oid.as_ref()?;
        git.fast(|store| store.commit(oid))
            .and_then(|commit| commit.subject())
    };
    let mut moved: Vec<Move> = plan
        .refs
        .iter()
        .filter(|movement| movement.name.starts_with("refs/heads/") || movement.name == "HEAD")
        .map(|movement| Move {
            name: movement.name.trim_start_matches("refs/heads/").to_owned(),
            old: movement.old.clone(),
            new: movement.new.clone(),
            subject: subject(&movement.new),
            switched: false,
        })
        .collect();
    if let Some(head) = &plan.head
        && let Some(name) = head.to.strip_prefix("refs/heads/")
        && !moved.iter().any(|moved| moved.name == name)
        && let Ok(Some(tip)) = git.ref_oid(&head.to)
    {
        let new = Some(tip);
        moved.push(Move {
            name: name.to_owned(),
            old: None,
            subject: subject(&new),
            new,
            switched: true,
        });
    }
    moved
}

/// The tips the plan this process last applied moved, one `<branch> <id7> <subject>` line each
/// (`<branch> deleted` for a removed branch). `None` when no plan describes the worktree (a
/// concurrently changed file was saved aside): print the full summary instead.
pub(crate) fn restored_tips(git: &Git) -> Option<Vec<String>> {
    let plan = applied()
        .lock()
        .expect("applied plans")
        .get(&git.dir)
        .cloned()?;
    let seven = |oid: &str| oid.chars().take(7).collect::<String>();
    Some(
        moves(git, &plan)
            .iter()
            .map(|moved| {
                let name = escape_line(&moved.name);
                match (&moved.new, &moved.subject) {
                    (Some(new), Some(subject)) => {
                        format!("{name} {} {}", seven(new), escape_line(subject))
                    }
                    (Some(new), None) => format!("{name} {}", seven(new)),
                    (None, _) => format!("{name} deleted"),
                }
            })
            .collect(),
    )
}

/// The text printed after a mutation: `result`, then the short summary (or the full compact log
/// with `--stack`).
pub(crate) fn text(after: &View, result: &str) -> String {
    if STACK.load(Ordering::SeqCst) {
        return format!("{result}\n{}", render::compact_text(after));
    }
    let mut out = String::from(result);
    // Each moved tip with full-enough ids to chain the next command.
    let seven = |oid: &str| oid.chars().take(7).collect::<String>();
    let mut tips = Vec::new();
    for moved in &after.moves {
        let name = escape_line(&moved.name);
        if result.contains(&format!("moved branch {}:", moved.name)) {
            continue;
        }
        let line = match &moved.new {
            Some(new) => {
                tips.push(new.clone());
                let tip = match (&moved.old, moved.switched) {
                    (_, true) => seven(new),
                    (Some(old), false) => format!("{}→{}", seven(old), seven(new)),
                    (None, false) => format!("new→{}", seven(new)),
                };
                match &moved.subject {
                    Some(subject) => format!("{name} {tip} {}", escape_line(subject)),
                    None => format!("{name} {tip}"),
                }
            }
            None => format!(
                "{name} {}→deleted",
                moved.old.as_deref().map(seven).unwrap_or_default()
            ),
        };
        let _ = write!(out, "\n{line}");
    }
    let _ = write!(out, "\n{}", render::header_line(after));
    for commit in &after.commits {
        // Only commits this operation wrote, unless a line above already names them.
        if after.created.contains(&commit.oid)
            && !result.contains(&commit.short)
            && !tips.contains(&commit.oid)
        {
            let _ = write!(out, "\n{} {}", commit.short, escape_line(&commit.subject));
        }
    }
    let rest = render::changes_and_state(after);
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(&rest);
    }
    out
}

fn light(
    git: &Git,
    branch: Option<&str>,
    with_changes: bool,
    plan: Option<&Plan>,
) -> Result<Option<View>> {
    let refs = view::read_refs(git)?;
    let head = git.head_state()?;
    let head_branch = head
        .symbolic
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    let (target, tip, is_head) = match branch {
        Some(name) if Some(name) != head_branch.as_deref() => {
            let Some(found) = refs.locals.iter().find(|candidate| candidate.name == name) else {
                return Ok(None);
            };
            (Some(name.to_owned()), found.oid.clone(), false)
        }
        _ => (head_branch.clone(), head.oid.clone(), true),
    };
    let base = view::find_base(&refs).filter(|(label, _, _)| target.as_deref() != Some(label));
    // Tips, upstream and ahead/behind as Git sees them now, in one process.
    let Some(name) = &target else {
        return Ok(None); // detached HEAD: let Git count
    };
    let reference = format!("refs/heads/{name}");
    let mut format =
        String::from("%(objectname)%00%(upstream:short)%00%(upstream:track,nobracket)");
    if let Some((_, refname, _)) = &base {
        let _ = write!(format, "%00%(ahead-behind:{refname})");
    }
    let format = format!("--format={format}");
    // The uncommitted paths: derived in process, else one `git status` run alongside.
    let wants_changes = is_head && with_changes;
    let derived = match plan {
        Some(plan) if wants_changes => derive_changes(git, &head.oid, plan).ok().flatten(),
        _ => None,
    };
    let (output, read) = std::thread::scope(|scope| {
        let refs = scope.spawn(|| git.run(&["for-each-ref", &format, &reference]));
        let read = (wants_changes && derived.is_none()).then(|| status_paths(git));
        (refs.join(), read)
    });
    let output = output.map_err(|_| anyhow::anyhow!("a Git reader panicked"))??;
    let line = String::from_utf8(output)?;
    let mut fields = line.trim_end_matches('\n').split('\0');
    let (Some(oid), Some(upstream), Some(track)) = (fields.next(), fields.next(), fields.next())
    else {
        return Ok(None);
    };
    if oid != tip {
        return Ok(None); // moved concurrently: read it all from Git
    }
    let base_info = match &base {
        Some((label, refname, base_oid)) => {
            let counts = fields.next().context("ahead-behind")?;
            let mut numbers = counts.split(' ').map(str::parse::<usize>);
            let (Some(Ok(ahead)), Some(Ok(behind))) = (numbers.next(), numbers.next()) else {
                return Ok(None);
            };
            Some(BaseInfo {
                label: label.clone(),
                refname: refname.clone(),
                oid: base_oid.clone(),
                ahead,
                behind,
            })
        }
        None => None,
    };
    let upstream = (is_head && !upstream.is_empty()).then(|| {
        let mut ahead = 0;
        let mut behind = 0;
        for part in track.split(", ") {
            if let Some(count) = part.strip_prefix("ahead ") {
                ahead = count.parse().unwrap_or(0);
            } else if let Some(count) = part.strip_prefix("behind ") {
                behind = count.parse().unwrap_or(0);
            }
        }
        UpstreamInfo {
            label: upstream.to_owned(),
            ahead,
            behind,
        }
    });
    // Commits: the newest 20 of the range, read in process (needed for the created ones).
    let commits = match view::walk(
        git,
        &tip,
        base_info.as_ref().map(|base| base.oid.as_str()),
        20,
    ) {
        Some((commits, _)) => commits,
        // The header has its counts; only the list of new commits is lost.
        None if base_info.is_some() => Vec::new(),
        None => return Ok(None),
    };
    let total = base_info.as_ref().map_or(commits.len(), |base| base.ahead);
    let mut status = None;
    let changes = if wants_changes {
        match derived {
            Some(changes) => Some(changes),
            None => match read.transpose()?.flatten() {
                Some(changes) => Some(changes),
                None => {
                    let (read, changes) = view::read_changes(git)?;
                    status = Some(read);
                    Some(changes)
                }
            },
        }
    } else {
        None
    };
    let sync_paused = std::fs::read(git.dir.join("aisc").join("sync.json"))
        .ok()
        .map(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| value["materialized"].as_array().map(Vec::len))
                .unwrap_or(0)
        });
    let created = created(&git.dir);
    Ok(Some(View {
        branch: target,
        tip,
        is_head,
        base: base_info,
        upstream,
        commits,
        total,
        status,
        changes,
        sync_paused,
        locals: refs.locals,
        created,
        moves: Vec::new(),
    }))
}

/// The uncommitted paths from one `git status` (no diff), when status alone settles every path:
/// a tracked path is listed by `git diff HEAD` exactly when its worktree differs from HEAD, and
/// status says so unless both the index and the worktree differ (then the worktree may equal
/// HEAD again). Unmerged paths and submodules also go to the full read.
fn status_paths(git: &Git) -> Result<Option<ChangeSet>> {
    let output = git.run(&[
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
        "--no-renames",
    ])?;
    let mut files = Vec::new();
    let mut nested = Vec::new();
    for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let Ok(record) = std::str::from_utf8(record) else {
            return Ok(None);
        };
        if let Some(path) = record.strip_prefix("? ") {
            if path.ends_with('/') {
                nested.push(path.to_owned());
            } else {
                files.push(untracked_stub(path));
            }
            continue;
        }
        let Some(rest) = record.strip_prefix("1 ") else {
            return Ok(None); // unmerged, renamed or unknown records
        };
        let fields: Vec<&str> = rest.splitn(8, ' ').collect();
        let [xy, submodule, _, _, _, _, _, path] = fields[..] else {
            return Ok(None);
        };
        let xy = xy.as_bytes();
        if submodule != "N..." || (xy[0] != b'.' && xy[1] != b'.') {
            return Ok(None);
        }
        files.push(modified_stub(path));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(ChangeSet { files, nested }))
}

/// The worktree side of a path for the "uncommitted" question.
enum Side {
    Absent,
    /// (mode, blob id of the bytes as stored, no filters)
    Present(String, String),
    Unknown,
}

fn worktree_side(root: &Path, path: &str) -> Side {
    let full = root.join(path);
    let Ok(metadata) = std::fs::symlink_metadata(&full) else {
        return Side::Absent;
    };
    if metadata.file_type().is_symlink() {
        let Ok(target) = std::fs::read_link(&full) else {
            return Side::Unknown;
        };
        return match store::hash_object(store::Kind::Blob, target.as_os_str().as_encoded_bytes()) {
            Ok(oid) => Side::Present("120000".to_owned(), oid),
            Err(_) => Side::Unknown,
        };
    }
    if !metadata.is_file() {
        return Side::Unknown;
    }
    use std::os::unix::fs::PermissionsExt;
    let mode = if metadata.permissions().mode() & 0o111 != 0 {
        "100755"
    } else {
        "100644"
    };
    match std::fs::read(&full)
        .ok()
        .and_then(|bytes| store::hash_object(store::Kind::Blob, &bytes).ok())
    {
        Some(oid) => Side::Present(mode.to_owned(), oid),
        None => Side::Unknown,
    }
}

/// Whether no ignore rule at all can apply to `path` (then an untracked file is listed).
fn no_ignore_rules(git: &Git, path: &str) -> Result<bool> {
    let has_rules = |file: &Path| -> bool {
        std::fs::read_to_string(file).is_ok_and(|text| {
            text.lines().any(|line| {
                let line = line.trim();
                !line.is_empty() && !line.starts_with('#')
            })
        }) || (file.exists() && std::fs::read_to_string(file).is_err())
    };
    let mut files = vec![git.root.join(".gitignore")];
    let mut directory = PathBuf::new();
    let components: Vec<&str> = path.split('/').collect();
    for component in &components[..components.len().saturating_sub(1)] {
        directory.push(component);
        files.push(git.root.join(&directory).join(".gitignore"));
    }
    let common = git.common_dir()?;
    files.push(common.join("info").join("exclude"));
    let config = crate::runner::config(&git.root)?;
    match config.string("core.excludesfile") {
        Some(_) => return Ok(false), // a configured file (with ~ expansion): let Git decide
        None => {
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty())
            {
                files.push(PathBuf::from(xdg).join("git").join("ignore"));
            } else if let Some(home) = std::env::var_os("HOME") {
                files.push(
                    PathBuf::from(home)
                        .join(".config")
                        .join("git")
                        .join("ignore"),
                );
            }
        }
    }
    Ok(!files.iter().any(|file| has_rules(file)))
}

fn untracked_stub(path: &str) -> FileChange {
    FileChange {
        path: path.to_owned(),
        kind: Kind::Untracked,
        old_mode: None,
        new_mode: None,
        old_oid: None,
        new_oid: None,
        blocks: Vec::new(),
        hunks: vec![HunkInfo {
            blocks: 0..0,
            id: String::new(),
            short: String::new(),
            line: 1,
            added: 0,
            removed: 0,
            hint: String::new(),
            ws: false,
        }],
        lines: 0,
        binary: false,
        digest: String::new(),
        ws: false,
    }
}

fn modified_stub(path: &str) -> FileChange {
    FileChange {
        kind: Kind::Modified,
        hunks: Vec::new(),
        ..untracked_stub(path)
    }
}

/// The uncommitted paths after `plan`, when every path's state is certain; `Ok(None)` otherwise.
///
/// A path the plan touched (its HEAD entry changed, or the plan wrote its file or index entry)
/// is uncommitted exactly when its worktree content differs from the new HEAD entry, or when it
/// is an untracked file no ignore rule can hide. The worktree content is known from Git's own
/// hash in the earlier diff (for a file the plan did not write), from the old HEAD entry (for a
/// file that was clean), or from the bytes on disk compared with the new entry. Anything else is
/// left to Git. Paths the plan did not touch keep their earlier state.
fn derive_changes(git: &Git, new_head: &str, plan: &Plan) -> Result<Option<ChangeSet>> {
    let Some((old_head, before)) = snapshots()
        .lock()
        .expect("snapshots")
        .get(&git.dir)
        .map(|snapshot| (snapshot.head.clone(), snapshot.changes.clone()))
    else {
        return Ok(None);
    };
    let Some(Some((old_tree, new_tree))) = git.fast(|store| {
        Ok(store
            .peel_to_tree(&old_head)?
            .zip(store.peel_to_tree(new_head)?))
    }) else {
        return Ok(None);
    };
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let Some(changed) = git.fast(|store| {
        let mut changed = Vec::new();
        store.diff_trees(Some(&old_tree), Some(&new_tree), "", &mut changed)?;
        Ok(changed)
    }) else {
        return Ok(None);
    };
    let mut old_entries: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    let mut new_entries: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    for (path, old, new) in changed {
        old_entries.insert(path.clone(), old);
        new_entries.insert(path.clone(), new);
        candidates.insert(path);
    }
    let written: BTreeMap<&str, &super::rewrite::WorktreeWrite> = plan
        .worktree
        .iter()
        .map(|write| (write.path.as_str(), write))
        .collect();
    candidates.extend(written.keys().map(|path| (*path).to_owned()));
    candidates.extend(plan.index.iter().map(|change| change.path.clone()));
    candidates.extend(plan.restore_ita.iter().cloned());
    candidates.extend(plan.remove_files.iter().cloned());
    if candidates.is_empty() {
        return Ok(Some(before));
    }
    let config = crate::runner::config(&git.root)?;
    let disabled = |key: &str| {
        config
            .string(key)
            .is_some_and(|value| crate::runner::parse_bool(value) == Some(false))
    };
    if disabled("core.filemode") || disabled("core.symlinks") {
        return Ok(None); // Git compares modes differently there
    }
    let index: BTreeMap<String, store::IndexEntry> = match store::read_index(&git.dir) {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect(),
        Err(_) => return Ok(None),
    };
    let lookup = |tree: &str, path: &str| -> Result<Option<Entry>> {
        git.fast(|store| store.entry_at(tree, path))
            .context("in-process store unavailable")
    };
    let mut files: BTreeMap<String, FileChange> = before
        .files
        .iter()
        .map(|file| (file.path.clone(), file.clone()))
        .collect();
    for path in &candidates {
        let previous = files.remove(path);
        let old = match old_entries.get(path) {
            Some(entry) => entry.clone(),
            None => lookup(&old_tree, path)?,
        };
        let new = match new_entries.get(path) {
            Some(entry) => entry.clone(),
            None => lookup(&new_tree, path)?,
        };
        let in_index = index.get(path);
        if in_index.is_some_and(|entry| entry.stage != 0 || entry.intent_to_add) {
            return Ok(None);
        }
        // The worktree content as Git would hash it; `raw` when hashed here without filters.
        let (worktree, raw): (Side, bool) = match written.get(path.as_str()) {
            Some(write) => match &write.after {
                None => (Side::Absent, false),
                Some(_) => (worktree_side(&git.root, path), true),
            },
            None => match &previous {
                None => match &old {
                    Some((mode, oid)) => (Side::Present(mode.clone(), oid.clone()), false),
                    None => (worktree_side(&git.root, path), true),
                },
                Some(file) if file.kind == Kind::Deleted => (Side::Absent, false),
                Some(file) if file.kind == Kind::Untracked => {
                    (worktree_side(&git.root, path), true)
                }
                Some(file) => match (&file.new_mode, &file.new_oid) {
                    (Some(mode), Some(oid)) if !oid.bytes().all(|byte| byte == b'0') => {
                        (Side::Present(mode.clone(), oid.clone()), false)
                    }
                    _ => return Ok(None),
                },
            },
        };
        let dirty = match (&worktree, &new, in_index) {
            (Side::Unknown, _, _) => return Ok(None),
            (Side::Absent, None, None) => None,
            (Side::Absent, Some(_), Some(_)) => Some(modified_stub(path)),
            (Side::Present(mode, oid), Some((new_mode, new_oid)), Some(_)) => {
                if mode == new_mode && oid == new_oid && (!raw || git.no_conversion(path)) {
                    None
                } else if raw {
                    return Ok(None); // raw bytes differ: filters could still make them equal
                } else {
                    Some(previous.clone().unwrap_or_else(|| modified_stub(path)))
                }
            }
            // A new file only in the index: `git diff HEAD` shows it as added.
            (Side::Present(..), None, Some(_)) if !raw => {
                Some(previous.clone().unwrap_or_else(|| modified_stub(path)))
            }
            (Side::Present(..), None, None) => {
                if std::fs::symlink_metadata(git.root.join(path))
                    .is_ok_and(|metadata| metadata.is_dir())
                    || !no_ignore_rules(git, path)?
                {
                    return Ok(None);
                }
                Some(untracked_stub(path))
            }
            _ => return Ok(None),
        };
        if let Some(file) = dirty {
            files.insert(path.clone(), file);
        }
    }
    Ok(Some(ChangeSet {
        files: files.into_values().collect(),
        nested: before.nested,
    }))
}
