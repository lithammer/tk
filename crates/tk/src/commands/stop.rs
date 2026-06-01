//! `tk stop` — return an active Ticket or Epic to open.

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
    ticket: "Stopped Ticket: ",
    epic: "Stopped Epic: ",
};

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
    lifecycle::transition(deps, "stop", &args.id, ItemStatus::Open, SUCCESS)
}
