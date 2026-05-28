//! Canonical user-observable phrasing for `tk` diagnostics and status output.
//!
//! Ported verbatim from `src/messages.zig` per ADR-0017. The source-side call
//! sites concatenate these constants with formatting tails (e.g.
//! `"{INIT_SUCCESS_FRESH}{db_path}\n"`); tests that pin user-visible output
//! reference the same constants, so runtime emission and test substring stay
//! lock-step.
//!
//! Names follow the domain vocabulary in CONTEXT.md (Repository Store,
//! Display ID, …). Trailing whitespace inside a constant is preserved
//! intentionally — call sites rely on it.
//!
//! Slice 0 ports the constants `tk init` and the shared git-discovery layer
//! need; the `GIT_*` group is intentionally not prefixed `INIT_GIT_*` because
//! every command that opens the Repository Store reuses them via
//! `git::discovery::render_failure`. Other commands port their command-
//! unique substrings as their slice lands.

// `tk init` — Repository Store creation outcomes.

/// Stdout line emitted when a fresh Repository Store is created. The trailing
/// space is intentional: callers append `"{db_path}\n"`.
pub const INIT_SUCCESS_FRESH: &str = "Initialized Repository Store at ";

/// Stdout line emitted when `tk init` is re-run on a Repository Store that
/// already belongs to tk. Trailing space, same reason as above.
pub const INIT_SUCCESS_EXISTING: &str = "Repository Store already initialized at ";

/// Stderr fragment when the candidate path is a SQLite file written by
/// something other than tk. Embedded inside a longer diagnostic.
pub const INIT_REFUSE_FOREIGN: &str = "not a tk Repository Store";

/// Stderr fragment when an existing Repository Store was created by a tk
/// version newer than this build. Embedded inside a longer diagnostic.
pub const INIT_REFUSE_FUTURE_VERSION: &str = "newer tk version";

// Git discovery outcomes. Shared by every command that opens the Repository
// Store via `git::discovery::render_failure` — `INIT_` prefix would falsely
// imply they're unique to `tk init`.

/// Stderr line used as a fallback when `git rev-parse` fails with empty stderr.
/// Git normally prints its own "not a git repository" diagnostic, which we
/// reuse verbatim; this constant covers only the empty-stderr fallback.
pub const GIT_OUTSIDE_DEFAULT: &str = "not in a git repository";

/// Stderr line when `git` is not on PATH at all.
pub const GIT_MISSING: &str = "git not found on PATH";

/// Stderr line when spawning `git` failed for a reason other than the binary
/// being missing (permission, fork failure, etc.).
pub const GIT_SPAWN_FAILED: &str = "failed to invoke git";

/// Stderr line when `git rev-parse` exits zero but stdout cannot be parsed
/// into the expected `(common-dir, toplevel)` line pair.
pub const GIT_UNPARSEABLE: &str = "git produced unexpected rev-parse output";

// `tk show` — single-item read.

/// Stderr line when no Repository Store exists for the current repository.
pub const SHOW_MISSING_STORE: &str = "tk show: Repository Store not initialized; run 'tk init'";

/// Stderr prefix for unknown Display ID or Alias. Callers append the supplied
/// id and [`SHOW_ID_NOT_FOUND_SUFFIX`] plus a newline.
pub const SHOW_ID_NOT_FOUND_PREFIX: &str = "tk show: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const SHOW_ID_NOT_FOUND_SUFFIX: &str = "' is not a known Display ID or Alias";

/// Stderr line when no positional id was supplied.
pub const SHOW_ID_REQUIRED: &str = "tk show: an item ID argument is required";

/// Stderr line for non-transient Repository Store read failures.
pub const SHOW_READ_FAILED: &str = "tk show: failed to read Repository Store";

/// Stderr line for busy/locked Repository Store reads.
pub const SHOW_STORE_BUSY_RETRY: &str = "tk show: Repository Store is busy; retry the command";

// Section headers for `tk show` output.

pub const SHOW_SECTION_DESCRIPTION: &str = "DESCRIPTION";
pub const SHOW_SECTION_PARENT: &str = "PARENT";
pub const SHOW_SECTION_TICKETS: &str = "TICKETS";
pub const SHOW_SECTION_BLOCKED_BY: &str = "BLOCKED BY";
pub const SHOW_SECTION_BLOCKING: &str = "BLOCKING";
pub const SHOW_SECTION_EXTERNAL_BLOCKERS: &str = "EXTERNAL BLOCKERS";
