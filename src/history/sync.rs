//! `aisc sync`: replay the branch onto a moved base (algorithm 6.6 of docs/design/history.md),
//! keeping uncommitted edits. Conflicts pause the sync with marker files and
//! `$GIT_DIR/aisc/sync.json`; refs, the index and unrelated files are untouched until `--continue`.
//! This module also hosts the replay helpers that `land` reuses; the worktree carry is in rewrite.
use super::plumbing::{CommitMeta, Entry, Favor, Git, MergeConflict, TreeEdit};
use super::render;
use super::rewrite::{
    Plan, RefMove, Session, WorktreeWrite, carry, dependent_moves, read_work, regular, stale,
    work_matches, write_file,
};
use super::view::{self, BranchRef};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use crate::store::Kind;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------------------------
// Replay engine (shared with land)
// ---------------------------------------------------------------------------------------------

/// One commit to replay, with the trees the three-way merge needs.
#[derive(Clone, Debug)]
pub(crate) struct Pick {
    pub oid: String,
    pub short: String,
    pub subject: String,
    pub tree: String,
    pub parent: String,
    pub parent_tree: String,
}

/// Commits reachable from `tip` but not from `exclude`, oldest first. Merges and roots are refused.
pub(crate) fn load_picks(git: &Git, tip: &str, exclude: &str) -> Result<Vec<Pick>> {
    load_picks_excluding(git, tip, &[exclude])
}

/// The commits of `tip` that none of `excludes` contain, oldest first.
pub(crate) fn load_picks_excluding(git: &Git, tip: &str, excludes: &[&str]) -> Result<Vec<Pick>> {
    let negatives: Vec<String> = excludes.iter().map(|oid| format!("^{oid}")).collect();
    let mut args = vec![
        "log",
        "--reverse",
        "--topo-order",
        "-z",
        "--abbrev=4",
        "--format=%H%x1f%h%x1f%T%x1f%P%x1f%s",
        tip,
    ];
    args.extend(negatives.iter().map(String::as_str));
    args.push("--");
    let output = git.run(&args)?;
    let mut picks = Vec::new();
    for record in output.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record).into_owned();
        let mut fields = record.split('\u{1f}');
        let oid = fields.next().context("log oid")?.to_owned();
        let short = fields.next().context("log short id")?.to_owned();
        let tree = fields.next().context("log tree")?.to_owned();
        let parents: Vec<&str> = fields
            .next()
            .context("log parents")?
            .split_whitespace()
            .collect();
        let subject = fields.next().unwrap_or_default().to_owned();
        let [parent] = parents[..] else {
            return Err(fail_hint(
                Code::Unsupported,
                format!("commit {short} \"{subject}\" is a merge or root commit"),
                "only linear history can be replayed; squash or rebuild it first",
            ));
        };
        picks.push(Pick {
            oid,
            short,
            subject,
            tree,
            parent: parent.to_owned(),
            parent_tree: String::new(),
        });
    }
    let in_process: Option<Vec<String>> = picks
        .iter()
        .map(|pick| git.fast(|store| store.peel_to_tree(&pick.parent)).flatten())
        .collect();
    if let Some(trees) = in_process {
        for (pick, tree) in picks.iter_mut().zip(trees) {
            pick.parent_tree = tree;
        }
    } else if !picks.is_empty() {
        let arguments: Vec<String> = picks
            .iter()
            .map(|pick| format!("{}^{{tree}}", pick.parent))
            .collect();
        let mut args = vec!["rev-parse"];
        args.extend(arguments.iter().map(String::as_str));
        let trees = git.line(&args)?;
        for (pick, tree) in picks.iter_mut().zip(trees.lines()) {
            pick.parent_tree = tree.to_owned();
        }
    }
    Ok(picks)
}

pub(crate) type Resolutions = BTreeMap<String, BTreeMap<String, Option<String>>>;

pub(crate) struct ReplayOpts<'a> {
    pub onto_label: &'a str,
    pub resolutions: &'a Resolutions,
    pub prefer_branch: &'a [String],
    pub prefer_onto: &'a [String],
}

/// A conflict that needs a human: the provisional blob already holds diff3 markers.
pub(crate) struct Conflicted {
    pub commit: usize,
    pub path: String,
    pub marker: Vec<u8>,
    pub mode: String,
}

pub(crate) struct Replayed {
    /// Result tree per pick; `None` marks a commit that became empty (already upstream).
    pub trees: Vec<Option<String>>,
    pub final_tree: String,
    pub pending: Vec<Conflicted>,
    /// Later commits touched an already pending path; more conflicts may follow.
    pub deferred: bool,
}

