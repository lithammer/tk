//! Shared Dependency-edge prologue for `tk block` / `tk unblock`.
//!
//! Both commands open the Repository Store, resolve the Blocked Item and the
//! Blocking Item from a Display ID or Alias, and refuse a self-edge before
//! their own Mutation. The blocked/blocking not-found phrasing is owned by
//! exactly this command family, as `item_status` owns the phrasing shared by
//! `tk start` / `tk stop` / `tk done`; the `tk <command>:` frame is supplied
//! by the dispatch seam (ADR-0032).

use std::path::Path;

use crate::cli::CommandError;
use crate::clock::Clock;
use crate::commands::resolver;
use crate::proc::ProcRunner;
use crate::store::repository::{ResolvedItemRef, Store};

/// Open the Repository Store and resolve both endpoints of a Dependency edge,
/// refusing a self-edge. On failure returns the [`CommandError`] for the
/// dispatch seam to frame as `tk block:` / `tk unblock:` (ADR-0032).
pub fn resolve<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    clock: &dyn Clock,
    blocked_arg: &str,
    blocking_arg: &str,
) -> Result<(Store, ResolvedItemRef, ResolvedItemRef), CommandError> {
    let store =
        resolver::open_for_command(runner, cwd, clock).map_err(|err| resolver::open_error(&err))?;

    let blocked = match resolver::resolve(&store, blocked_arg) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocked '{blocked_arg}' is not a known Display ID or Alias"
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let blocking = match resolver::resolve(&store, blocking_arg) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "blocking '{blocking_arg}' is not a known Display ID or Alias"
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    if blocked.id == blocking.id {
        return Err(CommandError::failure("an item cannot block itself"));
    }

    Ok((store, blocked, blocking))
}
