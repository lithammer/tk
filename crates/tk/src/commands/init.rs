//! `tk init` creates or validates a durable Repository Store (ADR-0053).
use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::git::discovery;
use clap::Args as ClapArgs;

/// Initialize the current repository's Store Association.
#[derive(Debug, ClapArgs)]
pub struct Args {}

/// Publish a fresh Store before installing its repository-local pointer.
pub fn run(deps: &mut Deps<'_>, _args: Args) -> Result<Exit, CommandError> {
    let paths = discovery::discover_paths(deps.runner, deps.cwd)
        .map_err(|e| CommandError::failure(e.to_string()))?;
    let result = crate::store::initialize::initialize(
        deps.runner,
        deps.cwd,
        deps.clock,
        deps.rng,
        deps.data_root,
        &paths,
    );
    let (path, prefix) = match result.map_err(|e| resolver::open_error(&e))? {
        crate::store::initialize::Initialized::Created(path) => {
            (path, "Initialized Repository Store at ")
        }
        crate::store::initialize::Initialized::Existing(path) => {
            (path, "Repository Store already initialized at ")
        }
    };
    let _ = writeln!(deps.stdout, "{prefix}{}", path.display());
    Ok(Exit::Ok)
}
