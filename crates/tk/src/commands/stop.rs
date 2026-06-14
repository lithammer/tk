//! `tk stop` — return an active Ticket or Epic to open.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
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

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    lifecycle::transition(deps, &args.id, ItemStatus::Open, SUCCESS, None)
}