fn matches_prefer(list: &[String], path: &str) -> bool {
    list.iter().any(|candidate| {
        let candidate = candidate.trim_end_matches('/');
        path == candidate
            || path
                .strip_prefix(candidate)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn stage_mode(stages: &[Option<Entry>; 3]) -> String {
    stages[2]
        .as_ref()
        .or(stages[1].as_ref())
        .or(stages[0].as_ref())
        .map_or_else(|| "100644".to_owned(), |(mode, _)| mode.clone())
}

enum Resolved {
    Entry(Option<Entry>),
    Pending(Vec<u8>, Entry),
}

fn resolve_conflict(
    git: &Git,
    pick: &Pick,
    conflict: &MergeConflict,
    opts: &ReplayOpts<'_>,
) -> Result<Resolved> {
    let path = &conflict.path;
    let stages = &conflict.stages;
    let mode = stage_mode(stages);
    if let Some(stored) = opts
        .resolutions
        .get(&pick.oid)
        .and_then(|paths| paths.get(path))
    {
        return Ok(Resolved::Entry(
            stored.as_ref().map(|oid| (mode.clone(), oid.clone())),
        ));
    }
    let prefer_branch = matches_prefer(opts.prefer_branch, path);
    let prefer_onto = !prefer_branch && matches_prefer(opts.prefer_onto, path);
    let all_text = stages
        .iter()
        .all(|stage| stage.as_ref().is_some_and(|(mode, _)| regular(mode)));
    if prefer_branch || prefer_onto {
        if all_text {
            let favor = if prefer_branch {
                Favor::Theirs
            } else {
                Favor::Ours
            };
            let labels = [opts.onto_label, "base", "branch"];
            let (bytes, _) = git
                .merge_file(
                    &stages[0].as_ref().expect("checked").1,
                    &stages[1].as_ref().expect("checked").1,
                    &stages[2].as_ref().expect("checked").1,
                    labels,
                    favor,
                    false,
                )
                .map_err(|_| unmergeable(path))?;
            let oid = git.write_object(Kind::Blob, &bytes)?;
            return Ok(Resolved::Entry(Some((mode, oid))));
        }
        let side = if prefer_branch { 2 } else { 1 };
        return Ok(Resolved::Entry(stages[side].clone()));
    }
    let blob = |stage: &Option<Entry>| -> Result<String> {
        match stage {
            Some((mode, oid)) if regular(mode) => Ok(oid.clone()),
            None => git.write_object(Kind::Blob, b""),
            Some(_) => Err(fail_hint(
                Code::Unsupported,
                format!("{path}: conflict in a symlink or submodule"),
                "pass --prefer-branch or --prefer-onto for this path",
            )),
        }
    };
    let (base, ours, theirs) = (blob(&stages[0])?, blob(&stages[1])?, blob(&stages[2])?);
    let theirs_label = format!("{} {}", pick.short, pick.subject);
    let (bytes, count) = git
        .merge_file(
            &base,
            &ours,
            &theirs,
            [opts.onto_label, "base", &theirs_label],
            Favor::None,
            true,
        )
        .map_err(|_| unmergeable(path))?;
    let oid = git.write_object(Kind::Blob, &bytes)?;
    if count == 0 {
        return Ok(Resolved::Entry(Some((mode, oid))));
    }
    Ok(Resolved::Pending(bytes, (mode, oid)))
}

fn unmergeable(path: &str) -> anyhow::Error {
    fail_hint(
        Code::Unsupported,
        format!("{path}: cannot be merged as text (binary file?)"),
        "pass --prefer-branch or --prefer-onto for this path",
    )
}

/// Replays `picks` (oldest first) onto `onto_tree`, working on trees only; no commit is created.
pub(crate) fn replay(
    git: &Git,
    onto_tree: &str,
    picks: &[Pick],
    opts: &ReplayOpts<'_>,
) -> Result<Replayed> {
    let mut current = onto_tree.to_owned();
    let mut trees = Vec::new();
    let mut pending: Vec<Conflicted> = Vec::new();
    let mut pending_paths: BTreeSet<String> = BTreeSet::new();
    let mut deferred = false;
    for (index, pick) in picks.iter().enumerate() {
        let result = if current == pick.parent_tree {
            pick.tree.clone()
        } else {
            let merged = git.merge_trees(&pick.parent_tree, &current, &pick.tree)?;
            if merged.conflicts.is_empty() {
                merged.tree
            } else {
                let mut edits = Vec::new();
                for conflict in &merged.conflicts {
                    if pending_paths.contains(&conflict.path) {
                        deferred = true;
                        let kept = git
                            .ls_tree(&current, std::slice::from_ref(&conflict.path))?
                            .remove(&conflict.path);
                        edits.push(TreeEdit {
                            path: conflict.path.clone(),
                            entry: kept,
                        });
                        continue;
                    }
                    match resolve_conflict(git, pick, conflict, opts)? {
                        Resolved::Entry(entry) => edits.push(TreeEdit {
                            path: conflict.path.clone(),
                            entry,
                        }),
                        Resolved::Pending(marker, entry) => {
                            pending_paths.insert(conflict.path.clone());
                            pending.push(Conflicted {
                                commit: index,
                                path: conflict.path.clone(),
                                marker,
                                mode: entry.0.clone(),
                            });
                            edits.push(TreeEdit {
                                path: conflict.path.clone(),
                                entry: Some(entry),
                            });
                        }
                    }
                }
                git.tree_with(&merged.tree, &edits)?
            }
        };
        if result == current && pick.tree != pick.parent_tree {
            trees.push(None);
        } else {
            trees.push(Some(result.clone()));
        }
        current = result;
    }
    Ok(Replayed {
        trees,
        final_tree: current,
        pending,
        deferred,
    })
}

pub(crate) struct Built {
    pub tip: String,
    pub rewritten: Vec<(String, String)>,
    pub dropped: Vec<usize>,
}

/// Creates the replayed commits (author, date and message kept). Unchanged prefixes keep their ids.
pub(crate) fn build_commits(
    git: &Git,
    onto_oid: &str,
    picks: &[Pick],
    trees: &[Option<String>],
) -> Result<Built> {
    let mut parent = onto_oid.to_owned();
    let mut rewritten = Vec::new();
    let mut dropped = Vec::new();
    for (index, (pick, tree)) in picks.iter().zip(trees).enumerate() {
        let Some(tree) = tree else {
            dropped.push(index);
            rewritten.push((pick.oid.clone(), parent.clone()));
            continue;
        };
        let new = if parent == pick.parent && *tree == pick.tree {
            pick.oid.clone()
        } else {
            let meta: CommitMeta = git.read_commit_meta(&pick.oid)?;
            git.commit_tree(
                tree,
                std::slice::from_ref(&parent),
                &meta.message,
                Some(&meta),
            )?
        };
        if new != pick.oid {
            rewritten.push((pick.oid.clone(), new.clone()));
        }
        parent = new;
    }
    Ok(Built {
        tip: parent,
        rewritten,
        dropped,
    })
}

// ---------------------------------------------------------------------------------------------
// Sync state
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Materialized {
    path: String,
    file: String,
    commit: String,
    subject: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Restore {
    path: String,
    /// `path` itself (the marker replaced a clean file) or `path.aisc-conflict` (a side file).
    file: String,
    mode: String,
    /// Raw bytes (blob id) the file had before the marker was written; `None` when absent.
    original: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    branch: Option<String>,
    is_head: bool,
    old_tip: String,
    onto_spec: String,
    onto_oid: String,
    prefer_branch: Vec<String>,
    prefer_onto: Vec<String>,
    resolutions: Resolutions,
    materialized: Vec<Materialized>,
    restore: Vec<Restore>,
    /// The branch's upstream (`origin/NAME`) when it has commits the branch lacks: they are
    /// replayed first, then the branch's own commits.
    #[serde(default)]
    upstream: Option<Upstream>,
}

/// Commits someone else pushed to the branch's upstream, to replay before the branch's own.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Upstream {
    /// `origin/NAME`.
    label: String,
    oid: String,
    /// Merge base of the branch and the upstream: commits below it are shared.
    base: String,
    /// Earlier tips of the local branch (its reflog). Upstream commits they contain were the
    /// user's own before a local rewrite (squash, amend) and are not taken back.
    seen: Vec<String>,
}

impl State {
    fn ref_name(&self) -> String {
        self.branch
            .as_ref()
            .map_or_else(|| "HEAD".to_owned(), |name| format!("refs/heads/{name}"))
    }
    fn label(&self) -> String {
        self.branch
            .clone()
            .unwrap_or_else(|| "HEAD (detached)".to_owned())
    }
}

fn state_path(git: &Git) -> PathBuf {
    git.dir.join("aisc").join("sync.json")
}

fn save_state(git: &Git, state: &State) -> Result<()> {
    let path = state_path(git);
    let directory = path.parent().context("state directory")?;
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!("sync.json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn load_state(git: &Git) -> Result<State> {
    let bytes = fs::read(state_path(git))
        .map_err(|_| fail_hint(Code::State, "no sync is paused", "start one with aisc sync"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| fail(Code::State, format!("unreadable sync state: {error}")))
}

fn pin_ref(git: &Git) -> Result<String> {
    Ok(super::oplog::log_ref(git)?.replace("/oplog/", "/sync/"))
}

/// Keeps the saved originals and resolutions reachable (gc-proof) while the sync is paused.
fn pin_blobs(git: &Git, state: &State) -> Result<()> {
    let mut oids: BTreeSet<String> = state
        .restore
        .iter()
        .filter_map(|entry| entry.original.clone())
        .collect();
    for paths in state.resolutions.values() {
        oids.extend(paths.values().flatten().cloned());
    }
    if oids.is_empty() {
        return Ok(());
    }
    let edits: Vec<TreeEdit> = oids
        .iter()
        .map(|oid| TreeEdit {
            path: format!("blobs/{oid}"),
            entry: Some(("100644".to_owned(), oid.clone())),
        })
        .collect();
    let tree = git.tree_with(&git.empty_tree()?, &edits)?;
    let name = pin_ref(git)?;
    if let Some(existing) = git.ref_oid(&name)?
        && git.commit_tree_of(&existing)? == tree
    {
        return Ok(());
    }
    let commit = git.commit_tree(&tree, &[], b"aisc sync pin", None)?;
    git.run(&["update-ref", &name, &commit])?;
    Ok(())
}

fn unpin(git: &Git) {
    if let Ok(name) = pin_ref(git) {
        let _ = git.run(&["update-ref", "-d", &name]);
    }
}

fn short(oid: &str) -> String {
    oid.chars().take(4).collect()
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

pub(crate) fn run(args: &cli::Sync) -> Result<Outcome> {
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    if args.continue_sync {
        return resume(&repo, &git, args);
    }
    if args.abort {
        return abort(&repo, &git);
    }
    start(&repo, &git, args)
}

fn remote_for(git: &Git, spec: Option<&str>) -> Result<String> {
    let remotes = git.remotes()?;
    if let Some(spec) = spec {
        let name = spec.strip_prefix("refs/remotes/").unwrap_or(spec);
        if let Some((remote, _)) = name.split_once('/')
            && remotes.iter().any(|candidate| candidate == remote)
        {
            return Ok(remote.to_owned());
        }
        let key = format!("branch.{name}.remote");
        if let Some(remote) = crate::runner::config(&git.root)?
            .string(&key)
            .map(str::to_owned)
            && remotes.contains(&remote)
        {
            return Ok(remote);
        }
    }
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok("origin".to_owned());
    }
    match remotes.as_slice() {
        [only] => Ok(only.clone()),
        _ => Err(fail_hint(
            Code::Usage,
            "cannot tell which remote to fetch",
            "pass ONTO as REMOTE/BRANCH, for example aisc sync origin/main --fetch",
        )),
    }
}

fn start(repo: &Repo, git: &Git, args: &cli::Sync) -> Result<Outcome> {
    let head = git.head_state()?;
    let head_branch = head
        .symbolic
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    // The target: HEAD (attached or detached) or another local branch via --branch.
    let (branch, is_head) = match &args.opts.branch {
        Some(name) if Some(name) != head_branch.as_ref() => (Some(name.clone()), false),
        _ => (head_branch.clone(), true),
    };
    let session = Session::open(repo, git, is_head, false)?;
    let old_tip = if is_head {
        session.head.oid.clone()
    } else {
        let name = branch.as_deref().expect("branch is set");
        if git.checked_out()?.contains(&format!("refs/heads/{name}")) {
            return Err(fail_hint(
                Code::State,
                format!("branch '{name}' is checked out in another worktree"),
                "run aisc sync from that worktree",
            ));
        }
        git.ref_oid(&format!("refs/heads/{name}"))?.ok_or_else(|| {
            fail_hint(
                Code::Selector,
                format!("unknown branch '{name}'"),
                "use a local branch name",
            )
        })?
    };
    if is_head && branch.is_none() && args.opts.branch.is_some() {
        return Err(fail(Code::Usage, "HEAD is detached"));
    }

    // Base / ONTO.
    let target_view = view::read(git, branch.as_deref(), 1, false)?;
    let mut onto_spec = args.onto.clone();
    if args.fetch {
        let base_label = target_view.base.as_ref().map(|base| base.label.clone());
        let remote = remote_for(git, onto_spec.as_deref().or(base_label.as_deref()))?;
        git.run(&["fetch", "--quiet", &remote])?;
        if onto_spec.is_none()
            && let Some(label) = &base_label
            && !label.contains('/')
        {
            let tracking = format!("{remote}/{label}");
            if git.ref_oid(&format!("refs/remotes/{tracking}"))?.is_some() {
                onto_spec = Some(tracking);
            }
        }
    }
    let target_view = if args.fetch {
        view::read(git, branch.as_deref(), 1, false)?
    } else {
        target_view
    };
    let (onto_spec, onto_oid) = match onto_spec {
        Some(spec) => {
            let oid = git.resolve_commit(&spec)?.ok_or_else(|| {
                fail_hint(
                    Code::Selector,
                    format!("unknown base '{spec}'"),
                    "use a branch name or REMOTE/BRANCH",
                )
            })?;
            (spec, oid)
        }
        None => {
            let base = target_view.base.as_ref().ok_or_else(|| {
                fail_hint(
                    Code::Usage,
                    "this branch has no base to sync onto",
                    "pass ONTO, for example aisc sync origin/main",
                )
            })?;
            (base.label.clone(), base.oid.clone())
        }
    };
    let upstream = match &branch {
        Some(name) => new_upstream_commits(git, name, &old_tip, &onto_oid)?,
        None => None,
    };
    if upstream.is_none()
        && git
            .probe(&["merge-base", "--is-ancestor", &onto_oid, &old_tip])?
            .is_some()
    {
        let label = branch
            .clone()
            .unwrap_or_else(|| "HEAD (detached)".to_owned());
        let text = format!(
            "{label} is up to date with {onto_spec} {}",
            short(&onto_oid)
        );
        return Ok(Outcome::with_text(
            json!({"verb": "sync", "status": "up-to-date", "branch": label,
                "onto": onto_spec, "onto_oid": onto_oid}),
            text,
        ));
    }
    let state = State {
        branch,
        is_head,
        old_tip,
        onto_spec,
        onto_oid,
        prefer_branch: args.prefer_branch.clone(),
        prefer_onto: args.prefer_onto.clone(),
        resolutions: BTreeMap::new(),
        materialized: Vec::new(),
        restore: Vec::new(),
        upstream,
    };
    drive(git, session, state, args.opts.quiet)
}

/// `NAME@{upstream}` when it holds commits that the branch never had: someone else pushed to
/// it. Commits the branch had before a local rewrite (in its reflog) do not count, so a squash
/// or amend of pushed work is never undone. `None` without an upstream, a reflog or new commits.
fn new_upstream_commits(git: &Git, name: &str, tip: &str, onto: &str) -> Result<Option<Upstream>> {
    let spec = format!("{name}@{{upstream}}");
    let Some(refname) =
        git.probe(&["rev-parse", "--verify", "-q", "--symbolic-full-name", &spec])?
    else {
        return Ok(None);
    };
    let refname = refname.trim().to_owned();
    let Some(oid) = git.ref_oid(&refname)? else {
        return Ok(None);
    };
    if oid == onto
        || git
            .probe(&["merge-base", "--is-ancestor", &oid, tip])?
            .is_some()
        || git
            .probe(&["merge-base", "--is-ancestor", &oid, onto])?
            .is_some()
    {
        return Ok(None);
    }
    // Without the branch's reflog a local rewrite cannot be told from a foreign push.
    let Ok(log) = std::fs::read_to_string(git.common_dir()?.join("logs/refs/heads").join(name))
    else {
        return Ok(None);
    };
    // Tips the branch reached by importing other people's commits (a sync, pull, merge, reset or
    // undo) do not make those commits the user's own.
    let imported = |message: &str| {
        [
            "aisc sync",
            "aisc undo",
            "aisc land",
            "pull",
            "merge",
            "reset",
            "rebase",
            "fetch",
            "branch: Reset",
        ]
        .iter()
        .any(|prefix| message.starts_with(prefix))
    };
    let mut seen: Vec<String> = log
        .lines()
        .filter(|line| {
            !line
                .split_once('\t')
                .is_some_and(|(_, message)| imported(message))
        })
        .filter_map(|line| line.split(' ').nth(1))
        .filter(|oid| oid.len() >= 40 && oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .filter(|oid| !oid.bytes().all(|byte| byte == b'0'))
        .map(str::to_owned)
        .collect();
    // Whatever the user pushed to the upstream is theirs too, even if only a sync or rebase
    // made it locally: Git records each push in the remote-tracking reflog.
    if let Ok(remote_log) = std::fs::read_to_string(git.common_dir()?.join("logs").join(&refname)) {
        seen.extend(
            remote_log
                .lines()
                .filter(|line| {
                    line.split_once('\t')
                        .is_some_and(|(_, message)| message.starts_with("update by push"))
                })
                .filter_map(|line| line.split(' ').nth(1))
                .filter(|oid| oid.len() >= 40 && oid.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .filter(|oid| !oid.bytes().all(|byte| byte == b'0'))
                .map(str::to_owned),
        );
    }
    seen.sort();
    seen.dedup();
    if seen.len() > 512 {
        return Ok(None);
    }
    let Some(base) = git.probe(&["merge-base", tip, &oid])? else {
        return Ok(None);
    };
    let mut excludes: Vec<&str> = vec![base.as_str(), onto, tip];
    excludes.extend(seen.iter().map(String::as_str));
    if load_picks_excluding(git, &oid, &excludes)?.is_empty() {
        return Ok(None);
    }
    let label = refname
        .strip_prefix("refs/remotes/")
        .unwrap_or(&refname)
        .to_owned();
    Ok(Some(Upstream {
        label,
        oid,
        base,
        seen,
    }))
}

fn resume(repo: &Repo, git: &Git, args: &cli::Sync) -> Result<Outcome> {
    let mut state = load_state(git)?;
    let session = Session::open(repo, git, state.is_head, true)?;
    check_not_moved(git, &session, &state)?;
    let mut hashed = BTreeMap::new();
    for entry in &state.materialized {
        let full = git.root.join(&entry.file);
        let bytes = match fs::read(&full) {
            Ok(bytes) => Some(bytes),
            Err(_) if entry.file == entry.path => None,
            Err(_) => {
                return Err(fail_hint(
                    Code::Conflict,
                    format!("{} is missing", entry.file),
                    "restore the file or run aisc sync --abort",
                ));
            }
        };
        if let Some(bytes) = &bytes
            && has_markers(bytes)
        {
            return Err(fail_hint(
                Code::Conflict,
                format!("{} still contains conflict markers", entry.file),
                "resolve every conflict region, then run aisc sync --continue again",
            ));
        }
        let oid = match bytes {
            Some(_) => Some(
                git.hash_worktree(std::slice::from_ref(&entry.file))?
                    .remove(0),
            ),
            None => None,
        };
        hashed.insert((entry.commit.clone(), entry.path.clone()), oid);
    }
    for ((commit, path), oid) in hashed {
        state
            .resolutions
            .entry(commit)
            .or_default()
            .insert(path, oid);
    }
    pin_blobs(git, &state)?;
    drive(git, session, state, args.opts.quiet)
}

fn has_markers(bytes: &[u8]) -> bool {
    bytes.split(|byte| *byte == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> ") || line == b"======="
    })
}

fn check_not_moved(git: &Git, session: &Session, state: &State) -> Result<()> {
    let current = if state.is_head {
        Some(session.head.oid.clone())
    } else {
        git.ref_oid(&state.ref_name())?
    };
    if current.as_deref() != Some(state.old_tip.as_str()) {
        return Err(stale(&format!(
            "{} moved while the sync was paused",
            state.label()
        )));
    }
    if state.is_head && state.branch.is_some() {
        let expected = state.ref_name();
        if session.head.symbolic.as_deref() != Some(expected.as_str()) {
            return Err(stale("HEAD moved while the sync was paused"));
        }
    }
    Ok(())
}

fn abort(repo: &Repo, git: &Git) -> Result<Outcome> {
    let state = load_state(git)?;
    let session = Session::open(repo, git, false, true)?;
    check_not_moved(git, &session, &state).map_err(|_| {
        fail_hint(
            Code::Stale,
            format!("{} moved since the sync was paused; nothing was changed", state.label()),
            "if the sync already applied, use aisc undo; otherwise delete .git/aisc/sync.json after checking aisc log",
        )
    })?;
    let originals: Vec<String> = state
        .restore
        .iter()
        .filter_map(|entry| entry.original.clone())
        .collect();
    let mut blobs = git.read_blobs(&originals)?.into_iter();
    for entry in &state.restore {
        if entry.file == entry.path {
            let write = WorktreeWrite {
                path: entry.path.clone(),
                before: None,
                before_mode: None,
                after: entry.original.as_ref().and_then(|_| blobs.next()),
                mode: entry.mode.clone(),
            };
            write_file(&git.root, &write)?;
        } else {
            let write = WorktreeWrite {
                path: entry.file.clone(),
                before: None,
                before_mode: None,
                after: None,
                mode: entry.mode.clone(),
            };
            write_file(&git.root, &write)?;
        }
    }
    fs::remove_file(state_path(git))?;
    unpin(git);
    let label = state.label();
    let text = format!("aborted sync of {label}; files restored, nothing was committed");
    Ok(Outcome::with_text(
        json!({"verb": "sync", "status": "aborted", "branch": label,
            "restored": state.restore.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>()}),
        text,
    ))
}

/// Replays with the current resolutions, then pauses on conflicts or finishes the rewrite.
fn drive(git: &Git, session: Session, mut state: State, quiet: bool) -> Result<Outcome> {
    let picks = match &state.upstream {
        Some(upstream) => {
            // Shared history, then the foreign upstream commits, then the branch's own.
            let mut picks = load_picks(git, &upstream.base, &state.onto_oid)?;
            let mut excludes: Vec<&str> = vec![&upstream.base, &state.onto_oid, &state.old_tip];
            excludes.extend(upstream.seen.iter().map(String::as_str));
            let foreign = load_picks_excluding(git, &upstream.oid, &excludes)?;
            let own =
                load_picks_excluding(git, &state.old_tip, &[&upstream.base, &state.onto_oid])?;
            // Foreign commits built on the shared history go first, as `git pull --rebase` does.
            // Built on commits the user has since rewritten, they go after the user's own.
            let on_shared = foreign.first().is_none_or(|first| {
                first.parent == upstream.base
                    || git
                        .probe(&["merge-base", "--is-ancestor", &first.parent, &upstream.base])
                        .is_ok_and(|found| found.is_some())
            });
            if on_shared {
                picks.extend(foreign);
                picks.extend(own);
            } else {
                picks.extend(own);
                picks.extend(foreign);
            }
            picks
        }
        None => load_picks(git, &state.old_tip, &state.onto_oid)?,
    };
    let onto_tree = git.commit_tree_of(&state.onto_oid)?;
    let old_tree = git.commit_tree_of(&state.old_tip)?;
    let opts = ReplayOpts {
        onto_label: &state.onto_spec,
        resolutions: &state.resolutions,
        prefer_branch: &state.prefer_branch,
        prefer_onto: &state.prefer_onto,
    };
    let replayed = replay(git, &onto_tree, &picks, &opts)?;
    if !replayed.pending.is_empty() {
        return pause(git, session, &mut state, &picks, &old_tree, &replayed);
    }
    finish(git, session, state, &picks, &old_tree, &replayed, quiet)
}

fn pause(
    git: &Git,
    session: Session,
    state: &mut State,
    picks: &[Pick],
    old_tree: &str,
    replayed: &Replayed,
) -> Result<Outcome> {
    let pending_paths: BTreeSet<String> = replayed.pending.iter().map(|c| c.path.clone()).collect();
    if !state.is_head {
        let mut message = format!(
            "sync of {} onto {} conflicts; nothing was changed",
            state.label(),
            state.onto_spec
        );
        for conflict in &replayed.pending {
            let pick = &picks[conflict.commit];
            message.push_str(&format!(
                "\n{} in {} \"{}\"",
                escape_line(&conflict.path),
                pick.short,
                escape_line(&pick.subject)
            ));
        }
        return Err(fail_hint(
            Code::Conflict,
            message,
            "check out the branch and run aisc sync, or use --prefer-branch / --prefer-onto",
        ));
    }
    // Refuse early when uncommitted edits collide with incoming changes elsewhere.
    carry(git, old_tree, &replayed.final_tree, &pending_paths)?;

    // Decide where each marker file goes and capture what is overwritten.
    let tree_entries = git.ls_tree(old_tree, &pending_paths.iter().cloned().collect::<Vec<_>>())?;
    let mut placements: Vec<(String, Vec<u8>, String)> = Vec::new(); // (file, marker, mode)
    let mut restore = state.restore.clone();
    let mut materialized = Vec::new();
    for conflict in &replayed.pending {
        let pick = &picks[conflict.commit];
        let existing = restore
            .iter()
            .find(|entry| entry.path == conflict.path)
            .cloned();
        let file = if let Some(existing) = existing {
            existing.file
        } else {
            let entry = tree_entries.get(&conflict.path);
            let work = read_work(&git.root, &conflict.path)?;
            let hash = if work.is_absent() {
                None
            } else {
                Some(
                    git.hash_worktree(std::slice::from_ref(&conflict.path))?
                        .remove(0),
                )
            };
            let clean = work_matches(&work, hash.as_ref(), entry);
            let file = if clean {
                conflict.path.clone()
            } else {
                let side = format!("{}.aisc-conflict", conflict.path);
                if fs::symlink_metadata(git.root.join(&side)).is_ok() {
                    return Err(fail_hint(
                        Code::Dirty,
                        format!("{side} already exists"),
                        "move it away, then retry",
                    ));
                }
                side
            };
            let original = if clean && !work.is_absent() {
                let bytes = work.bytes().unwrap_or_default();
                Some(git.write_blob(&bytes, true)?)
            } else {
                None
            };
            restore.push(Restore {
                path: conflict.path.clone(),
                file: file.clone(),
                mode: entry.map_or_else(|| "100644".to_owned(), |e| e.0.clone()),
                original,
            });
            file
        };
        placements.push((file.clone(), conflict.marker.clone(), conflict.mode.clone()));
        materialized.push(Materialized {
            path: conflict.path.clone(),
            file,
            commit: pick.oid.clone(),
            subject: pick.subject.clone(),
        });
    }
    state.restore = restore;
    state.materialized = materialized;
    pin_blobs(git, state)?;
    save_state(git, state)?;
    for (file, marker, mode) in &placements {
        let write = WorktreeWrite {
            path: file.clone(),
            before: None,
            before_mode: None,
            after: Some(marker.clone()),
            mode: if mode == "100755" {
                mode.clone()
            } else {
                "100644".to_owned()
            },
        };
        if let Err(error) = write_file(&git.root, &write) {
            // Put everything back; the state file goes with it.
            let _ = restore_files(git, state);
            let _ = fs::remove_file(state_path(git));
            unpin(git);
            return Err(error.context("could not write conflict markers"));
        }
    }
    drop(session);
    Ok(paused_outcome(state, picks, replayed, &placements))
}

fn restore_files(git: &Git, state: &State) -> Result<()> {
    let originals: Vec<String> = state
        .restore
        .iter()
        .filter_map(|e| e.original.clone())
        .collect();
    let mut blobs = git.read_blobs(&originals)?.into_iter();
    for entry in &state.restore {
        let after = if entry.file == entry.path {
            entry.original.as_ref().and_then(|_| blobs.next())
        } else {
            None
        };
        write_file(
            &git.root,
            &WorktreeWrite {
                path: entry.file.clone(),
                before: None,
                before_mode: None,
                after,
                mode: entry.mode.clone(),
            },
        )?;
    }
    Ok(())
}

struct Region {
    line: usize,
    onto: Vec<String>,
    base: Vec<String>,
    branch: Vec<String>,
    first: usize,
    last: usize,
}

fn parse_regions(bytes: &[u8]) -> (Vec<String>, Vec<Region>) {
    let lines: Vec<String> = String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_owned)
        .collect();
    let mut regions = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].starts_with("<<<<<<< ") {
            let first = index;
            let (mut onto, mut base, mut branch) = (Vec::new(), Vec::new(), Vec::new());
            let mut section = 0;
            index += 1;
            while index < lines.len() && !lines[index].starts_with(">>>>>>> ") {
                let line = &lines[index];
                if line.starts_with("||||||| ") || line == "|||||||" {
                    section = 1;
                } else if line == "=======" {
                    section = 2;
                } else {
                    match section {
                        0 => onto.push(line.clone()),
                        1 => base.push(line.clone()),
                        _ => branch.push(line.clone()),
                    }
                }
                index += 1;
            }
            regions.push(Region {
                line: first + 1,
                onto,
                base,
                branch,
                first,
                last: index.min(lines.len().saturating_sub(1)),
            });
        }
        index += 1;
    }
    (lines, regions)
}

/// Marker lines of the conflict regions only, with up to 3 lines of context between regions.
fn compact_regions(lines: &[String], regions: &[Region]) -> Vec<String> {
    const CAP: usize = 40;
    let mut out = Vec::new();
    let mut previous_end: Option<usize> = None;
    for region in regions {
        if let Some(end) = previous_end {
            let gap = region.first.saturating_sub(end + 1);
            if gap <= 3 {
                out.extend(lines[end + 1..region.first].iter().cloned());
            } else {
                out.push("…".to_owned());
            }
        }
        out.extend(lines[region.first..=region.last].iter().cloned());
        previous_end = Some(region.last);
    }
    if out.len() > CAP {
        let more = out.len() - CAP;
        out.truncate(CAP);
        out.push(format!("… +{more} lines"));
    }
    out.iter().map(|line| escape_line(line)).collect()
}

fn paused_outcome(
    state: &State,
    picks: &[Pick],
    replayed: &Replayed,
    placements: &[(String, Vec<u8>, String)],
) -> Outcome {
    let onto = format!("{} {}", state.onto_spec, short(&state.onto_oid));
    let mut text = format!(
        "CONFLICTS: sync {} onto {} paused, nothing committed. Edit the marked files, then: aisc sync --continue (or --abort)",
        state.label(),
        onto
    );
    let mut conflicts = Vec::new();
    for (conflict, (file, marker, _)) in replayed.pending.iter().zip(placements) {
        let pick = &picks[conflict.commit];
        let (lines, regions) = parse_regions(marker);
        text.push_str(&format!(
            "\n{} in {} \"{}\"",
            escape_line(&conflict.path),
            pick.short,
            escape_line(&pick.subject)
        ));
        if *file != conflict.path {
            text.push_str(&format!(
                " (markers are in {}; {} keeps your edits)",
                escape_line(file),
                escape_line(&conflict.path)
            ));
        }
        for line in compact_regions(&lines, &regions) {
            text.push('\n');
            text.push_str(&line);
        }
        conflicts.push(json!({
            "commit": pick.short,
            "subject": pick.subject,
            "path": conflict.path,
            "file": file,
            "regions": regions.iter().map(|region| json!({
                "line": region.line, "onto": region.onto,
                "base": region.base, "branch": region.branch})).collect::<Vec<_>>(),
        }));
    }
    if replayed.deferred {
        text.push_str("\nmore conflicts may follow in later commits");
    }
    Outcome::with_text(
        json!({"verb": "sync", "status": "conflicts", "onto": onto, "conflicts": conflicts,
            "more_may_follow": replayed.deferred,
            "next": ["aisc sync --continue", "aisc sync --abort"]}),
        text,
    )
}

fn local_branches(git: &Git) -> Result<Vec<BranchRef>> {
    if let Some(listed) = git.fast(|store| store.list_refs(&["refs/heads/"])) {
        return Ok(listed
            .into_iter()
            .filter_map(|(name, oid, _)| {
                Some(BranchRef {
                    name: name.strip_prefix("refs/heads/")?.to_owned(),
                    oid,
                })
            })
            .collect());
    }
    let output = git.line(&[
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/heads",
    ])?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let (oid, name) = line.split_once(' ')?;
            Some(BranchRef {
                name: name.strip_prefix("refs/heads/")?.to_owned(),
                oid: oid.to_owned(),
            })
        })
        .collect())
}

