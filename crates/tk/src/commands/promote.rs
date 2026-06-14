//! `tk promote` — not-yet-implemented stub.
//!
//! Promotion (Local → Remote) lands in a later release, once the Remote and sync
//! engine are exercised against a real Backend Adapter (tk-40). The stub keeps
//! the command surface present — `tk --help` lists it and a fresh agent session
//! gets a clear "planned" signal — while exiting 1 with empty stdout so no
//! caller mistakes it for a working operation.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to promote.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
}

pub fn run(_deps: &mut Deps<'_>, _args: Args) -> Result<Exit, CommandError> {
    // Two-line body: the seam frames the first line as `tk promote:`; the
    // "Planned:" cause line is tk's own voice and rides along after the
    // newline (ADR-0032).
    Err(CommandError::failure(
        "not yet implemented\nPlanned: available once Remote and sync are in place.",
    ))
}
