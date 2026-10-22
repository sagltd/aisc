//! `aisc commit`: new commits from selected hunks, files or content, built with plumbing only.
use super::apply::{join_lines, splice, split_lines};
use super::model::{ChangeSet, Kind};
use super::plumbing::{Entry, Git, TreeEdit};
use super::render;
use super::rewrite::{HeadMove, IndexChange, Plan, RefMove, Session, stale};
use super::selector::{self, FileSel, Selection};
use super::view;
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
                let oid = self.git.write_blob(&join_lines(&lines), true)?;
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
        if git.ref_oid(&format!("refs/heads/{name}"))?.is_some() {
            return Err(fail_hint(
                Code::State,
                format!("branch '{name}' already exists"),
                "pick another name, or use aisc switch",
            ));
        }
    }
    let session = Session::open(&repo, &git, true, false)?;
    let head = &session.head;
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
        let oid = git.write_blob(&join_lines(&rebuilt), true)?;
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
    let ids = {
        let mut arguments = vec![
            "log".to_owned(),
            "--no-walk=unsorted".to_owned(),
            "--abbrev=4".to_owned(),
            "--format=%h".to_owned(),
        ];
        arguments.extend(commits.iter().map(|(oid, _)| oid.clone()));
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        git.line(&arguments)?
    };
    let ids: Vec<String> = ids.lines().map(str::to_owned).collect();
    plan.summary = commits
        .iter()
        .zip(&ids)
        .map(|((_, message), id)| format!("{id} {}", first_line(message)))
        .collect::<Vec<_>>()
        .join(", ");

    let applied = session.apply(&plan)?;

    let after = view::read(&git, None, 20, true)?;
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
        format!("{result}\n{}", render::compact_text(&after))
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

fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}
