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

// `tk add` — local Ticket creation.

/// Stderr line when message input has no title after normalization.
pub const add_empty_message = "tk add: Aborting add due to empty message.";

/// Stderr line when message input contains a NUL byte.
pub const add_nul_message = "tk add: message contains NUL byte";

/// Stderr line when no Repository Store exists for the current repository.
pub const add_missing_store = "tk add: Repository Store not initialized; run 'tk init'";

/// Stderr prefix for message file read failures. Callers append
/// `"{s}: {s}\n"` for the user-typed path and error name.
pub const add_file_read_prefix = "tk add: could not read message file ";

/// Stderr prefix for stdin read failures. Callers append `"{s}\n"` for the
/// error name.
pub const add_stdin_read_prefix = "tk add: could not read message from stdin: ";

/// Stderr line for unexpected Repository Store write failures.
pub const add_create_failed_retry = "tk add: failed to create Ticket; retry the command";

/// Stderr line for busy/locked Repository Store writes.
pub const add_store_busy_retry = "tk add: Repository Store is busy; retry the command";

/// Stdout prefix for successful Ticket creation. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const add_created_ticket_prefix = "Created Ticket: ";

/// Stdout label for the created Ticket's Priority.
pub const add_priority_label = "Priority: ";

/// Stdout label for the created Ticket's Item Status.
pub const add_status_label = "Status: ";

// `tk list` — Repository Store reads.

/// Stderr line when no Repository Store exists for the current repository.
pub const list_missing_store = "tk list: Repository Store not initialized; run 'tk init'";

/// Stdout line for an empty default List Tree.
pub const list_empty_default = "No open or active items.";

/// Stdout line when `tk list --all` finds no items.
pub const list_empty_all = "No items.";

/// Stdout line when `tk list --ready` finds no ready items.
pub const list_empty_ready = "No ready items.";

/// Stdout line when `tk list --blocked` finds no blocked items.
pub const list_empty_blocked = "No blocked items.";

/// Stdout line when `tk list --active` finds no active items.
pub const list_empty_active = "No active items.";

/// Stdout line when an Origin-filtered list finds no local items.
pub const list_empty_local = "No local items.";

/// Stdout line when an Origin-filtered list finds no Remote-backed items.
pub const list_empty_remote = "No remote items.";

/// Stderr line when mutually exclusive readiness filters are combined.
pub const list_conflicting_readiness_filters = "tk list: choose at most one of --all, --ready, --blocked, or --active";

/// Stderr line when both Origin filters are combined.
pub const list_conflicting_origin_filters = "tk list: choose at most one of --local or --remote";

/// Stderr line for unexpected Repository Store read failures.
pub const list_read_failed_retry = "tk list: failed to read Repository Store; retry the command";

/// Stderr line for busy/locked Repository Store reads.
pub const list_store_busy_retry = "tk list: Repository Store is busy; retry the command";

/// Stdout label for the rendered-row count footer.
pub const list_total_label = "Total: ";

/// Stdout label for the Item Status legend footer.
pub const list_status_label = "Status: ";
