use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "aisc",
    version,
    about = "Task-oriented source control, backed by Git",
    after_help = "Examples:\n  aisc log\n  aisc commit -b topic -m 'Add check' README.md 'src/a.rs~validate'\n  aisc commit -b docs --onto main -m 'Update guide' docs/guide.md\n  aisc undo\n  aisc start fix --in-place\n  aisc start fix --path ../fix --sparse src\n  aisc checkpoint -m 'Fix parser' -- src/parser.rs\n  aisc verify -- cargo test\n  aisc finish\n\nTask verbs default to JSON when redirected; history verbs (log, show, commit, ...) print compact text unless --json or AISC_OUTPUT=json.\nPaths are literal files relative to the repository root.\nSelectors (SEL): PATH | hunk id | PATH:L-R | PATH~text. L-R are new-side line numbers; a deletion counts at the line that follows it (within L..R+1).\nNo automatic push or merge. finish removes the worktree `start --path` created (its branch stays; --keep opts out); aisc gc lists leftovers.\nConfigured Git hooks/signing run normally for checkpoint."
)]
pub(crate) struct Cli {
    /// Emit stable, ANSI-free JSON, including errors and help
    #[arg(long, global = true, conflicts_with = "human")]
    pub json: bool,
    /// Emit readable text even when redirected
    #[arg(long, global = true, conflicts_with = "json")]
    pub human: bool,
    /// Color human output; NO_COLOR disables colors in every mode
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: Color,
    #[command(subcommand)]
    pub command: Option<Operation>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Subcommand)]
