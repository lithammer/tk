//! `tk unblock` — remove a Dependency edge.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::store::repository::dependency::{self, DependencyEdge, RemoveDependencyError};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Item that was being blocked.
    #[arg(value_name = "BLOCKED")]
    pub blocked: String,
    /// Item that no longer blocks.
    #[arg(value_name = "BLOCKING")]
    pub blocking: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let blocked = match resolver::resolve(&store, &args.blocked) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocked '{}' is not a known Display ID or Alias",
                args.blocked
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let blocking = match resolver::resolve(&store, &args.blocking) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocking '{}' is not a known Display ID or Alias",
                args.blocking
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    if blocked.id == blocking.id {
        return Err(CommandError::failure("an item cannot block itself"));
    }

    match dependency::remove_dependency(
        &mut store,
        deps.clock,
        DependencyEdge {
            blocked_id: &blocked.id,
            blocking_id: &blocking.id,
        },
    ) {
        Ok(()) => {
            let _ = writeln!(
                deps.stdout,
                "Unblocked: {} no longer blocked by {}",
                args.blocked, args.blocking
            );
            Ok(Exit::Ok)
        }
        Err(RemoveDependencyError::EndpointMissing) => {
            Err(CommandError::failure("endpoint missing in items table"))
        }
        Err(RemoveDependencyError::Sqlite(err)) => Err(resolver::storage_error(&err)),
        Err(RemoveDependencyError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}
