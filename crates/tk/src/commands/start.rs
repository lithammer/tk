//! `tk start` — mark one Ticket or Epic active.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::item_status::{self, SuccessLabel, Transition};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to start.
    #[arg(value_name = "ID")]
    pub id: String,
}

const SUCCESS: SuccessLabel = SuccessLabel {
    ticket: "Started Ticket: ",
    epic: "Started Epic: ",
};

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    item_status::transition(deps, &args.id, Transition::Start, SUCCESS)
}
