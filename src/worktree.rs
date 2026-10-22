//! Slice stub: worktree registry, finish cleanup and gc are implemented in a later slice.
use crate::cli;
use crate::history::{Outcome, not_implemented};
use anyhow::Result;

pub(crate) fn gc(_args: &cli::Gc) -> Result<Outcome> {
    Err(not_implemented("gc"))
}
