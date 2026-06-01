//! `tk promote` — not-yet-implemented stub.
//!
//! Promotion (Local → Remote) lands in a later slice, once the Remote and sync
//! engine are exercised against a real Backend Adapter (tk-40). The stub keeps
//! the command surface present — `tk --help` lists it and a fresh agent session
//! gets a clear "planned" signal — while exiting 1 with empty stdout so no
//! caller mistakes it for a working operation.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to promote.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
}

#[must_use]
pub fn run(deps: Deps<'_>, _args: Args) -> Exit {
    let _ = writeln!(deps.stderr, "tk promote: not yet implemented");
    let _ = writeln!(
        deps.stderr,
        "Planned: later slice once Remote and sync are in place."
    );
    Exit::Failure
}
