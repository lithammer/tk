//! Per-invocation Scope resolution (ADR-0022).
//!
//! Scope is the Epic that narrows `tk next` and `tk list`. It is supplied as
//! an explicit `<epic-id>` positional argument or the `TK_SCOPE` environment
//! variable — the argument wins — and is never persisted or inferred from git
//! state. Resolution is Epic-only: a value that resolves to a Ticket is a
//! typed error, surfaced by the command via [`resolver::ResolveEpicError`].
//!
//! The command layer owns Scope resolution (per CONTEXT.md); the store-facing
//! selection accepts an already-resolved Epic id.

use std::io::Write;

use crate::cli::{CommandError, Exit};
use crate::commands::resolver::{self, ResolveEpicError};
use crate::store::repository::{ResolvedItemRefWithDisplay, Store};

/// Environment variable carrying a session Scope for orchestrated / AFK runs.
/// A parent process exports it so every `tk` subprocess inherits the same
/// Epic without the agent restating it on each call.
const SCOPE_ENV: &str = "TK_SCOPE";

/// Resolve the active Scope for a command (ADR-0032).
///
/// Reads the `<epic-id>` `arg` if present, else `TK_SCOPE`, else `None`, then
/// resolves Epic-only: a miss or a non-Epic becomes a [`CommandError`] for the
/// dispatch seam to frame. `Ok(None)` means no Scope was supplied.
pub fn resolve(
    store: &Store,
    arg: Option<&str>,
) -> Result<Option<ResolvedItemRefWithDisplay>, CommandError> {
    let Some(value) = effective_value(arg, env_value().as_deref()) else {
        return Ok(None);
    };
    match resolver::resolve_epic_with_display(store, &value) {
        Ok(epic) => Ok(Some(epic)),
        Err(ResolveEpicError::NotFound) => Err(CommandError::failure(format!(
            "scope '{value}' is not a known Display ID or Alias"
        ))),
        Err(ResolveEpicError::NotAnEpic) => Err(CommandError::failure(format!(
            "scope '{value}' is not an Epic"
        ))),
        Err(ResolveEpicError::Storage(err)) => Err(resolver::storage_error(&err)),
    }
}

/// Resolve the active Scope, rendering any failure to `stderr` with the
/// `tk <command>:` prefix and returning [`Exit::Failure`]. Pre-seam shim over
/// [`resolve`] for commands not yet converted to the ADR-0032 seam.
pub fn resolve_rendered<W: Write + ?Sized>(
    store: &Store,
    stderr: &mut W,
    command: &str,
    arg: Option<&str>,
) -> Result<Option<ResolvedItemRefWithDisplay>, Exit> {
    resolve(store, arg).map_err(|err| {
        err.render(stderr, command);
        err.exit()
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_wins_over_environment() {
        assert_eq!(
            effective_value(Some("tk-1"), Some("tk-2")).as_deref(),
            Some("tk-1")
        );
    }

    #[test]
    fn falls_back_to_environment_when_no_argument() {
        assert_eq!(effective_value(None, Some("tk-2")).as_deref(), Some("tk-2"));
    }

    #[test]
    fn blank_environment_is_no_scope() {
        assert_eq!(effective_value(None, Some("   ")), None);
        assert_eq!(effective_value(None, Some("")), None);
    }

    #[test]
    fn no_argument_and_no_environment_is_no_scope() {
        assert_eq!(effective_value(None, None), None);
    }
}
