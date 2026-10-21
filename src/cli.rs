use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "aisc",
    version,
    about = "Task-oriented source control, backed by Git",
    after_help = "Examples:\n  aisc start fix --in-place\n  aisc start fix --path ../fix\n  aisc checkpoint -m 'Fix parser' -- src/parser.rs\n  aisc verify -- cargo test\n  aisc finish\n\nRedirected output defaults to JSON; terminals get readable text.\nPaths are literal files relative to the repository root.\nNo automatic push, merge, or cleanup. Configured Git hooks/signing run normally."
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
    Finish,
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
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("location").required(true).multiple(false).args(["path", "in_place"])))]
pub(crate) struct Start {
    pub name: String,
    /// Complete destination path; must not exist and its parent must exist
    #[arg(long, group = "location", value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Keep this directory and branch; requires a clean committed checkout
    #[arg(long, group = "location")]
    pub in_place: bool,
}

impl Operation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::Context { .. } => "context",
            Self::Diff { .. } => "diff",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Verify { .. } => "verify",
            Self::Finish => "finish",
            Self::Github { .. } => "github",
            Self::Gitlab { .. } => "gitlab",
        }
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
