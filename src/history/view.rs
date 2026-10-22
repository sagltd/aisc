//! Branch view: target branch, base, upstream, range commits and uncommitted changes.
use super::diff::{self, Status};
use super::model::ChangeSet;
use super::plumbing::Git;
use super::{Code, fail, fail_hint};
use crate::git::text;
use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub(crate) struct CommitInfo {
    pub oid: String,
    pub short: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub files: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct BranchRef {
    pub name: String,
    pub oid: String,
}

#[derive(Clone, Debug)]
pub(crate) struct BaseInfo {
    pub label: String,
    pub refname: String,
    pub oid: String,
    /// Commits on the target that the base lacks.
    pub ahead: usize,
    /// Commits on the base that the target lacks.
    pub behind: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct UpstreamInfo {
    pub label: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug)]
pub(crate) struct View {
    /// Branch name; `None` for a detached HEAD.
    pub branch: Option<String>,
    pub tip: String,
    /// The view target is the checked-out HEAD.
    pub is_head: bool,
    pub base: Option<BaseInfo>,
    pub upstream: Option<UpstreamInfo>,
    /// Newest first, at most `limit`.
    pub commits: Vec<CommitInfo>,
    pub total: usize,
    pub status: Option<Status>,
    pub changes: Option<ChangeSet>,
    pub sync_paused: Option<usize>,
    pub locals: Vec<BranchRef>,
    /// Commits this process wrote (the post-mutation summary lists those in the range).
    pub created: std::collections::HashSet<String>,
}

impl View {
    pub fn label(&self) -> String {
        self.branch
            .clone()
            .unwrap_or_else(|| "HEAD (detached)".to_owned())
    }

    /// Resolves a commit reference: hex id in range, exact unique subject, or any revision.
    pub fn resolve_opt(&self, git: &Git, spec: &str) -> Result<Option<String>> {
        if spec.len() >= 4 && spec.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let lower = spec.to_ascii_lowercase();
            let matches: Vec<&CommitInfo> = self
                .commits
                .iter()
                .filter(|commit| commit.oid.starts_with(&lower))
                .collect();
            match matches.len() {
                1 => return Ok(Some(matches[0].oid.clone())),
                0 => {}
                _ => {
                    return Err(fail(
                        Code::Selector,
                        format!("ambiguous commit id '{spec}'; use more characters"),
                    ));
                }
            }
        }
        let matches: Vec<&CommitInfo> = self
            .commits
            .iter()
            .filter(|commit| commit.subject == spec)
            .collect();
        match matches.len() {
            1 => return Ok(Some(matches[0].oid.clone())),
            0 => {}
            _ => {
                let candidates = matches
                    .iter()
                    .map(|commit| commit.short.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(fail_hint(
                    Code::Selector,
                    format!("subject '{spec}' matches several commits: {candidates}"),
                    "use a commit id",
                ));
            }
        }
        git.resolve_commit(spec)
    }

    pub fn resolve(&self, git: &Git, spec: &str) -> Result<String> {
        self.resolve_opt(git, spec)?.ok_or_else(|| {
            fail_hint(
                Code::Selector,
                format!("unknown commit '{spec}'"),
                "run aisc log for ids and subjects",
            )
        })
    }
}

pub(crate) struct Refs {
    pub locals: Vec<BranchRef>,
    remote_head: Option<String>,
    remotes: Vec<BranchRef>,
}

/// Local and remote-tracking branches (`for-each-ref refs/heads refs/remotes`), read in process
/// when possible.
pub(crate) fn read_refs(git: &Git) -> Result<Refs> {
    let listed = match git.fast(|store| store.list_refs(&["refs/heads/", "refs/remotes/"])) {
        Some(listed) => listed,
        None => {
            let output = git.run(&[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(symref)",
                "refs/heads",
                "refs/remotes",
            ])?;
            let mut listed = Vec::new();
            for line in text(&output)?.lines() {
                let mut fields = line.split('\0');
                if let (Some(name), Some(oid), Some(symref)) =
                    (fields.next(), fields.next(), fields.next())
                {
                    listed.push((name.to_owned(), oid.to_owned(), symref.to_owned()));
                }
            }
            listed
        }
    };
    let mut refs = Refs {
        locals: Vec::new(),
        remote_head: None,
        remotes: Vec::new(),
    };
    for (name, oid, symref) in &listed {
        let (name, oid, symref) = (name.as_str(), oid.as_str(), symref.as_str());
        if let Some(local) = name.strip_prefix("refs/heads/") {
            refs.locals.push(BranchRef {
                name: local.to_owned(),
                oid: oid.to_owned(),
            });
        } else if name == "refs/remotes/origin/HEAD" {
            if let Some(target) = symref.strip_prefix("refs/remotes/origin/") {
                refs.remote_head = Some(target.to_owned());
            }
        } else if let Some(remote) = name.strip_prefix("refs/remotes/") {
            refs.remotes.push(BranchRef {
                name: remote.to_owned(),
                oid: oid.to_owned(),
            });
        }
    }
    Ok(refs)
}

/// Default branch: origin/HEAD target, else the first of main, master, trunk.
pub(crate) fn find_base(refs: &Refs) -> Option<(String, String, String)> {
    let local = |name: &str| refs.locals.iter().find(|branch| branch.name == name);
    let candidate = refs.remote_head.clone().or_else(|| {
        ["main", "master", "trunk"]
            .into_iter()
            .find(|name| local(name).is_some())
            .map(str::to_owned)
    })?;
    if let Some(branch) = local(&candidate) {
        return Some((
            candidate.clone(),
            format!("refs/heads/{candidate}"),
            branch.oid.clone(),
        ));
    }
    let remote = format!("origin/{candidate}");
    refs.remotes
        .iter()
        .find(|branch| branch.name == remote)
        .map(|branch| {
            (
                remote.clone(),
                format!("refs/remotes/{remote}"),
                branch.oid.clone(),
            )
        })
}

pub(crate) fn parse_log(output: &[u8]) -> Result<Vec<CommitInfo>> {
    let mut commits = Vec::new();
    for chunk in text(output)?
        .split('\u{1}')
        .filter(|chunk| !chunk.is_empty())
    {
        let mut lines = chunk.lines();
        let header = lines.next().context("empty log record")?;
        let mut fields = header.split('\u{1f}');
        let oid = fields.next().context("log oid")?.to_owned();
        let short = fields.next().context("log short id")?.to_owned();
        let tree = fields.next().context("log tree")?.to_owned();
        let parents = fields
            .next()
            .context("log parents")?
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let subject = fields.next().unwrap_or_default().to_owned();
        let files = lines
            .filter(|line| !line.is_empty())
            .map(|line| {
                if line.starts_with('"') {
                    String::from_utf8(diff::unquote(line.as_bytes()).unwrap_or_default())
                        .unwrap_or_else(|_| line.to_owned())
                } else {
                    line.to_owned()
                }
            })
            .collect();
        commits.push(CommitInfo {
            oid,
            short,
            tree,
            parents,
            subject,
            files,
        });
    }
    Ok(commits)
}

/// Longest first-parent line walked in process before Git is asked instead.
const WALK_CAP: usize = 5_000;

/// The commits shown, and `(ahead, behind)` when exact.
pub(crate) type Walked = (Vec<CommitInfo>, Option<(usize, usize)>);

/// The range `git log --first-parent TIP ^BASE -nLIMIT --name-only` prints, read in process.
/// Exact whenever TIP's first-parent line reaches BASE's tip: nothing on that line above it can be
/// reachable from BASE. Without a base it is simply the newest LIMIT commits. The second value
/// is `(ahead, behind)` when that is exact too (no merges on the line, so nothing else counts).
/// `None` hands the range to Git (and `rev-list --count`).
pub(crate) fn walk(git: &Git, tip: &str, base: Option<&str>, limit: usize) -> Option<Walked> {
    git.fast(|store| {
        let mut commits = Vec::new();
        let mut current = Some(tip.to_owned());
        let mut steps = 0;
        let mut merges = false;
        loop {
            let Some(oid) = current else {
                if base.is_some() {
                    anyhow::bail!("the first-parent line does not reach the base");
                }
                break;
            };
            if Some(oid.as_str()) == base || (base.is_none() && commits.len() == limit) {
                break;
            }
            steps += 1;
            if steps > WALK_CAP {
                anyhow::bail!("long range");
            }
            let commit = store.commit(&oid)?;
            if commit.has_encoding() {
                anyhow::bail!("re-encoded message");
            }
            merges |= commit.parents.len() > 1;
            if commits.len() < limit {
                let parent_tree = match commit.parents.first() {
                    Some(parent) => Some(store.commit(parent)?.tree),
                    None => None,
                };
                let mut changed = Vec::new();
                store.diff_trees(parent_tree.as_deref(), Some(&commit.tree), "", &mut changed)?;
                commits.push(CommitInfo {
                    short: store.abbrev(&oid, 4)?,
                    tree: commit.tree.clone(),
                    parents: commit.parents.clone(),
                    subject: commit.subject().context("non-UTF-8 message")?,
                    files: changed.into_iter().map(|(path, _, _)| path).collect(),
                    oid: oid.clone(),
                });
            }
            current = commit.parents.first().cloned();
        }
        Ok((commits, (base.is_some() && !merges).then_some((steps, 0))))
    })
}

/// Reads only HEAD's status and uncommitted changes (two parallel Git processes).
pub(crate) fn read_changes(git: &Git) -> Result<(Status, ChangeSet)> {
    let (status, patch) = std::thread::scope(|scope| {
        let status = scope.spawn(|| {
            git.run(&[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
            ])
        });
        let patch = scope.spawn(|| git.run(&diff::worktree_diff_args()));
        (status.join(), patch.join())
    });
    let panicked = || anyhow::anyhow!("a Git reader panicked");
    let status = diff::parse_status(&status.map_err(|_| panicked())??)?;
    let patch = patch.map_err(|_| panicked())??;
    let changes = diff::build_changes(&git.root, &patch, &status.untracked)?;
    if let Some(oid) = &status.oid {
        super::summary::note_changes(git, oid, &changes);
    }
    Ok((status, changes))
}

/// Reads the branch view. `branch` selects another local branch (no uncommitted changes then).
pub(crate) fn read(
    git: &Git,
    branch: Option<&str>,
    limit: usize,
    with_changes: bool,
) -> Result<View> {
    let (refs, head, status_output, patch) = std::thread::scope(|scope| {
        let refs = scope.spawn(|| read_refs(git));
        let head = scope.spawn(|| git.head_state());
        let status = scope.spawn(|| {
            if with_changes {
                git.run(&[
                    "status",
                    "--porcelain=v2",
                    "--branch",
                    "-z",
                    "--untracked-files=all",
                ])
                .map(Some)
            } else {
                Ok(None)
            }
        });
        let patch = scope.spawn(|| {
            if with_changes {
                git.run(&diff::worktree_diff_args()).map(Some)
            } else {
                Ok(None)
            }
        });
        (refs.join(), head.join(), status.join(), patch.join())
    });
    let panicked = || anyhow::anyhow!("a Git reader panicked");
    let refs = refs.map_err(|_| panicked())??;
    let head = head.map_err(|_| panicked())??;
    let status_output = status_output.map_err(|_| panicked())??;
    let patch = patch.map_err(|_| panicked())??;

    let head_branch = head
        .symbolic
        .as_deref()
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    let (target, tip, is_head) = match branch {
        Some(name) if Some(name) != head_branch.as_deref() => {
            let found = refs
                .locals
                .iter()
                .find(|candidate| candidate.name == name)
                .ok_or_else(|| {
                    fail_hint(
                        Code::Selector,
                        format!("unknown branch '{name}'"),
                        "use a local branch name",
                    )
                })?;
            (Some(name.to_owned()), found.oid.clone(), false)
        }
        _ => (head_branch.clone(), head.oid.clone(), true),
    };
    let base = find_base(&refs).filter(|(label, _, _)| {
        // The base branch itself has no base.
        target.as_deref() != Some(label.as_str())
    });
    let walked = walk(
        git,
        &tip,
        base.as_ref().map(|(_, _, oid)| oid.as_str()),
        limit,
    );
    let tip_argument = tip.clone();
    let limit_argument = format!("-n{limit}");
    let (counts, log) = std::thread::scope(|scope| {
        let counts = scope.spawn(|| -> Result<Option<(usize, usize)>> {
            let Some((_, refname, _)) = &base else {
                return Ok(None);
            };
            if let Some((_, Some(counts))) = &walked {
                return Ok(Some(*counts));
            }
            let range = format!("{refname}...{tip_argument}");
            let output = git.line(&["rev-list", "--left-right", "--count", &range])?;
            let mut numbers = output.split_whitespace().map(|part| part.parse::<usize>());
            let behind = numbers.next().context("rev-list count")??;
            let ahead = numbers.next().context("rev-list count")??;
            Ok(Some((ahead, behind)))
        });
        let log = scope.spawn(|| {
            if walked.is_some() {
                return Ok(Vec::new());
            }
            let exclude = base.as_ref().map(|(_, refname, _)| format!("^{refname}"));
            let mut args = vec![
                "-c",
                "core.quotePath=false",
                "log",
                "--first-parent",
                "--no-renames",
                "--name-only",
                "--abbrev=4",
                "--format=%x01%H%x1f%h%x1f%T%x1f%P%x1f%s",
                &limit_argument,
                &tip_argument,
            ];
            if let Some(exclude) = &exclude {
                args.push(exclude);
            }
            args.push("--");
            git.run(&args)
        });
        (counts.join(), log.join())
    });
    let counts = counts.map_err(|_| panicked())??;
    let log = log.map_err(|_| panicked())??;
    let commits = match walked {
        Some((commits, _)) => commits,
        None => parse_log(&log)?,
    };
    let base_info = base.map(|(label, refname, oid)| {
        let (ahead, behind) = counts.unwrap_or((0, 0));
        BaseInfo {
            label,
            refname,
            oid,
            ahead,
            behind,
        }
    });
    let total = base_info.as_ref().map_or(commits.len(), |base| base.ahead);

    let mut status = None;
    let mut changes = None;
    let mut upstream = None;
    if is_head && let (Some(raw), Some(patch)) = (status_output, patch) {
        let parsed = diff::parse_status(&raw)?;
        let built = diff::build_changes(&git.root, &patch, &parsed.untracked)?;
        if let Some(oid) = &parsed.oid {
            super::summary::note_changes(git, oid, &built);
        }
        changes = Some(built);
        if let Some(label) = &parsed.upstream {
            upstream = Some(UpstreamInfo {
                label: label.clone(),
                ahead: parsed.ahead,
                behind: parsed.behind,
            });
        }
        status = Some(parsed);
    }
    let sync_paused = std::fs::read(git.dir.join("aisc").join("sync.json"))
        .ok()
        .map(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| value["materialized"].as_array().map(Vec::len))
                .unwrap_or(0)
        });
    Ok(View {
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
        created: super::plumbing::created(&git.dir),
    })
}
