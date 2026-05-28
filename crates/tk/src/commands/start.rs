//! `tk start` — mark one Ticket or Epic active.

use clap::Args as ClapArgs;

use crate::cli::Deps;
use crate::commands::lifecycle::{self, SuccessLabel};
use crate::domain::status::ItemStatus;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "ID")]
    pub id: String,
}

const SUCCESS: SuccessLabel = SuccessLabel {
    ticket: "Started Ticket: ",
    epic: "Started Epic: ",
};

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    lifecycle::transition(deps, "start", &args.id, ItemStatus::Active, SUCCESS)
}
