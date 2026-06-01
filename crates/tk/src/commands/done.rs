//! `tk done` — close a Ticket or Epic. Terminal state per ADR-0006.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::lifecycle::{self, SuccessLabel};
use crate::domain::status::ItemStatus;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "ID")]
    pub id: String,
}

const SUCCESS: SuccessLabel = SuccessLabel {
    ticket: "Done Ticket: ",
    epic: "Done Epic: ",
};

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
    lifecycle::transition(deps, "done", &args.id, ItemStatus::Done, SUCCESS)
}