fn finish(
    git: &Git,
    session: Session,
    state: State,
    picks: &[Pick],
    old_tree: &str,
    replayed: &Replayed,
    quiet: bool,
) -> Result<Outcome> {
    let built = build_commits(git, &state.onto_oid, picks, &replayed.trees)?;
    let new_tip = built.tip.clone();
    let ref_name = state.ref_name();
    let mut plan = Plan {
        verb: "sync".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    plan.refs.push(RefMove {
        name: ref_name.clone(),
        old: Some(state.old_tip.clone()),
        new: Some(new_tip.clone()),
    });
    plan.rewritten = built.rewritten.clone();
    // Local branches that pointed at a replayed commit move along (D5).
    let locals = local_branches(git)?;
    let skip_name = state.branch.clone().unwrap_or_default();
    let dependents = dependent_moves(&locals, &built.rewritten, &[skip_name.as_str()]);
    if !dependents.is_empty() {
        let elsewhere = git.checked_out()?;
        for moved in &dependents {
            let is_here = session.head.symbolic.as_deref() == Some(moved.name.as_str());
            if elsewhere.contains(&moved.name) && !is_here {
                return Err(fail_hint(
                    Code::State,
                    format!(
                        "{} points at a replayed commit and is checked out in another worktree",
                        moved.name.trim_start_matches("refs/heads/")
                    ),
                    "switch that worktree away first",
                ));
            }
        }
    }
    plan.refs.extend(dependents.clone());

    // The pause ends with the reference transaction, whatever happens after it.
    plan.remove_state = vec!["aisc/sync.json".to_owned()];
    if state.is_head {
        let skip: BTreeSet<String> = state
            .restore
            .iter()
            .filter(|entry| entry.file == entry.path)
            .map(|entry| entry.path.clone())
            .collect();
        let carried = carry(git, old_tree, &replayed.final_tree, &skip)?;
        plan.worktree = carried.writes;
        plan.index = carried.index;
        // Materialized files take the final content of the branch.
        let direct: Vec<&Restore> = state.restore.iter().filter(|e| e.file == e.path).collect();
        let finals = git.ls_tree(
            &replayed.final_tree,
            &direct.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        )?;
        let originals: Vec<String> = direct.iter().filter_map(|e| e.original.clone()).collect();
        let mut original_bytes = git.read_blobs(&originals)?.into_iter();
        let final_oids: Vec<String> = direct
            .iter()
            .filter_map(|entry| finals.get(&entry.path).map(|(_, oid)| oid.clone()))
            .collect();
        let mut final_bytes = git.read_blobs(&final_oids)?.into_iter();
        for entry in direct {
            let before = entry.original.as_ref().and_then(|_| original_bytes.next());
            let target = finals.get(&entry.path);
            let after = target.and_then(|_| final_bytes.next());
            plan.worktree.push(WorktreeWrite {
                path: entry.path.clone(),
                before,
                before_mode: entry.original.as_ref().map(|_| entry.mode.clone()),
                after,
                mode: target.map_or_else(|| entry.mode.clone(), |(mode, _)| mode.clone()),
            });
        }
        plan.remove_files = state
            .restore
            .iter()
            .filter(|entry| entry.file != entry.path)
            .map(|entry| entry.file.clone())
            .collect();
    }
    let moved_text: Vec<String> = dependents
        .iter()
        .map(|moved| {
            format!(
                "moved branch {}: {}→{}",
                moved.name.trim_start_matches("refs/heads/"),
                short(moved.old.as_deref().unwrap_or("")),
                short(moved.new.as_deref().unwrap_or(""))
            )
        })
        .collect();
    let kept = picks.len() - built.dropped.len();
    let onto_label = format!("{} {}", state.onto_spec, short(&state.onto_oid));
    plan.summary = format!(
        "{} onto {onto_label} ({kept} commits, {} dropped)",
        state.label(),
        built.dropped.len()
    );
    let applied = match session.apply(&plan) {
        Ok(applied) => applied,
        Err(error) => {
            // References already moved (apply removed the state and side files): unpin too.
            let partial = error
                .downcast_ref::<super::HistoryError>()
                .is_some_and(|e| e.message.starts_with("partially applied"));
            if partial {
                unpin(git);
            }
            return Err(error);
        }
    };
    unpin(git);

    let after = super::summary::after(git, state.branch.as_deref(), state.is_head)?;
    let with_upstream = state
        .upstream
        .as_ref()
        .map(|upstream| format!(" with {}", upstream.label))
        .unwrap_or_default();
    let mut result = format!(
        "synced {}{with_upstream} onto {onto_label} ({kept} commits, {} dropped); op {}",
        state.label(),
        built.dropped.len(),
        applied.op
    );
    for index in &built.dropped {
        result.push_str(&format!(
            "\ndropped {} \"{}\" (already upstream)",
            picks[*index].short,
            escape_line(&picks[*index].subject)
        ));
    }
    for line in &moved_text {
        result.push('\n');
        result.push_str(line);
    }
    let text = if quiet {
        result
    } else {
        super::summary::text(&after, &result)
    };
    let uncommitted: Vec<&str> = after
        .changes
        .as_ref()
        .map(|changes| {
            changes
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect()
        })
        .unwrap_or_default();
    let subject_of = |old: &str| -> String {
        picks
            .iter()
            .find(|pick| pick.oid == old)
            .map(|pick| pick.subject.clone())
            .unwrap_or_default()
    };
    let value: Value = json!({
        "op": applied.op,
        "verb": "sync",
        "branch": state.label(),
        "onto": state.onto_spec,
        "old": state.old_tip,
        "new": new_tip,
        "rewritten": built.rewritten.iter().map(|(old, new)|
            json!({"old": old, "new": new, "subject": subject_of(old)})).collect::<Vec<_>>(),
        "dropped": built.dropped.iter().map(|i|
            json!({"id": picks[*i].short, "oid": picks[*i].oid, "subject": picks[*i].subject}))
            .collect::<Vec<_>>(),
        "moved_refs": plan.refs.iter().map(|m|
            json!({"ref": m.name, "old": m.old, "new": m.new})).collect::<Vec<_>>(),
        "uncommitted": uncommitted,
        "log": render::log_json(&after, false),
    });
    Ok(Outcome::with_text(value, text))
}

#[cfg(test)]
mod tests {
    use super::has_markers;

    #[test]
    fn markers_are_found_with_lf_and_crlf_line_endings() {
        for newline in ["\n", "\r\n"] {
            let lines = |parts: &[&str]| parts.join(newline).into_bytes();
            assert!(has_markers(&lines(&["a", "=======", "b"])), "{newline:?}");
            assert!(
                has_markers(&lines(&["<<<<<<< ONTO", "x", ""])),
                "{newline:?}"
            );
            assert!(
                has_markers(&lines(&["x", ">>>>>>> 1a2b topic"])),
                "{newline:?}"
            );
            assert!(!has_markers(&lines(&["a", "======== not a marker", "b"])));
        }
    }
}
