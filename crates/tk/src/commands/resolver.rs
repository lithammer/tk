//! Command-side Repository Store access seam.
//!
//! Every item command runs the same prologue: open the Repository Store,
//! then resolve a Display ID or Alias into an internal stable item ID.
//! This module exposes the prologue plus typed error enums; per-command
//! handlers render the diagnostic with their stable phrasing (ADR-0017)
//! and return exit code 1.
//!
//! Errors are the standard Rust `Result` shape: [`OpenError`],
//! [`ResolveError`], [`ResolveEpicError`]. They carry the failure
//! discriminator (`NotFound`, `NotAnEpic`, an underlying SQLite error)
//! without baking in any rendering templates — the caller owns the
//! per-command phrasing.

use std::io::Write;
use std::path::Path;

use crate::git::discovery::{self, Outcome as DiscoveryOutcome};
use crate::messages;
use crate::proc::ProcRunner;
use crate::store::repository::{
    self, OpenError as RepoOpenError, OpenOutcome, ResolveEpicOutcome,
    ResolveEpicWithDisplayOutcome, ResolvedItemRef, ResolvedItemRefWithDisplay, Store,
};

/// Per-command storage-error phrasing for the [`render_storage_error`]
/// helper. `fallback` is the non-transient diagnostic; the renderer
/// appends the underlying error's `Display` so the SQLite errmsg reaches
/// stderr verbatim.
#[derive(Debug, Clone, Copy)]
pub struct StorageErrorMessages {
    pub busy_retry: &'static str,
    pub out_of_memory: &'static str,
    pub fallback: &'static str,
}

/// Per-command phrasing for [`render_open_error`].
#[derive(Debug, Clone, Copy)]
pub struct OpenErrorMessages {
    /// Subcommand name as it appears in diagnostics, e.g. `"show"`.
    pub command_name: &'static str,
    /// Pre-formatted "Repository Store not initialized" line.
    pub missing_store: &'static str,
    pub storage: StorageErrorMessages,
}

/// Result of [`open_for_command`].
pub struct OpenedStore {
    pub store: Store,
    pub storage: StorageErrorMessages,
}

/// Failure of [`open_for_command`].
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
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

/// Failure of [`resolve_or_render`].
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error(transparent)]
    Storage(rusqlite::Error),
}

/// Failure of [`resolve_epic`] / [`resolve_epic_with_display`].
#[derive(Debug, thiserror::Error)]
pub enum ResolveEpicError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error("resolved Item is not an Epic")]
    NotAnEpic,
    #[error(transparent)]
    Storage(rusqlite::Error),
}

/// Open the Repository Store for a command. Errors are typed; the caller
/// renders the curated stderr line and returns exit code 1.
pub fn open_for_command<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    msgs: StorageErrorMessages,
) -> Result<OpenedStore, OpenError> {
    let outcome = match repository::open_existing(runner, cwd) {
        Ok(outcome) => outcome,
        Err(RepoOpenError::Sqlite(err)) => return Err(OpenError::Storage(err)),
    };
    match outcome {
        OpenOutcome::Ok(store) => Ok(OpenedStore {
            store,
            storage: msgs,
        }),
        OpenOutcome::DiscoveryFailed(inner) => Err(OpenError::DiscoveryFailed(inner)),
        OpenOutcome::StoreMissing => Err(OpenError::StoreMissing),
        OpenOutcome::NotTicketStore => Err(OpenError::NotTicketStore),
        OpenOutcome::FromFutureVersion => Err(OpenError::FromFutureVersion),
    }
}

/// Resolve a Display ID or Alias against an opened store, propagating a
/// typed `NotFound` for unknown values.
pub fn resolve(store: &Store, arg: &str) -> Result<ResolvedItemRef, ResolveError> {
    match repository::resolve_item_ref(store.conn(), arg) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(ResolveError::NotFound),
        Err(err) => Err(ResolveError::Storage(err)),
    }
}

/// Like [`resolve`] but also returns the current Display ID.
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

/// Resolve a Display ID or Alias that must refer to an Epic. The
/// `NotAnEpic` arm fires when the value resolves but to a Ticket.
pub fn resolve_epic(store: &Store, arg: &str) -> Result<ResolvedItemRef, ResolveEpicError> {
    match repository::resolve_as_epic(store.conn(), arg) {
        Ok(ResolveEpicOutcome::Epic(r)) => Ok(r),
        Ok(ResolveEpicOutcome::NotFound) => Err(ResolveEpicError::NotFound),
        Ok(ResolveEpicOutcome::NotAnEpic(_)) => Err(ResolveEpicError::NotAnEpic),
        Err(err) => Err(ResolveEpicError::Storage(err)),
    }
}

/// Like [`resolve_epic`] but with the current Display ID attached for
/// success diagnostics.
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

/// Render an [`OpenError`] using the caller's per-command phrasing.
pub fn render_open_error<W: Write + ?Sized>(
    stderr: &mut W,
    err: &OpenError,
    msgs: OpenErrorMessages,
) {
    match err {
        OpenError::DiscoveryFailed(inner) => {
            discovery::render_failure(stderr, msgs.command_name, inner);
        }
        OpenError::StoreMissing => {
            let _ = writeln!(stderr, "{}", msgs.missing_store);
        }
        OpenError::NotTicketStore => {
            let _ = writeln!(
                stderr,
                "tk {}: Repository Store is {}",
                msgs.command_name,
                messages::INIT_REFUSE_FOREIGN
            );
        }
        OpenError::FromFutureVersion => {
            let _ = writeln!(
                stderr,
                "tk {}: Repository Store was created by a {}",
                msgs.command_name,
                messages::INIT_REFUSE_FUTURE_VERSION
            );
        }
        OpenError::Storage(err) => render_storage_error(stderr, err, msgs.storage),
    }
}

/// Render a Repository Store storage failure.
///
/// Busy/locked classes — `SQLITE_BUSY` / `SQLITE_LOCKED` — get a dedicated
/// "retry" diagnostic. Everything else falls through to `fallback`
/// followed by the underlying error's `Display`.
pub fn render_storage_error<W: Write + ?Sized>(
    stderr: &mut W,
    err: &rusqlite::Error,
    msgs: StorageErrorMessages,
) {
    if is_busy_error(err) {
        let _ = writeln!(stderr, "{}", msgs.busy_retry);
        return;
    }
    let _ = writeln!(stderr, "{}\n{err}", msgs.fallback);
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
