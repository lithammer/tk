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

use crate::proc::ProcRunner;
use crate::store::repository::{
    self, ResolveEpicOutcome, ResolveEpicWithDisplayOutcome, ResolvedItemRef,
    ResolvedItemRefWithDisplay, Store,
};

/// Failure of [`open_for_command`], re-exported from the store layer where
/// [`repository::open_existing`] produces it. [`render_open_error`] renders it
/// behind the `tk <command>:` prefix.
pub use crate::store::repository::OpenError;

/// Failure of [`resolve`] / [`resolve_with_display`].
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error(transparent)]
    Storage(rusqlite::Error),
}

/// Failure of [`resolve_epic`] / [`resolve_epic_with_display`].
#[derive(Debug, Error)]
pub enum ResolveEpicError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error("resolved Item is not an Epic")]
    NotAnEpic,
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
) -> Result<Store, OpenError> {
    repository::open_existing(runner, cwd)
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
    match repository::resolve_as_epic(store.conn(), arg) {
        Ok(ResolveEpicOutcome::Epic(r)) => Ok(r),
        Ok(ResolveEpicOutcome::NotFound) => Err(ResolveEpicError::NotFound),
        Ok(ResolveEpicOutcome::NotAnEpic(_)) => Err(ResolveEpicError::NotAnEpic),
        Err(err) => Err(ResolveEpicError::Storage(err)),
    }
}

/// Like [`resolve_epic`] but with the current Display ID attached.
pub fn resolve_epic_with_display(
    store: &Store,
    arg: &str,
) -> Result<ResolvedItemRefWithDisplay, ResolveEpicError> {
    match repository::resolve_as_epic_with_display(store.conn(), arg) {
        Ok(ResolveEpicWithDisplayOutcome::Epic(r)) => Ok(r),
        Ok(ResolveEpicWithDisplayOutcome::NotFound) => Err(ResolveEpicError::NotFound),
        Ok(ResolveEpicWithDisplayOutcome::NotAnEpic(_)) => Err(ResolveEpicError::NotAnEpic),
        Err(err) => Err(ResolveEpicError::Storage(err)),
    }
}

/// Render an [`OpenError`] to stderr with the supplied `tk <command>:`
/// prefix. The phrasing is identical across commands — only the
/// command-name token varies.
pub fn render_open_error<W: Write + ?Sized>(stderr: &mut W, command: &str, err: &OpenError) {
    match err {
        // SQLite faults route through the busy-aware storage renderer; every
        // other variant's `Display` is already the stable user-facing line
        // (including DiscoveryFailed, which forwards git's own message).
        OpenError::Sqlite(e) => render_storage_error(stderr, command, e),
        _ => {
            let _ = writeln!(stderr, "tk {command}: {err}");
        }
    }
}

/// Render a Repository Store storage error to stderr.
///
/// `SQLITE_BUSY` / `SQLITE_LOCKED` get the "retry the command" line;
/// everything else falls through to `failed to read Repository Store`
/// followed by the underlying error's `Display`.
pub fn render_storage_error<W: Write + ?Sized>(
    stderr: &mut W,
    command: &str,
    err: &rusqlite::Error,
) {
    if is_busy_error(err) {
        let _ = writeln!(
            stderr,
            "tk {command}: Repository Store is busy; retry the command"
        );
        return;
    }
    let _ = writeln!(
        stderr,
        "tk {command}: failed to read Repository Store\n{err}"
    );
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
