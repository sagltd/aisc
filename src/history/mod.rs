//! Shared history engine: branch view, hunks, selectors, plumbing, rewrite plans and the oplog.
//! Verbs live in sibling modules; see docs/design/history.md.
use crate::cli::Operation;
use anyhow::Result;
use serde_json::Value;
use std::fmt;

// Engine modules expose helpers that later slices (amend, split, squash, sync, ...) consume.
pub(crate) mod amend;
#[allow(dead_code)]
pub(crate) mod apply;
pub(crate) mod commit;
#[allow(dead_code)]
pub(crate) mod diff;
pub(crate) mod discard;
pub(crate) mod land;
pub(crate) mod log;
#[allow(dead_code)]
pub(crate) mod model;
pub(crate) mod oplog;
pub(crate) mod pick;
#[allow(dead_code)]
pub(crate) mod plumbing;
pub(crate) mod render;
pub(crate) mod reorder;
pub(crate) mod rewrite;
#[allow(dead_code)]
pub(crate) mod selector;
pub(crate) mod split;
pub(crate) mod squash;
pub(crate) mod sync;
pub(crate) mod undo;
#[allow(dead_code)]
pub(crate) mod view;

/// Machine-readable error classes; `Stale` maps to exit code 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // `Conflict` is raised by sync in a later slice
pub(crate) enum Code {
    Usage,
    Selector,
    Stale,
    Dirty,
    Conflict,
    Unsupported,
    State,
    Git,
}

impl Code {
    pub fn name(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Selector => "selector",
            Self::Stale => "stale",
            Self::Dirty => "dirty",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
            Self::State => "state",
            Self::Git => "git",
        }
    }
    pub fn exit(self) -> i32 {
        if self == Self::Stale { 4 } else { 1 }
    }
}

#[derive(Debug)]
pub(crate) struct HistoryError {
    pub code: Code,
    pub message: String,
    pub hint: Option<String>,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for HistoryError {}

pub(crate) fn fail(code: Code, message: impl Into<String>) -> anyhow::Error {
    HistoryError {
        code,
        message: message.into(),
        hint: None,
    }
    .into()
}

pub(crate) fn fail_hint(
    code: Code,
    message: impl Into<String>,
    hint: impl Into<String>,
) -> anyhow::Error {
    HistoryError {
        code,
        message: message.into(),
        hint: Some(hint.into()),
    }
    .into()
}

/// What a verb prints: JSON value, compact text, and the process exit code.
pub(crate) struct Outcome {
    pub value: Value,
    pub text: Option<String>,
    pub exit: i32,
}

impl Outcome {
    pub fn plain(value: Value) -> Self {
        Self {
            value,
            text: None,
            exit: 0,
        }
    }
    pub fn with_text(value: Value, text: String) -> Self {
        Self {
            value,
            text: Some(text),
            exit: 0,
        }
    }
}

/// Dispatch for every history verb. Returns `None` for task verbs.
pub(crate) fn run(command: &Operation) -> Option<Result<Outcome>> {
    Some(match command {
        Operation::Log(args) => log::run(args),
        Operation::Show(args) => log::show(args),
        Operation::Commit(args) => commit::run(args),
        Operation::Amend(args) => amend::run(args),
        Operation::Discard(args) => discard::run(args),
        Operation::Split(args) => split::run_split(args),
        Operation::Uncommit(args) => split::run_uncommit(args),
        Operation::Squash(args) => squash::run(args),
        Operation::Move(args) => reorder::run(args),
        Operation::Pick(args) => pick::run(args),
        Operation::Sync(args) => sync::run(args),
        Operation::Land(args) => land::run(args),
        Operation::Switch(args) => land::switch(args),
        Operation::Undo { op } => undo::run(op.as_deref()),
        Operation::Oplog { limit } => oplog::run(*limit),
        Operation::Gc(args) => crate::worktree::gc(args),
        _ => return None,
    })
}
