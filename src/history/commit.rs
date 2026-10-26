//! `aisc commit`: new commits from selected hunks, files or content, built with plumbing only.
use super::apply::{join_lines, splice, split_lines};
use super::model::{ChangeSet, FileChange, Kind};
use super::plumbing::{Entry, Git, TreeEdit};
use super::render;
use super::rewrite::{HeadMove, IndexChange, Plan, RefMove, Session, stale};
use super::selector::{self, FileSel, Selection};
use super::view;
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use crate::store::Kind as ObjectKind;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

struct Group {
    message: String,
    /// `None` selects everything (`--all`).
    tokens: Option<Vec<String>>,
}

fn parse_groups(args: &cli::Commit) -> Result<Vec<Group>> {
    if args.opts.branch.is_some() {
        return Err(fail_hint(
            Code::Usage,
            "commit works on HEAD",
            "use -b NEW to create a branch for the commit",
        ));
    }
    let mut groups = Vec::new();
    if !args.groups.is_empty() {
        for group in &args.groups {
            let (message, tokens) = group.split_first().expect("clap requires one value");
            if message.trim().is_empty() {
                return Err(fail(Code::Usage, "a commit message must not be empty"));
            }
            if tokens.is_empty() {
                return Err(fail(
                    Code::Usage,
                    format!("--into \"{message}\" needs at least one selector"),
                ));
            }
            groups.push(Group {
                message: message.clone(),
                tokens: Some(tokens.to_vec()),
            });
        }
        return Ok(groups);
    }
    let message = args
        .message
        .clone()
        .filter(|message| !message.trim().is_empty())
        .ok_or_else(|| {
            fail_hint(
                Code::Usage,
                "a commit message is required",
                "use -m MESSAGE",
            )
        })?;
    if args.all {
        groups.push(Group {
            message,
            tokens: None,
        });
    } else if args.selectors.is_empty() {
        return Err(fail_hint(
            Code::Usage,
            "name what to commit",
            "pass selectors (PATH, hunk id, PATH~text) or --all",
        ));
    } else {
        groups.push(Group {
            message,
            tokens: Some(args.selectors.clone()),
        });
    }
    Ok(groups)
}

/// Pre-computed inputs for building tree entries.
struct Context<'a> {
    git: &'a Git,
    changes: &'a ChangeSet,
    base: BTreeMap<String, Entry>,
    hashed: BTreeMap<String, String>,
    old_lines: BTreeMap<String, Vec<Vec<u8>>>,
}

impl Context<'_> {
    fn entry(&self, index: usize, selected: &FileSel) -> Result<Option<Entry>> {
        let file = &self.changes.files[index];
        match selected {
            FileSel::Whole => {
                let Some(mode) = &file.new_mode else {
                    return Ok(None);
                };
                let oid = if file.kind == Kind::ModeOnly {
                    self.base
                        .get(&file.path)
                        .map(|(_, oid)| oid.clone())
                        .ok_or_else(|| stale(&format!("{} is no longer tracked", file.path)))?
                } else {
                    self.hashed
                        .get(&file.path)
                        .cloned()
                        .ok_or_else(|| fail(Code::State, "internal: file was not hashed"))?
                };
                Ok(Some((mode.clone(), oid)))
            }
            FileSel::Blocks(blocks) => {
                let old = &self.old_lines[&file.path];
                let chosen: Vec<_> = blocks.iter().map(|block| &file.blocks[*block]).collect();
                let lines = splice(old, &chosen)
                    .map_err(|error| stale(&format!("{}: {error}", file.path)))?;
                let oid = self
                    .git
                    .write_object(ObjectKind::Blob, &join_lines(&lines))?;
                let mode = file.old_mode.clone().unwrap_or_else(|| "100644".to_owned());
                Ok(Some((mode, oid)))
            }
        }
    }
}

