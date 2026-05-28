//! `tk unblock` — remove a Dependency edge.

use clap::Args as ClapArgs;

use crate::cli::Deps;
use crate::commands::resolver;
use crate::store::repository::dependency::{self, DependencyEdge, RemoveDependencyError};

const COMMAND: &str = "unblock";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Item that was being blocked.
    #[arg(value_name = "BLOCKED")]
    pub blocked: String,
    /// Item that no longer blocks.
    #[arg(value_name = "BLOCKING")]
    pub blocking: String,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        ..
    } = deps;

    let mut store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    let blocked = match resolver::resolve(&store, &args.blocked) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk unblock: blocked '{}' is not a known Display ID or Alias",
                args.blocked
            );
            return 1;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return 1;
        }
    };
    let blocking = match resolver::resolve(&store, &args.blocking) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk unblock: blocking '{}' is not a known Display ID or Alias",
                args.blocking
            );
            return 1;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    if blocked.id == blocking.id {
        let _ = writeln!(stderr, "tk unblock: an item cannot block itself");
        return 1;
    }

    match dependency::remove_dependency(
        &mut store,
        clock,
        DependencyEdge {
            blocked_id: &blocked.id,
            blocking_id: &blocking.id,
        },
    ) {
        Ok(()) => {
            let _ = writeln!(
                stdout,
                "Unblocked: {} no longer blocked by {}",
                args.blocked, args.blocking
            );
            0
        }
        Err(RemoveDependencyError::EndpointMissing) => {
            let _ = writeln!(stderr, "tk unblock: endpoint missing in items table");
            1
        }
        Err(RemoveDependencyError::Sqlite(err)) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            1
        }
        Err(RemoveDependencyError::Mutation(err)) => {
            let _ = writeln!(stderr, "tk unblock: failed to append Mutation: {err}");
            1
        }
    }
}
