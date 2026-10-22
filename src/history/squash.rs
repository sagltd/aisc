//! Slice stub: squash is implemented in a later slice.
use super::{Outcome, not_implemented};
use crate::cli;
use anyhow::Result;

pub(crate) fn run(_args: &cli::Squash) -> Result<Outcome> {
    Err(not_implemented("squash"))
}
