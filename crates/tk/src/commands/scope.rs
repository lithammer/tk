//! Per-invocation Scope resolution (ADR-0022).
//!
//! Scope is the Container that narrows `tk next` and `tk list`. It is supplied
//! as an explicit `<epic-id>` positional argument or the `TK_SCOPE`
//! environment variable — the argument wins — and is never persisted or
//! inferred from git state. A Scope is an Epic or a Ticket that holds Tickets
//! (ADR-0055): any other Ticket is a typed error, surfaced by the command via
//! [`resolver::ResolveContainerError`].
//!
//! The command layer owns Scope resolution (per GLOSSARY.md); the store-facing
//! selection accepts an already-resolved Container id.

use crate::cli::CommandError;
use crate::commands::resolver::{self, ResolveContainerError};
use crate::domain::item_class::ItemClass;
use crate::store::repository::{ResolvedItemRef, Store};

/// Environment variable carrying a session Scope for orchestrated / AFK runs.
/// A parent process exports it so every `tk` subprocess inherits the same
/// Container without the agent restating it on each call.
const SCOPE_ENV: &str = "TK_SCOPE";

/// Invalid input can be shown as context; a failed Store read cannot (ADR-0052).
#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    #[error("scope '{0}' is not a known Display ID or Alias")]
    NotFound(String),
    #[error("scope '{0}' is neither an Epic nor a Ticket that holds Tickets")]
    NotAContainer(String),
    #[error(transparent)]
    Storage(rusqlite::Error),
}

impl From<ScopeError> for CommandError {
    fn from(error: ScopeError) -> Self {
        match error {
            ScopeError::Storage(err) => resolver::storage_error(&err),
            error => Self::failure(error),
        }
    }
}

/// Resolve the active Scope for a command (ADR-0032).
///
/// Reads the `<epic-id>` `arg` if present, else `TK_SCOPE`, else `None`, then
/// resolves it as a Container. `Ok(None)` means no Scope was supplied.
/// Commands choose whether invalid input is a failure or a briefing warning.
pub fn resolve(store: &Store, arg: Option<&str>) -> Result<Option<ResolvedItemRef>, ScopeError> {
    let Some(value) = effective_value(arg, env_value().as_deref()) else {
        return Ok(None);
    };
    match resolver::resolve_container(store, &value) {
        Ok(container) => Ok(Some(container)),
        Err(ResolveContainerError::NotFound) => Err(ScopeError::NotFound(value)),
        Err(ResolveContainerError::NotAContainer) => Err(ScopeError::NotAContainer(value)),
        Err(ResolveContainerError::Storage(err)) => Err(ScopeError::Storage(err)),
    }
}

/// What a Scope of this class covers, for the hint above a Scope-filtered view.
#[must_use]
pub fn extent(class: ItemClass) -> &'static str {
    match class {
        ItemClass::Epic => "(Epic + child Tickets)",
        ItemClass::Ticket => "(Ticket + held Tickets)",
    }
}

/// The Scope value in effect for one invocation: the positional `arg` if
/// present, else a non-empty trimmed `TK_SCOPE`, else `None`. The argument
/// wins when both are set.
fn effective_value(arg: Option<&str>, env: Option<&str>) -> Option<String> {
    if let Some(arg) = arg {
        return Some(arg.to_owned());
    }
    env.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Read `TK_SCOPE` from the process environment. Returns `None` when unset or
/// not valid UTF-8; emptiness is filtered later by [`effective_value`].
fn env_value() -> Option<String> {
    std::env::var(SCOPE_ENV).ok()
}
