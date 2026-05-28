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

use crate::git::discovery::{self, Outcome as DiscoveryOutcome};
use crate::proc::ProcRunner;
use crate::store::repository::{
    self, OpenError as RepoOpenError, OpenOutcome, ResolveEpicOutcome,
    ResolveEpicWithDisplayOutcome, ResolvedItemRef, ResolvedItemRefWithDisplay, Store,
};

/// Failure of [`open_for_command`]. Each variant carries the data its
/// rendered message needs; [`render_open_error`] picks the right line.
#[derive(Debug, Error)]
pub enum OpenError {
    /// `git rev-parse` failed; the inner outcome carries the exact shape
    /// the shared `discovery::render_failure` consumes.
    #[error("git discovery failed")]
    DiscoveryFailed(DiscoveryOutcome),
    #[error("Repository Store not initialized")]
    StoreMissing,
    #[error("not a tk Repository Store")]
    NotTicketStore,
    #[error("Repository Store created by a newer tk version")]
    FromFutureVersion,
    #[error(transparent)]
    Storage(rusqlite::Error),
}

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
pub fn open_for_command<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
) -> Result<Store, OpenError> {
    let outcome = match repository::open_existing(runner, cwd) {
        Ok(outcome) => outcome,
        Err(RepoOpenError::Sqlite(err)) => return Err(OpenError::Storage(err)),
    };
    match outcome {
        OpenOutcome::Ok(store) => Ok(store),
        OpenOutcome::DiscoveryFailed(inner) => Err(OpenError::DiscoveryFailed(inner)),
        OpenOutcome::StoreMissing => Err(OpenError::StoreMissing),
        OpenOutcome::NotTicketStore => Err(OpenError::NotTicketStore),
        OpenOutcome::FromFutureVersion => Err(OpenError::FromFutureVersion),
    }
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
        OpenError::DiscoveryFailed(inner) => discovery::render_failure(stderr, command, inner),
        OpenError::StoreMissing => {
            let _ = writeln!(
                stderr,
                "tk {command}: Repository Store not initialized; run 'tk init'"
            );
        }
        OpenError::NotTicketStore => {
            let _ = writeln!(stderr, "tk {command}: Repository Store is not a tk Repository Store");
        }
        OpenError::FromFutureVersion => {
            let _ = writeln!(
                stderr,
                "tk {command}: Repository Store was created by a newer tk version"
            );
        }
        OpenError::Storage(err) => render_storage_error(stderr, command, err),
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
