//! `tk done` — close a Ticket or Epic. Terminal state per ADR-0006.
//!
//! `-m/--message` records an optional Closing Reason (ADR-0023): a Local
//! Field captured set-once on the `→ done` transition, rendered by `tk show`,
//! never synced. The reason is inline-only and must be non-empty.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::lifecycle::{self, SuccessLabel};
use crate::domain::status::ItemStatus;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "ID")]
    pub id: String,
    /// Optional Closing Reason recorded against the closed item.
    #[arg(short = 'm', long = "message", value_name = "TEXT")]
    pub message: Option<String>,
}

const SUCCESS: SuccessLabel = SuccessLabel {
    ticket: "Done Ticket: ",
    epic: "Done Epic: ",
};

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    // Trim first, then reject empty: the column invariant is "absent = NULL,
    // present = non-empty" (ADR-0023), so a whitespace-only `-m` is a no-op
    // dressed as a reason and must fail loudly rather than store blank.
    let closing_reason = match args.message.as_deref().map(str::trim) {
        Some("") => {
            return Err(CommandError::failure("closing reason must not be empty"));
        }
        other => other,
    };
    lifecycle::transition(deps, &args.id, ItemStatus::Done, SUCCESS, closing_reason)
}
