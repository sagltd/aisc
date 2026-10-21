//! Slice stub: discard is implemented in a later slice.
use super::{Outcome, not_implemented};
use crate::cli;
use anyhow::Result;

pub(crate) fn run(_args: &cli::Discard) -> Result<Outcome> {
    Err(not_implemented("discard"))
}
