//! Canonical user-observable phrasing for `tk` diagnostics and status output.
//!
//! Source-side call sites (in `src/commands/...` and `src/cli.zig`) build their
//! format strings by `++`-concatenating these constants with the formatting
//! suffix they need (e.g. `messages.init_success_fresh ++ "{s}\n"`). Tests that
//! check user-visible output reference the same constants, so the runtime
//! emission and the test substring stay lock-step.
//!
//! Names follow the domain vocabulary in CONTEXT.md (Repository Store, Ticket
//! version, etc.). Trailing whitespace inside a constant is preserved
//! intentionally — the source code relies on it to keep `print` format strings
//! comptime-known.

// `tk init` — Repository Store creation outcomes.

/// Stdout line emitted when a fresh Repository Store is created. The trailing
/// space is intentional: callers append `"{s}\n"` for the database path.
pub const init_success_fresh = "Initialized Repository Store at ";

/// Stdout line emitted when `tk init` is re-run on a Repository Store that
/// already belongs to Ticket. Trailing space, same reason as above.
pub const init_success_existing = "Repository Store already initialized at ";

/// Stderr fragment when the candidate path is a SQLite file written by
/// something other than Ticket. Embedded inside a longer diagnostic.
pub const init_refuse_foreign = "not a Ticket Repository Store";

/// Stderr fragment when an existing Repository Store was created by a Ticket
/// version newer than this build. Embedded inside a longer diagnostic.
pub const init_refuse_future_version = "newer Ticket version";

// `tk init` — Git discovery outcomes.

/// Stderr line used as a fallback when `git rev-parse` fails with empty stderr.
/// Git normally prints its own "not a git repository" diagnostic, which we
/// reuse verbatim; this constant covers only the empty-stderr fallback.
pub const init_outside_git_default = "not in a git repository";

/// Stderr line when `git` is not on PATH at all.
pub const init_git_missing = "git not found on PATH";

/// Stderr line when spawning `git` failed for a reason other than the binary
/// being missing (permission, fork failure, etc.).
pub const init_git_spawn_failed = "failed to invoke git";

/// Stderr line when `git rev-parse` exits zero but stdout cannot be parsed
/// into the expected `(common-dir, toplevel)` line pair.
pub const init_git_unparseable = "git produced unexpected rev-parse output";
