//! Command-side Repository Store access seam.
//!
//! Every item command runs the same prologue: open the Repository Store,
//! then resolve a Display ID or Alias into an internal stable item ID.
//! This module owns the typed errors that prologue can raise plus the
//! shared stderr-rendering helpers; the per-command not-found phrasing
//! is owned by the command itself, inlined into its own typed error
//! variant.

use std::io::Write;
use std::path::Path;

use thiserror::Error;

use crate::cli::CommandError;
use crate::clock::Clock;
use crate::proc::ProcRunner;
use crate::store::repository::{self, ResolvedItemRef, ResolvedItemRefWithDisplay, Store};

/// Errors re-exported from the store layer, where the operations that produce
/// them live. [`OpenError`] is rendered by [`render_open_error`]; the resolve
/// errors are matched and rendered by each command.
pub use crate::store::repository::{OpenError, ResolveEpicError};

/// Failure of [`resolve`] / [`resolve_with_display`].
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error(transparent)]
    Storage(rusqlite::Error),
}

/// Open the Repository Store for a command.
///
/// A thin command-facing alias for [`repository::open_existing`]; the rich
/// [`OpenError`] now flows straight through (commands hand it to
/// [`render_open_error`] without inspecting variants).
pub fn open_for_command<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    clock: &dyn Clock,
) -> Result<Store, OpenError> {
    repository::open_existing(runner, cwd, clock)
}

/// Resolve a Display ID or Alias against an opened store.
pub fn resolve(store: &Store, arg: &str) -> Result<ResolvedItemRef, ResolveError> {
    match repository::resolve_item_ref(store.conn(), arg) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(ResolveError::NotFound),
        Err(err) => Err(ResolveError::Storage(err)),
    }
}

/// Like [`resolve`] but with the current Display ID attached.
pub fn resolve_with_display(
    store: &Store,
    arg: &str,
) -> Result<ResolvedItemRefWithDisplay, ResolveError> {
    match repository::resolve_item_ref_with_display(store.conn(), arg) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(ResolveError::NotFound),
        Err(err) => Err(ResolveError::Storage(err)),
    }
}

/// Resolve a Display ID or Alias that must refer to an Epic.
pub fn resolve_epic(store: &Store, arg: &str) -> Result<ResolvedItemRef, ResolveEpicError> {
    repository::resolve_as_epic(store.conn(), arg)
}

/// Like [`resolve_epic`] but with the current Display ID attached.
pub fn resolve_epic_with_display(
    store: &Store,
    arg: &str,
) -> Result<ResolvedItemRefWithDisplay, ResolveEpicError> {
    repository::resolve_as_epic_with_display(store.conn(), arg)
}

/// Build the [`CommandError`] for an [`OpenError`] (ADR-0032). The body is the
/// stable user-facing line minus the `tk <command>:` frame, which the seam
/// supplies. The phrasing is identical across commands.
#[must_use]
pub fn open_error(err: &OpenError) -> CommandError {
    match err {
        // SQLite faults route through the busy-aware storage classifier; every
        // other variant's `Display` is already the stable user-facing body
        // (including DiscoveryFailed, which forwards git's own message).
        OpenError::Sqlite(e) => storage_error(e),
        // An open-time migration failure surfaces its static line plus the
        // underlying SQLite cause (matching the storage-error shape), so the
        // upgrade fault is not reduced to a bare headline.
        OpenError::MigrationFailed(e) => CommandError::failure(format!("{err}\n{e}")),
        _ => CommandError::failure(format!("{err}")),
    }
}

/// Build the [`CommandError`] for a Repository Store storage error (ADR-0032).
///
/// `SQLITE_BUSY` / `SQLITE_LOCKED` get the "retry the command" line;
/// everything else falls through to `failed to read Repository Store`
/// followed by the underlying error's `Display`.
#[must_use]
pub fn storage_error(err: &rusqlite::Error) -> CommandError {
    if is_busy_error(err) {
        CommandError::failure("Repository Store is busy; retry the command")
    } else {
        CommandError::failure(format!("failed to read Repository Store\n{err}"))
    }
}

/// Render an [`OpenError`] to stderr with the supplied `tk <command>:` prefix.
/// Pre-seam shim over [`open_error`] for commands not yet converted to the
/// ADR-0032 seam.
pub fn render_open_error<W: Write + ?Sized>(stderr: &mut W, command: &str, err: &OpenError) {
    open_error(err).render(stderr, command);
}

/// Render a Repository Store storage error to stderr. Pre-seam shim over
/// [`storage_error`] for commands not yet converted to the ADR-0032 seam.
pub fn render_storage_error<W: Write + ?Sized>(
    stderr: &mut W,
    command: &str,
    err: &rusqlite::Error,
) {
    storage_error(err).render(stderr, command);
}

fn is_busy_error(err: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    if let rusqlite::Error::SqliteFailure(inner, _) = err {
        return matches!(
            inner.code,
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
        );
    }
    false
}
