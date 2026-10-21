//! Slice stub: land and switch are implemented in a later slice.
use super::{Outcome, not_implemented};
use crate::cli;
use anyhow::Result;

pub(crate) fn run(_args: &cli::Land) -> Result<Outcome> {
    Err(not_implemented("land"))
}

pub(crate) fn switch(_args: &cli::Switch) -> Result<Outcome> {
    Err(not_implemented("switch"))
}