fn merge(into: &mut Selection, more: &Selection) {
    for (file, selected) in &more.files {
        match (into.files.get_mut(file), selected) {
            (Some(FileSel::Whole), _) => {}
            (Some(FileSel::Blocks(existing)), FileSel::Blocks(extra)) => existing.extend(extra),
            _ => {
                into.files.insert(*file, selected.clone());
            }
        }
    }
}

pub(crate) fn run(args: &cli::Commit) -> Result<Outcome> {
    let groups = parse_groups(args)?;
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    if let Some(name) = &args.new_branch {
        git.valid_branch_name(name)?;
        if let Some(existing) = conflicting_branch(&git, name)? {
            return Err(fail_hint(
                Code::State,
                format!("branch '{name}' conflicts with existing branch {existing}"),
                "a branch cannot be both a name and a directory of names; pick another name",
            ));
        }
        if git.ref_oid(&format!("refs/tags/{name}"))?.is_some() {
            eprintln!("warning: refname '{name}' is ambiguous (a tag of that name exists)");
        }
        if git.ref_oid(&format!("refs/heads/{name}"))?.is_some() && !args.existing {
            let hint = if args.onto.is_some() {
                format!("pick another name, or add --existing --onto {name} to commit on top of it")
            } else {
                "pick another name, or use aisc switch".to_owned()
            };
            return Err(fail_hint(
                Code::State,
                format!("branch '{name}' already exists"),
                hint,
            ));
        }
    }
    let session = Session::open(&repo, &git, true, false)?;
    let head = &session.head;
    // With --onto the selection leaves the worktree: any change from here on is concurrent.
    let started = SystemTime::now();
    let (status, changes) = view::read_changes(&git)?;
    if status.oid.as_deref() != Some(head.oid.as_str()) {
        return Err(stale("HEAD changed while aisc was reading"));
    }
    if !status.unmerged.is_empty() {
        return Err(fail(Code::Dirty, "resolve the unmerged paths first"));
    }

    // Selections, one per group; each change may be claimed once.
    let mut selections = Vec::new();
    for group in &groups {
        selections.push(match &group.tokens {
            None => selector::all(&changes)?,
            Some(tokens) => selector::resolve(&git.root, &changes, tokens)?,
        });
    }
    selector::check_disjoint(&changes, &selections)?;
    if args.onto.is_some() {
        let message = groups[0].message.clone();
        return onto(
            args,
            &message,
            (&git, session),
            &changes,
            &selections[0],
            started,
        );
    }

    // Gather everything the entries need with as few processes as possible.
    let mut touched: BTreeSet<usize> = BTreeSet::new();
    let mut partial: BTreeSet<usize> = BTreeSet::new();
    for selection in &selections {
        for (file, selected) in &selection.files {
            touched.insert(*file);
            if matches!(selected, FileSel::Blocks(_)) {
                partial.insert(*file);
            }
        }
    }
    let base_tree = git.commit_tree_of(&head.oid)?;
    let touched_paths: Vec<String> = touched
        .iter()
        .map(|index| changes.files[*index].path.clone())
        .collect();
    let base = git.ls_tree(&base_tree, &touched_paths)?;

    let mut to_hash: Vec<String> = Vec::new();
    for index in &touched {
        let file = &changes.files[*index];
        let whole_somewhere = selections
            .iter()
            .any(|selection| selection.files.get(index) == Some(&FileSel::Whole));
        if whole_somewhere && file.new_mode.is_some() && file.kind != Kind::ModeOnly {
            to_hash.push(file.path.clone());
        }
    }
    // Hashed as `git add` would store them, so this compares with what the listing saw.
    let hashes = super::amend::hash_as_staged(&git, &to_hash)?;
    let mut hashed = BTreeMap::new();
    for (path, oid) in to_hash.iter().zip(hashes) {
        let file = &changes.files[changes.index_of(path).expect("path from the change set")];
        if file.kind == Kind::Modified && file.new_oid.as_deref().is_some_and(|known| known != oid)
        {
            return Err(stale(&format!("{path} changed while aisc was working")));
        }
        hashed.insert(path.clone(), oid);
    }
    let partial_list: Vec<usize> = partial.iter().copied().collect();
    let mut old_oids = Vec::new();
    for index in &partial_list {
        let file = &changes.files[*index];
        old_oids.push(file.old_oid.clone().ok_or_else(|| {
            fail(
                Code::Unsupported,
                format!(
                    "{}: cannot select part of this change; select the whole file",
                    file.path
                ),
            )
        })?);
    }
    let old_blobs = git.read_blobs(&old_oids)?;
    let mut old_lines = BTreeMap::new();
    for (index, blob) in partial_list.iter().zip(old_blobs) {
        let file = &changes.files[*index];
        let lines = split_lines(&blob);
        // Exactness gate: all blocks together must reproduce the worktree blob exactly.
        let every: Vec<_> = file.blocks.iter().collect();
        let rebuilt = splice(&lines, &every)
            .map_err(|error| fail(Code::Unsupported, format!("{}: {error}", file.path)))?;
        let oid = git.blob_id(&join_lines(&rebuilt))?;
        if file.new_oid.as_deref() != Some(oid.as_str()) {
            return Err(fail_hint(
                Code::Unsupported,
                format!("{}: Git filters or line-ending conversion apply", file.path),
                "select the whole file",
            ));
        }
        old_lines.insert(file.path.clone(), lines);
    }
    let context = Context {
        git: &git,
        changes: &changes,
        base,
        hashed,
        old_lines,
    };

    // One tree and commit per group; tree k = HEAD + groups 1..k.
    let mut cumulative = Selection::default();
    let mut previous_tree = base_tree.clone();
    let mut parent = head.oid.clone();
    let mut commits: Vec<(String, String)> = Vec::new(); // (oid, message)
    let mut final_entries: BTreeMap<String, Option<Entry>> = BTreeMap::new();
    for (group, selection) in groups.iter().zip(&selections) {
        merge(&mut cumulative, selection);
        let mut edits = Vec::new();
        for index in selection.files.keys() {
            let file = &changes.files[*index];
            let entry = context.entry(*index, &cumulative.files[index])?;
            final_entries.insert(file.path.clone(), entry.clone());
            edits.push(TreeEdit {
                path: file.path.clone(),
                entry,
            });
        }
        let tree = git.tree_with(&previous_tree, &edits)?;
        if tree == previous_tree {
            return Err(fail(
                Code::Selector,
                format!("the selection for \"{}\" changes nothing", group.message),
            ));
        }
        let commit = git.commit_tree(
            &tree,
            std::slice::from_ref(&parent),
            format!("{}\n", group.message).as_bytes(),
            None,
        )?;
        commits.push((commit.clone(), group.message.clone()));
        parent = commit;
        previous_tree = tree;
    }
    let tip = parent;

    // Plan: refs, HEAD, and the index rule (entries equal to HEAD's follow the new tree).
    let mut plan = Plan {
        verb: "commit".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    plan.index = final_entries
        .iter()
        .filter(|(path, entry)| context.base.get(*path) != entry.as_ref())
        .map(|(path, entry)| IndexChange {
            path: path.clone(),
            from: context.base.get(path).cloned(),
            to: entry.clone(),
        })
        .collect();
    let mut moved_refs = Vec::new();
    let mut head_changed_to = None;
    let target_name = match (&args.new_branch, &head.symbolic) {
        (Some(name), from) => {
            let reference = format!("refs/heads/{name}");
            plan.refs.push(RefMove {
                name: reference.clone(),
                old: None,
                new: Some(tip.clone()),
            });
            plan.head = Some(HeadMove {
                from: from.clone(),
                to: reference.clone(),
            });
            head_changed_to = Some(name.clone());
            moved_refs.push(json!({"ref": reference, "old": Value::Null, "new": tip}));
            name.clone()
        }
        (None, Some(symbolic)) => {
            plan.refs.push(RefMove {
                name: symbolic.clone(),
                old: Some(head.oid.clone()),
                new: Some(tip.clone()),
            });
            moved_refs.push(json!({"ref": symbolic, "old": head.oid, "new": tip}));
            symbolic.trim_start_matches("refs/heads/").to_owned()
        }
        (None, None) => {
            plan.refs.push(RefMove {
                name: "HEAD".to_owned(),
                old: Some(head.oid.clone()),
                new: Some(tip.clone()),
            });
            moved_refs.push(json!({"ref": "HEAD", "old": head.oid, "new": tip}));
            "HEAD (detached)".to_owned()
        }
    };
    let old_tip = head.oid.clone();
    let file_count = cumulative.files.len();
    let oids: Vec<String> = commits.iter().map(|(oid, _)| oid.clone()).collect();
    let ids = git.short_ids(&oids)?;
    plan.summary = commits
        .iter()
        .zip(&ids)
        .map(|((_, message), id)| format!("{id} {}", first_line(message)))
        .collect::<Vec<_>>()
        .join(", ");

    let applied = session.apply(&plan)?;

    let after = super::summary::after(&git, None, true)?;
    let on = if args.new_branch.is_some() {
        format!("new branch {target_name}")
    } else {
        target_name.clone()
    };
    let described = commits
        .iter()
        .zip(&ids)
        .map(|((_, message), id)| format!("{id} \"{}\"", escape_line(first_line(message))))
        .collect::<Vec<_>>();
    let noun = if commits.len() == 1 {
        String::new()
    } else {
        format!("{} commits ", commits.len())
    };
    let result = format!(
        "committed {noun}{} on {on} ({file_count} file{}); op {}",
        described.join(", "),
        if file_count == 1 { "" } else { "s" },
        applied.op
    );
    let text = if args.opts.quiet {
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
    let value = json!({
        "op": applied.op,
        "verb": "commit",
        "branch": target_name,
        "old": old_tip,
        "new": tip,
        "rewritten": [],
        "committed": commits.iter().zip(&ids).map(|((oid, message), id)|
            json!({"id": id, "oid": oid, "subject": first_line(message)})).collect::<Vec<_>>(),
        "moved_refs": moved_refs,
        "head_changed_to": head_changed_to,
        "uncommitted": uncommitted,
        "log": render::log_json(&after, false),
    });
    Ok(Outcome::with_text(value, text))
}

/// An existing branch that `refs/heads/NAME` cannot live beside: `a` for `a/b`, or `a/b` for
/// `a` (Git keeps refs as paths, so a name cannot also be a directory of names).
fn conflicting_branch(git: &Git, name: &str) -> Result<Option<String>> {
    let parts: Vec<&str> = name.split('/').collect();
    for end in 1..parts.len() {
        let parent = format!("refs/heads/{}", parts[..end].join("/"));
        if git.ref_oid(&parent)?.is_some() {
            return Ok(Some(parent));
        }
    }
    let prefix = format!("refs/heads/{name}/");
    let common = git.common_dir()?;
    if !common.join("reftable").exists() {
        // Loose refs below refs/heads/NAME/, then packed ones; read in process.
        let mut stack = vec![common.join("refs/heads").join(name)];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(relative) = path.strip_prefix(common.join("refs/heads")) {
                    return Ok(Some(format!("refs/heads/{}", relative.to_string_lossy())));
                }
            }
        }
        let packed = std::fs::read_to_string(common.join("packed-refs")).unwrap_or_default();
        return Ok(packed
            .lines()
            .filter_map(|line| line.split_once(' ').map(|(_, name)| name))
            .find(|reference| reference.starts_with(&prefix))
            .map(str::to_owned));
    }
    let found = git.line(&["for-each-ref", "--count=1", "--format=%(refname)", &prefix])?;
    Ok((!found.is_empty()).then_some(found))
}

fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// `base`'s tree with `selection` (this worktree's changes against `head`) applied to it, each
/// block placed where the diff from BASE to HEAD puts it on BASE's version, plus the verified
/// worktree blob ids a removal needs. Refuses, writing nothing, when a change cannot be placed.
pub(crate) struct Placed {
    pub base_tree: String,
    pub tree: String,
    pub hashed: BTreeMap<String, String>,
}

pub(crate) fn tree_with_selection(
    git: &Git,
    changes: &ChangeSet,
    selection: &Selection,
    head_oid: &str,
    base: &str,
    rev: &str,
    hint: &str,
) -> Result<Placed> {
    // The selection as each file's entry on BASE.
    let head_tree = git.commit_tree_of(head_oid)?;
    let base_tree = git.commit_tree_of(base)?;
    let paths: Vec<String> = selection
        .files
        .keys()
        .map(|index| changes.files[*index].path.clone())
        .collect();
    let at_head = git.ls_tree(&head_tree, &paths)?;
    let at_base = git.ls_tree(&base_tree, &paths)?;
    let to_hash: Vec<String> = selection
        .files
        .iter()
        .map(|(index, selected)| (&changes.files[*index], selected))
        .filter(|(file, selected)| {
            **selected == FileSel::Whole && file.new_mode.is_some() && file.kind != Kind::ModeOnly
        })
        .map(|(file, _)| file.path.clone())
        .collect();
    let hashed: BTreeMap<String, String> = to_hash
        .iter()
        .cloned()
        .zip(super::amend::hash_as_staged(git, &to_hash)?)
        .collect();
    for (path, oid) in &hashed {
        let file = &changes.files[changes.index_of(path).expect("path from the change set")];
        if file.kind == Kind::Modified && file.new_oid.as_deref().is_some_and(|known| known != oid)
        {
            return Err(stale(&format!("{path} changed while aisc was working")));
        }
    }
    // Modified files: HEAD's and BASE's versions, and how BASE became HEAD where they differ.
    let modified: Vec<&FileChange> = selection
        .files
        .keys()
        .map(|index| &changes.files[*index])
        .filter(|file| file.kind == Kind::Modified)
        .collect();
    let differ: Vec<String> = modified
        .iter()
        .filter(|file| at_base.contains_key(&file.path))
        .filter(|file| at_base.get(&file.path) != at_head.get(&file.path))
        .map(|file| file.path.clone())
        .collect();
    let mut steps: super::amend::Recorded = vec![BTreeMap::new(), BTreeMap::new()];
    if !differ.is_empty() {
        for file in super::diff::parse_patch(&git.tree_patch(&base_tree, &head_tree, &differ)?)? {
            let blocks = match file.kind {
                Kind::Modified if !file.binary => Some(file.blocks),
                Kind::ModeOnly => Some(Vec::new()),
                _ => None,
            };
            steps[1].insert(file.path, blocks);
        }
    }
    let mut versions: Vec<String> = modified
        .iter()
        .filter_map(|file| at_head.get(&file.path).map(|(_, oid)| oid.clone()))
        .collect();
    versions.extend(
        differ
            .iter()
            .filter_map(|path| at_base.get(path).map(|(_, oid)| oid.clone())),
    );
    let versions: BTreeSet<String> = versions.into_iter().collect();
    let versions: Vec<String> = versions.into_iter().collect();
    let blobs: BTreeMap<&String, Vec<u8>> =
        versions.iter().zip(git.read_blobs(&versions)?).collect();

    let mut edits = Vec::new();
    for (index, selected) in &selection.files {
        let file = &changes.files[*index];
        let refuse = |why: String| {
            fail_hint(
                Code::Conflict,
                format!("{}: {why}; nothing was changed", file.path),
                hint,
            )
        };
        let on_head = at_head.get(&file.path);
        let on_base = at_base.get(&file.path);
        let new_mode = file.new_mode.clone().unwrap_or_else(|| "100644".to_owned());
        let entry = match file.kind {
            Kind::Modified => {
                let (base_mode, base_oid) =
                    on_base.ok_or_else(|| refuse(format!("it does not exist on {rev}")))?;
                let (head_mode, head_oid) =
                    on_head.ok_or_else(|| stale(&format!("{} is no longer tracked", file.path)))?;
                let whole = *selected == FileSel::Whole;
                let head_lines = split_lines(&blobs[head_oid]);
                // Exact blocks: the listed ones, or for a whole file whose listed blocks do not
                // rebuild the stored blob (filters), the diff of HEAD's blob and that blob.
                let exact = super::amend::rebuilds(
                    git,
                    &head_lines,
                    &file.blocks,
                    file.new_oid.as_deref(),
                )?;
                let all = if exact {
                    file.blocks.clone()
                } else if whole {
                    super::amend::stored_blocks(git, &head_tree, file, &hashed[&file.path])?.blocks
                } else {
                    return Err(fail_hint(
                        Code::Unsupported,
                        format!("{}: Git filters or line-ending conversion apply", file.path),
                        "select the whole file",
                    ));
                };
                let chosen: Vec<(usize, &super::model::Block)> = if whole {
                    all.iter().enumerate().collect()
                } else {
                    super::amend::selected_blocks(file, selected)
                        .into_iter()
                        .map(|block| (block, &all[block]))
                        .collect()
                };
                let mode = if whole && file.mode_changed() {
                    if base_mode != head_mode {
                        return Err(refuse(format!("its mode differs on {rev}")));
                    }
                    new_mode
                } else {
                    base_mode.clone()
                };
                // Each block goes where the diff from BASE to HEAD puts it on BASE's version.
                let mut placed = Vec::new();
                for (number, block) in chosen {
                    let old_pos = super::amend::follow(
                        &steps,
                        &file.path,
                        [1],
                        super::amend::Way::Back,
                        block.old_pos,
                        block.removed.len(),
                    )
                    .map_err(|_| {
                        refuse(format!(
                            "hunk {} changes lines that differ on {rev}",
                            super::amend::hunk_name(file, number)
                        ))
                    })?;
                    placed.push(super::model::Block {
                        old_pos,
                        ..block.clone()
                    });
                }
                let base_lines = if on_base == on_head {
                    head_lines
                } else {
                    split_lines(&blobs[base_oid])
                };
                let refs: Vec<&super::model::Block> = placed.iter().collect();
                let lines = splice(&base_lines, &refs)
                    .map_err(|_| refuse(format!("it does not apply onto {rev}")))?;
                Some((
                    mode,
                    git.write_object(ObjectKind::Blob, &join_lines(&lines))?,
                ))
            }
            Kind::Untracked | Kind::Added => {
                if on_base.is_some() {
                    return Err(refuse(format!("it already exists on {rev}")));
                }
                Some((new_mode, hashed[&file.path].clone()))
            }
            Kind::Deleted => {
                if on_base != on_head {
                    return Err(refuse(format!("it differs on {rev}")));
                }
                None
            }
            Kind::ModeOnly => {
                let (_, oid) = on_base
                    .filter(|(_, oid)| on_head.map(|(_, head)| head) == Some(oid))
                    .ok_or_else(|| refuse(format!("it differs on {rev}")))?;
                Some((new_mode, oid.clone()))
            }
            Kind::Binary | Kind::TypeChange | Kind::Symlink => {
                if on_base != on_head {
                    return Err(refuse(format!("it differs on {rev}")));
                }
                Some((new_mode, hashed[&file.path].clone()))
            }
        };
        edits.push(TreeEdit {
            path: file.path.clone(),
            entry,
        });
    }
    let tree = git.tree_with(&base_tree, &edits)?;
    Ok(Placed {
        base_tree,
        tree,
        hashed,
    })
}

