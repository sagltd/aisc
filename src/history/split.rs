//! Slice stub: split and uncommit are implemented in a later slice (algorithm 6.5).
use super::{Outcome, not_implemented};
use crate::cli;
use anyhow::Result;

pub(crate) fn run_split(_args: &cli::Split) -> Result<Outcome> {
    Err(not_implemented("split"))
}

pub(crate) fn run_uncommit(_args: &cli::Uncommit) -> Result<Outcome> {
    Err(not_implemented("uncommit"))
}
