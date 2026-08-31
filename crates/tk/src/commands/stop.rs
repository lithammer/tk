//! `tk stop` — return an active Ticket or Epic to open.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::item_status::{self, SuccessLabel, Transition};

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
    item_status::transition(deps, &args.id, Transition::Stop, SUCCESS)
}
