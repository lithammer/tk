//! `tk unblock` — remove a Dependency edge.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::dependency_edge;
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
    let (mut store, blocked, blocking) = dependency_edge::resolve(
        deps.runner,
        deps.cwd,
        deps.clock,
        &args.blocked,
        &args.blocking,
    )?;

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
        Err(RemoveDependencyError::BackendBinding(err)) => {
            Err(resolver::backend_binding_error(&err))
        }
        Err(RemoveDependencyError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}
