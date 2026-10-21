//! `aisc squash --into MESSAGE COMMIT COMMIT... [--into ...]`: adjacent commits become one commit
//! with the newest member's tree, the oldest member's author, and later commits re-parented.
use super::plumbing::Git;
use super::reorder::{
    LIMIT, Report, finish_rewrite, first_line, linear_range, locate, outcome, require_linear,
    short_ids,
};
use super::rewrite::Session;
use super::view::{self, CommitInfo};
use super::{Code, Outcome, fail, fail_hint};
use crate::cli;
use crate::git::Repo;
use crate::output::escape_line;
use anyhow::Result;

/// A group of adjacent commits, as inclusive positions in the oldest-first range.
#[derive(Debug, PartialEq, Eq)]
struct Group {
    message: String,
    start: usize,
    end: usize,
}

/// Checks that `positions` (any order, no duplicates) form one adjacent run; returns its bounds.
fn adjacent(range: &[CommitInfo], positions: &[usize]) -> Result<(usize, usize)> {
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    for pair in sorted.windows(2) {
        if pair[1] != pair[0] + 1 {
            let between = &range[pair[0] + 1];
            return Err(fail_hint(
                Code::Selector,
                format!(
                    "commits {} and {} are not adjacent: {} \"{}\" sits between them",
                    range[pair[0]].short,
                    range[pair[1]].short,
                    between.short,
                    escape_line(&between.subject)
                ),
                "move them together first: aisc move COMMIT… --after COMMIT",
            ));
        }
    }
    Ok((sorted[0], sorted[sorted.len() - 1]))
}

fn parse_groups(
    git: &Git,
    view: &view::View,
    range: &[CommitInfo],
    raw: &[Vec<String>],
) -> Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    let mut claimed: Vec<usize> = Vec::new();
    for group in raw {
        let (message, members) = group.split_first().expect("clap requires three values");
        if members.len() < 2 {
            return Err(fail(
                Code::Usage,
                format!("--into \"{message}\" needs at least 2 commits to squash"),
            ));
        }
        let mut positions = Vec::new();
        for spec in members {
            let position = locate(git, view, range, spec)?;
            if claimed.contains(&position) || positions.contains(&position) {
                return Err(fail(
                    Code::Usage,
                    format!("{} is named twice", range[position].short),
                ));
            }
            positions.push(position);
        }
        let (start, end) = adjacent(range, &positions)?;
        claimed.extend(&positions);
        groups.push(Group {
            message: message.clone(),
            start,
            end,
        });
    }
    groups.sort_by_key(|group| group.start);
    Ok(groups)
}

pub(crate) fn run(args: &cli::Squash) -> Result<Outcome> {
    let raw = &args.groups;
    if raw.is_empty() {
        return Err(fail(Code::Usage, "use --into MESSAGE COMMIT COMMIT..."));
    }
    let repo = Repo::discover()?;
    let git = Git::from_repo(&repo);
    let session = Session::open(&repo, &git, false, false)?;
    let branch_option = args.opts.branch.as_deref();
    let view = view::read(&git, branch_option, LIMIT, false)?;
    let range = linear_range(&view);
    let groups = parse_groups(&git, &view, &range, raw)?;
    let first = groups[0].start;
    require_linear(&range, first)?;

    let mut parent: Option<String> = range[first].parents.first().cloned();
    let mut rewritten: Vec<(String, String)> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();
    let mut index = first;
    while index < range.len() {
        if let Some(group) = groups.iter().find(|group| group.start == index) {
            let members = &range[group.start..=group.end];
            let metas = members
                .iter()
                .map(|member| git.read_commit_meta(&member.oid))
                .collect::<Result<Vec<_>>>()?;
            let message = if group.message.is_empty() {
                let parts: Vec<String> = metas
                    .iter()
                    .map(|meta| String::from_utf8_lossy(&meta.message).trim_end().to_owned())
                    .collect();
                parts.join("\n\n")
            } else {
                group.message.clone()
            };
            let newest = &members[members.len() - 1];
            let commit = git.commit_tree(
                &newest.tree,
                parent.as_slice(),
                message.as_bytes(),
                Some(&metas[0]),
            )?;
            for member in members {
                rewritten.push((member.oid.clone(), commit.clone()));
            }
            summaries.push(format!(
                "{} -> \"{}\"",
                members.len(),
                escape_line(first_line(&message))
            ));
            created.push(commit.clone());
            parent = Some(commit);
            index = group.end + 1;
        } else {
            let commit = &range[index];
            if commit.parents.first() == parent.as_ref() {
                parent = Some(commit.oid.clone());
            } else {
                let meta = git.read_commit_meta(&commit.oid)?;
                let replacement =
                    git.commit_tree(&commit.tree, parent.as_slice(), &meta.message, Some(&meta))?;
                rewritten.push((commit.oid.clone(), replacement.clone()));
                parent = Some(replacement);
            }
            index += 1;
        }
    }
    let new_tip = parent.expect("a group produced a commit");
    let new_tree = git.commit_tree_of(&new_tip)?;
    if new_tree != range[range.len() - 1].tree {
        return Err(fail(
            Code::State,
            "internal: squash changed the branch's final tree; nothing was changed",
        ));
    }
    let squashed: usize = groups.iter().map(|group| group.end - group.start + 1).sum();
    let finished = finish_rewrite(
        &git,
        session,
        &view,
        "squash",
        format!("squashed {squashed} commits into {}", groups.len()),
        &new_tip,
        rewritten.clone(),
    )?;
    let ids = short_ids(&git, &created)?;
    let described: Vec<String> = ids
        .iter()
        .zip(&summaries)
        .map(|(id, summary)| format!("{id} ({summary})"))
        .collect();
    let line = format!(
        "squashed {squashed} commits into {}: {}",
        if groups.len() == 1 {
            "1 commit".to_owned()
        } else {
            format!("{} commits", groups.len())
        },
        described.join(", ")
    );
    let report = Report {
        branch_option,
        quiet: args.opts.quiet,
        verb: "squash",
        line,
        old: &view.tip,
        new: &new_tip,
        rewritten: &rewritten,
    };
    outcome(&git, &report, &finished)
}
