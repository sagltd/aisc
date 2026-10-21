//! Slice stub: sync is implemented in a later slice (algorithm 6.6 of docs/design/history.md).
use super::{Outcome, not_implemented};
use crate::cli;
use anyhow::Result;

pub(crate) fn run(_args: &cli::Sync) -> Result<Outcome> {
    Err(not_implemented("sync"))
}