fn branch_name(symbolic: Option<&str>) -> &str {
    symbolic.map_or("HEAD", |name| name.trim_start_matches("refs/heads/"))
}

/// `commit -b NEW --onto BASE`: NEW becomes BASE plus one commit holding the selection, built
/// with plumbing only; HEAD never moves. Unless `--keep`, the selection then leaves the worktree
/// and index in the same operation (as if committed on NEW and switched back), so `aisc undo`
/// deletes NEW and puts the changes back.
fn onto(
    args: &cli::Commit,
    message: &str,
    (git, session): (&Git, Session),
    changes: &ChangeSet,
    selection: &Selection,
    started: SystemTime,
) -> Result<Outcome> {
    let rev = args.onto.as_deref().expect("--onto");
    let name = args
        .new_branch
        .as_deref()
        .expect("clap requires -b with --onto");
    let (head_oid, head_symbolic) = (session.head.oid.clone(), session.head.symbolic.clone());
    let base = git.resolve_commit(rev)?.ok_or_else(|| {
        fail_hint(
            Code::Selector,
            format!("unknown revision '{rev}'"),
            "--onto names the branch or commit the new branch starts from",
        )
    })?;
    let reference = format!("refs/heads/{name}");
    let existing = git.ref_oid(&reference)?;
    if let Some(tip) = &existing {
        if head_symbolic.as_deref() == Some(reference.as_str()) {
            return Err(fail_hint(
                Code::Usage,
                format!("'{name}' is the checked-out branch"),
                "commit without --onto",
            ));
        }
        if *tip != base {
            return Err(fail_hint(
                Code::Usage,
                format!("with --existing, --onto must be '{name}' itself"),
                format!("aisc commit -b {name} --existing --onto {name} ..."),
            ));
        }
        if git.checked_out()?.contains(&reference) {
            return Err(fail(
                Code::State,
                format!("branch '{name}' is checked out in another worktree"),
            ));
        }
    }

    let placed = tree_with_selection(
        git,
        changes,
        selection,
        &head_oid,
        &base,
        rev,
        &format!(
            "commit it on {} first, or base the branch on another commit",
            branch_name(head_symbolic.as_deref())
        ),
    )?;
    let (base_tree, tree, hashed) = (placed.base_tree, placed.tree, placed.hashed);
    if tree == base_tree {
        return Err(fail(
            Code::Selector,
            format!("the selection changes nothing on {rev}"),
        ));
    }
    let commit = git.commit_tree(
        &tree,
        std::slice::from_ref(&base),
        format!("{message}\n").as_bytes(),
        None,
    )?;

    // One operation: the branch, and (unless --keep) the selection leaving worktree and index.
    let mut plan = Plan {
        verb: "commit".to_owned(),
        argv: std::env::args().skip(1).collect(),
        ..Plan::default()
    };
    plan.refs.push(RefMove {
        name: reference.clone(),
        old: existing.clone(),
        new: Some(commit.clone()),
    });
    let removed = if args.keep {
        Vec::new()
    } else {
        let removal =
            super::discard::removal(git, changes, selection, &head_oid, (started, &hashed))?;
        plan.worktree = removal.worktree;
        plan.index = removal.index;
        removal.paths
    };
    let id = git
        .short_ids(std::slice::from_ref(&commit))?
        .pop()
        .unwrap_or_default();
    plan.summary = format!("{id} {} on {name}", first_line(message));
    super::discard::ensure_unchanged(git, &plan.worktree)?;
    let applied = session.apply(&plan)?;

    let noun = if existing.is_some() {
        "branch"
    } else {
        "new branch"
    };
    let text = format!(
        "{noun} {name}: {id} \"{}\"",
        escape_line(first_line(message))
    );
    let mut value = json!({
        "op": applied.op,
        "verb": "commit",
        "branch": name,
        "onto": rev,
        "base": base,
        "old": existing,
        "new": commit,
        "rewritten": [],
        "committed": [{"id": id, "oid": commit, "subject": first_line(message)}],
        "moved_refs": [{"ref": reference, "old": existing, "new": commit}],
        "head_changed_to": Value::Null,
        "kept": args.keep,
        "removed": removed,
    });
    if super::summary::full() {
        // JSON gets the full state after the operation; text stays one line.
        let after = super::summary::after(git, None, true)?;
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
        value["uncommitted"] = json!(uncommitted);
        value["log"] = render::log_json(&after, false);
    }
    Ok(Outcome::with_text(value, text))
}