pub(crate) enum Operation {
    /// Start a task in this checkout or a new isolated worktree
    Start(Start),
    /// Inspect branch, changes, conflicts and staged/unstaged line counts
    Context {
        /// Maximum entries per list; omitted counts remain explicit
        #[arg(long, default_value = "30", value_parser = bounded::<1_000>)]
        limit: usize,
    },
    /// Show tracked changes against HEAD, optionally narrowed to literal files
    Diff {
        #[arg(long, default_value = "8192", value_parser = bounded::<1_048_576>)]
        max_bytes: usize,
        #[arg(last = true, value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Commit whole selected files, preserving unrelated staging
    Checkpoint {
        #[arg(short = 'm', long, value_name = "MESSAGE")]
        message: String,
        #[arg(last = true, required = true, num_args = 1.., value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Run a program directly and bind success to the clean committed candidate
    Verify {
        /// Maximum execution time in seconds; process group is stopped on timeout
        #[arg(long, default_value = "300", value_parser = bounded::<3_600>)]
        timeout: usize,
        #[arg(last = true, required = true, num_args = 1.., allow_hyphen_values = true, value_name = "PROGRAM ARG...")]
        command: Vec<String>,
    },
    /// Return the verified commit/ref for review, without pushing or merging
    Finish {
        /// Keep the worktree created by `start --path` (by default it is removed; the branch stays)
        #[arg(long)]
        keep: bool,
        /// Remove the worktree even if it holds ignored files that are not known to be regenerable
        /// (tool dirs like node_modules/.venv anywhere; build/dist/target/... only at the root or
        /// next to a project manifest), secret-like files or nested repositories. Without it, files
        /// inside installed packages (node_modules, site-packages, .gradle, .dart_tool) are still
        /// deleted without secret-name checks: an accepted trade-off
        #[arg(long, conflicts_with = "keep", aliases = ["delete-ignored-secrets", "delete-nested-repos"])]
        delete_ignored: bool,
    },
    /// Read a bounded GitHub PR/review/check overview using existing gh auth
    Github {
        #[arg(long, default_value = "10", value_parser = bounded::<100>)]
        limit: usize,
    },
    /// Read open merge requests for this branch using existing glab auth
    Gitlab {
        #[arg(long, default_value = "10", value_parser = bounded::<100>)]
        limit: usize,
    },
    /// Branch view: commits (hex ids) plus uncommitted hunks (letter ids)
    Log(Log),
    /// Show hunks of a commit, a range A..B, or the uncommitted changes
    Show(Show),
    /// List local branches, or those whose history contains COMMIT
    Branches(Branches),
    /// Commit selected hunks/files/content as one or several new commits
    Commit(Commit),
    /// Move uncommitted hunks into existing commits
    Amend(Amend),
    /// Revert selected uncommitted changes
    Discard(Discard),
    /// Split a commit into several commits
    Split(Split),
    /// Move part of a commit back to the working tree
    Uncommit(Uncommit),
    /// Squash adjacent commits into groups
    Squash(Squash),
    /// Move or reorder commits, or move them to another branch (--to)
    Move(Move),
    /// Change one commit's message
    Reword(Reword),
    /// Remove commits from the branch, and their changes from the worktree
    Drop(Drop),
    /// Copy commits onto another branch
    Pick(Pick),
    /// Rebase the branch onto its base while keeping uncommitted work
    Sync(Sync),
    /// Integrate branches into a target branch
    Land(Land),
    /// Switch branches carrying uncommitted work
    Switch(Switch),
    /// Undo the latest (or a given) history operation; undoing an undo redoes it
    Undo {
        /// Operation id from `aisc oplog`
        op: Option<String>,
        /// Print only the result line
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Also print the branch's commits after the change
        #[arg(short = 'v', long)]
        stack: bool,
    },
    /// List recorded history operations, newest first
    Oplog {
        #[arg(long, default_value = "10", value_parser = bounded::<1_000>)]
        limit: usize,
    },
    /// List or remove reclaimable aisc worktrees
    Gc(Gc),
    /// Agent setup: the version-control block for AGENTS.md or CLAUDE.md
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum AgentAction {
    /// Print the version-control block to put in an agent's instruction file
    Setup {
        /// Print the block to stdout (aisc never edits instruction files itself)
        #[arg(long, required = true)]
        print: bool,
    },
}

/// Flags shared by every verb that rewrites history.
#[derive(Args, Clone, Debug, Default)]
pub(crate) struct MutationOpts {
    /// Rewrite this branch without checking it out
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Print only the result line
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Also print the branch's commits after the change
    #[arg(short = 'v', long)]
    pub stack: bool,
}

#[derive(Args, Debug)]
pub(crate) struct Log {
    /// Another local or remote-tracking branch to view (`main`, `origin/main`)
    #[arg(value_name = "BRANCH", conflicts_with = "branch")]
    pub target: Option<String>,
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
    #[arg(long, default_value = "20", value_parser = bounded::<1_000>)]
    pub limit: usize,
    /// Omit per-hunk lines of uncommitted changes
    #[arg(long)]
    pub no_hunks: bool,
    /// Only commit ids and subjects, and the uncommitted paths
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Args, Debug)]
pub(crate) struct Branches {
    /// A commit id, exact subject (on any local branch) or revision; only branches containing it are listed
    #[arg(value_name = "COMMIT")]
    pub commit: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct Show {
    /// Optional commit/range first, then selectors (PATH, hunk id, PATH:L-R on new-side lines, PATH~text)
    #[arg(value_name = "REV|A..B|SEL")]
    pub args: Vec<String>,
    /// Include 3 lines of context and do not cap hunk length
    #[arg(long)]
    pub full: bool,
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct Commit {
    #[arg(short = 'm', long, value_name = "MESSAGE")]
    pub message: Option<String>,
    /// Create this branch at the new commit and switch HEAD to it
    #[arg(short = 'b', long = "new-branch", value_name = "NEW")]
    pub new_branch: Option<String>,
    /// Commit onto BASE without switching: alone, BASE is an existing local branch and the commit
    /// goes on top of it; with -b NEW, NEW is built on BASE (a parallel branch). HEAD stays; the
    /// committed changes leave the worktree and index unless --keep. Runs no Git hooks (D3), unlike git switch + commit
    #[arg(long, value_name = "BASE", conflicts_with = "into")]
    pub onto: Option<String>,
    /// With --onto: leave the committed changes in the worktree and index too
    #[arg(long, requires = "onto")]
    pub keep: bool,
    /// With -b NEW --onto NEW: NEW may exist (implied when -b names the --onto branch)
    #[arg(long, requires = "onto")]
    pub existing: bool,
    /// Commit every change, including untracked files that are not ignored
    #[arg(long, conflicts_with_all = ["into", "selectors"])]
    pub all: bool,
    /// Stack: `--into MESSAGE SEL...`, repeatable; each group becomes one commit
    #[arg(long, num_args = 1.., action = clap::ArgAction::Append, value_parser = clap::value_parser!(String), value_name = "ARG", conflicts_with_all = ["message", "selectors"])]
    pub into: Vec<String>,
    /// Occurrences of `--into`, filled from the parsed matches after parsing.
    #[arg(skip)]
    pub groups: Vec<Vec<String>>,
    #[arg(value_name = "SEL")]
    pub selectors: Vec<String>,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Amend {
    /// `--into COMMIT SEL...`, repeatable
    #[arg(long, num_args = 2.., required = true, action = clap::ArgAction::Append, value_parser = clap::value_parser!(String), value_name = "ARG")]
    pub into: Vec<String>,
    /// Occurrences of `--into`, filled from the parsed matches after parsing.
    #[arg(skip)]
    pub groups: Vec<Vec<String>>,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Discard {
    #[arg(value_name = "SEL")]
    pub selectors: Vec<String>,
    /// Revert only changes that differ by whitespace
    #[arg(long)]
    pub whitespace_only: bool,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq, Eq)]
pub(crate) enum Rest {
    #[default]
    Uncommit,
    Keep,
}

#[derive(Args, Debug)]
pub(crate) struct Split {
    pub commit: String,
    /// `--into MESSAGE SEL...`, repeatable
    #[arg(long, num_args = 2.., required = true, action = clap::ArgAction::Append, value_parser = clap::value_parser!(String), value_name = "ARG")]
    pub into: Vec<String>,
    /// Occurrences of `--into`, filled from the parsed matches after parsing.
    #[arg(skip)]
    pub groups: Vec<Vec<String>>,
    #[arg(long, value_enum, default_value = "uncommit")]
    pub rest: Rest,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Uncommit {
    pub commit: String,
    #[arg(value_name = "SEL", required = true)]
    pub selectors: Vec<String>,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Squash {
    /// `--into MESSAGE COMMIT COMMIT...`, repeatable
    #[arg(long, num_args = 3.., required = true, action = clap::ArgAction::Append, value_parser = clap::value_parser!(String), value_name = "ARG")]
    pub into: Vec<String>,
    /// Occurrences of `--into`, filled from the parsed matches after parsing.
    #[arg(skip)]
    pub groups: Vec<Vec<String>>,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("placement").required(true).multiple(false).args(["before", "after", "order", "to"])))]
pub(crate) struct Move {
    #[arg(value_name = "COMMIT", required = true)]
    pub commits: Vec<String>,
    /// Move the commits to the top of this branch (removing them here); with --onto, a new branch
    #[arg(long, value_name = "BRANCH")]
    pub to: Option<String>,
    /// With --to NEW: create NEW at BASE (a branch or commit) and move the commits there
    #[arg(long, value_name = "BASE", requires = "to")]
    pub onto: Option<String>,
    #[arg(long, value_name = "COMMIT")]
    pub before: Option<String>,
    #[arg(long, value_name = "COMMIT")]
    pub after: Option<String>,
    /// COMMITs list the full range, oldest first, in the wanted order
    #[arg(long)]
    pub order: bool,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Reword {
    #[arg(value_name = "COMMIT")]
    pub commit: String,
    /// The new message
    #[arg(short = 'm', long, value_name = "MSG")]
    pub message: String,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Drop {
    #[arg(value_name = "COMMIT", required = true)]
    pub commits: Vec<String>,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Pick {
    #[arg(value_name = "COMMIT", required = true)]
    pub commits: Vec<String>,
    /// Replay onto this existing branch (not checked out)
    #[arg(long, value_name = "BRANCH", conflicts_with = "new_branch")]
    pub to: Option<String>,
    /// Create this branch (at --onto, default the base) and replay there
    #[arg(short = 'b', long = "new-branch", value_name = "NEW")]
    pub new_branch: Option<String>,
    #[arg(long, value_name = "BASE", requires = "new_branch")]
    pub onto: Option<String>,
    /// Resolve COMMIT names against this branch
    #[arg(long, value_name = "BRANCH")]
    pub from: Option<String>,
    /// With -b NEW or --to BRANCH: switch to that branch afterwards, carrying uncommitted changes
    #[arg(long)]
    pub switch: bool,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Sync {
    #[arg(value_name = "ONTO", conflicts_with_all = ["continue_sync", "abort"])]
    pub onto: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub prefer_branch: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub prefer_onto: Vec<String>,
    #[arg(long)]
    pub fetch: bool,
    #[arg(long = "continue", conflicts_with = "abort")]
    pub continue_sync: bool,
    #[arg(long)]
    pub abort: bool,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Land {
    #[arg(value_name = "BRANCH", required = true)]
    pub branches: Vec<String>,
    #[arg(long, value_name = "TARGET")]
    pub into: Option<String>,
    #[arg(long)]
    pub switch: bool,
    /// Delete the landed branches (undo restores them)
    #[arg(long)]
    pub delete: bool,
    #[command(flatten)]
    pub opts: MutationOpts,
}

#[derive(Args, Debug)]
pub(crate) struct Switch {
    pub branch: String,
    #[arg(short = 'c', long)]
    pub create: bool,
    /// Print only the result line
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Also print the branch's commits after the change
    #[arg(short = 'v', long)]
    pub stack: bool,
}

#[derive(Args, Debug)]
pub(crate) struct Gc {
    /// Remove reclaimable worktrees (default is a dry run)
    #[arg(long)]
    pub apply: bool,
    /// Keep worktrees with Git activity within this many hours
    #[arg(long, default_value = "24", value_name = "HOURS")]
    pub min_age: u64,
    /// Also remove worktrees holding ignored files that are not known to be regenerable (tool
    /// dirs like node_modules/.venv anywhere; build/dist/target/... only at the root or next to
    /// a project manifest), secret-like files or nested repositories
    #[arg(long, aliases = ["delete-ignored-secrets", "delete-nested-repos"])]
    pub delete_ignored: bool,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("location").multiple(false).args(["path", "in_place"])))]
pub(crate) struct Start {
    pub name: String,
    /// Complete destination path; must not exist and its parent must exist
    #[arg(long, group = "location", value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Keep this directory and branch; requires a clean committed checkout (the default)
    #[arg(long, group = "location")]
    pub in_place: bool,
    /// Cone-mode sparse directories for a new worktree (requires --path). Like `git
    /// sparse-checkout`, this enables extensions.worktreeConfig in the shared Git config; AISC
    /// says so when it does and never reverts it
    #[arg(long, requires = "path", num_args = 1.., value_name = "DIR")]
    pub sparse: Vec<String>,
}

impl Operation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::Context { .. } => "context",
            Self::Diff { .. } => "diff",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Verify { .. } => "verify",
            Self::Finish { .. } => "finish",
            Self::Github { .. } => "github",
            Self::Gitlab { .. } => "gitlab",
            Self::Log(_) => "log",
            Self::Show(_) => "show",
            Self::Branches(_) => "branches",
            Self::Commit(_) => "commit",
            Self::Amend(_) => "amend",
            Self::Discard(_) => "discard",
            Self::Split(_) => "split",
            Self::Uncommit(_) => "uncommit",
            Self::Squash(_) => "squash",
            Self::Move(_) => "move",
            Self::Reword(_) => "reword",
            Self::Drop(_) => "drop",
            Self::Pick(_) => "pick",
            Self::Sync(_) => "sync",
            Self::Land(_) => "land",
            Self::Switch(_) => "switch",
            Self::Undo { .. } => "undo",
            Self::Oplog { .. } => "oplog",
            Self::Gc(_) => "gc",
            Self::Agent { .. } => "agent",
        }
    }

    /// Fills `groups` (the per-occurrence values of repeatable `--into`) from the parsed matches.
    pub fn attach_groups(&mut self, matches: &clap::ArgMatches) {
        let Some((_, sub)) = matches.subcommand() else {
            return;
        };
        let groups = || -> Vec<Vec<String>> {
            sub.get_occurrences::<String>("into")
                .map(|occurrences| {
                    occurrences
                        .map(|group| group.cloned().collect::<Vec<_>>())
                        .collect()
                })
                .unwrap_or_default()
        };
        match self {
            Self::Commit(args) => args.groups = groups(),
            Self::Amend(args) => args.groups = groups(),
            Self::Split(args) => args.groups = groups(),
            Self::Squash(args) => args.groups = groups(),
            _ => {}
        }
    }

    /// `-q`: a mutation prints only its result line.
    pub fn quiet(&self) -> bool {
        match self {
            Self::Commit(args) => args.opts.quiet,
            Self::Amend(args) => args.opts.quiet,
            Self::Discard(args) => args.opts.quiet,
            Self::Split(args) => args.opts.quiet,
            Self::Uncommit(args) => args.opts.quiet,
            Self::Squash(args) => args.opts.quiet,
            Self::Reword(args) => args.opts.quiet,
            Self::Drop(args) => args.opts.quiet,
            Self::Move(args) => args.opts.quiet,
            Self::Pick(args) => args.opts.quiet,
            Self::Sync(args) => args.opts.quiet,
            Self::Land(args) => args.opts.quiet,
            Self::Switch(args) => args.quiet,
            Self::Undo { quiet, .. } => *quiet,
            _ => false,
        }
    }

    /// `--stack`/`-v`: the full commit list after a mutation.
    pub fn stack(&self) -> bool {
        match self {
            Self::Commit(args) => args.opts.stack,
            Self::Amend(args) => args.opts.stack,
            Self::Discard(args) => args.opts.stack,
            Self::Split(args) => args.opts.stack,
            Self::Uncommit(args) => args.opts.stack,
            Self::Squash(args) => args.opts.stack,
            Self::Reword(args) => args.opts.stack,
            Self::Drop(args) => args.opts.stack,
            Self::Move(args) => args.opts.stack,
            Self::Pick(args) => args.opts.stack,
            Self::Sync(args) => args.opts.stack,
            Self::Land(args) => args.opts.stack,
            Self::Switch(args) => args.stack,
            Self::Undo { stack, .. } => *stack,
            _ => false,
        }
    }

    /// History verbs print compact text by default, even when stdout is redirected.
    pub fn prefers_text(&self) -> bool {
        !matches!(
            self,
            Self::Start(_)
                | Self::Context { .. }
                | Self::Diff { .. }
                | Self::Checkpoint { .. }
                | Self::Verify { .. }
                | Self::Finish { .. }
                | Self::Github { .. }
                | Self::Gitlab { .. }
        )
    }
}

fn bounded<const MAXIMUM: usize>(value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "expected a positive integer".to_owned())?;
    if value == 0 || value > MAXIMUM {
        return Err(format!("must be between 1 and {MAXIMUM}"));
    }
    Ok(value)
}
