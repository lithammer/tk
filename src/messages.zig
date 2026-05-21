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
/// already belongs to tk. Trailing space, same reason as above.
pub const init_success_existing = "Repository Store already initialized at ";

/// Stderr fragment when the candidate path is a SQLite file written by
/// something other than tk. Embedded inside a longer diagnostic.
pub const init_refuse_foreign = "not a tk Repository Store";

/// Stderr fragment when an existing Repository Store was created by a tk
/// version newer than this build. Embedded inside a longer diagnostic.
pub const init_refuse_future_version = "newer tk version";

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

/// Stderr line when both message input mechanisms are supplied.
pub const add_conflicting_message_flags = "tk add: choose at most one of -m/--message or -F/--file";

/// Stderr line when more than one message file is supplied.
pub const add_repeated_file_flags = "tk add: choose at most one -F/--file";

/// Stderr line when neither non-interactive message input mechanism is supplied.
pub const add_message_required = "tk add: one of -m/--message or -F/--file is required";

/// Stderr line when more than one item-class flag is supplied.
pub const add_conflicting_class_flags = "tk add: choose at most one of --bug or --epic";

/// Stderr line when `--priority` is supplied for an Epic.
pub const add_priority_on_epic = "tk add: --priority cannot be set on an Epic";

/// Stderr line when `--parent` is supplied while creating an Epic.
pub const add_parent_on_epic = "tk add: --parent is only valid for Tickets";

/// Stderr prefix for any `--parent` diagnostic (unknown id or wrong class).
pub const add_parent_prefix = "tk add: parent '";

/// Stderr suffix for an unknown `--parent` Display ID.
pub const add_parent_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr suffix when `--parent` resolves to a Ticket instead of an Epic.
pub const add_parent_not_epic_suffix = "' is a Ticket, not an Epic";

/// Stderr prefix for message file read failures. Callers append
/// `"{s}: {s}\n"` for the user-typed path and error name.
pub const add_file_read_prefix = "tk add: could not read message file ";

/// Stderr prefix for stdin read failures. Callers append `"{s}\n"` for the
/// error name.
pub const add_stdin_read_prefix = "tk add: could not read message from stdin: ";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`. Distinct from the busy
/// and out-of-memory variants so we only suggest a retry when one is plausible.
pub const add_create_failed = "tk add: failed to create item";

/// Stderr line when allocation fails during a `tk add` Repository Store write.
pub const add_out_of_memory = "tk add: out of memory";

/// Stderr line for busy/locked Repository Store writes.
pub const add_store_busy_retry = "tk add: Repository Store is busy; retry the command";

/// Stdout prefix for successful Ticket creation. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const add_created_ticket_prefix = "Created Ticket: ";

/// Stdout prefix for successful Epic creation. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const add_created_epic_prefix = "Created Epic: ";

/// Stdout label for the created Ticket's Ticket Kind.
pub const add_kind_label = "Kind: ";

/// Stdout label for the created Ticket's Priority.
pub const add_priority_label = "Priority: ";

/// Stdout label for the created Ticket's Item Status.
pub const add_status_label = "Status: ";

/// Stdout label for a created Ticket's containing Epic.
pub const add_parent_label = "Parent: ";

// `tk list` — Repository Store reads.

/// Stderr line when no Repository Store exists for the current repository.
pub const list_missing_store = "tk list: Repository Store not initialized; run 'tk init'";

/// Stdout line for an empty default List Tree.
pub const list_empty_default = "No open or active items.";

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
pub const list_conflicting_readiness_filters = "tk list: choose at most one of --ready, --blocked, or --active";

/// Stderr line when both Origin filters are combined.
pub const list_conflicting_origin_filters = "tk list: choose at most one of --local or --remote";

/// Stderr line for non-transient Repository Store read failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`. Distinct from the busy
/// and out-of-memory variants so we only suggest a retry when one is plausible.
pub const list_read_failed = "tk list: failed to read Repository Store";

/// Stderr line when allocation fails during a `tk list` Repository Store read.
pub const list_out_of_memory = "tk list: out of memory";

/// Stderr line for busy/locked Repository Store reads.
pub const list_store_busy_retry = "tk list: Repository Store is busy; retry the command";

/// Stdout label for the rendered-row count footer.
pub const list_total_label = "Total: ";

/// Stdout label for the Item Status legend footer.
pub const list_status_label = "Status: ";

// `tk show` — single-item read.

/// Stderr line when no Repository Store exists for the current repository.
pub const show_missing_store = "tk show: Repository Store not initialized; run 'tk init'";

/// Stderr prefix for unknown Display ID or Alias. Callers append
/// `"{s}" ++ show_id_not_found_suffix ++ "\n"` for the supplied id.
pub const show_id_not_found_prefix = "tk show: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const show_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line when no positional id was supplied.
pub const show_id_required = "tk show: an item ID argument is required";

/// Stderr line for non-transient Repository Store read failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const show_read_failed = "tk show: failed to read Repository Store";

/// Stderr line when allocation fails during a `tk show` Repository Store read.
pub const show_out_of_memory = "tk show: out of memory";

/// Stderr line for busy/locked Repository Store reads.
pub const show_store_busy_retry = "tk show: Repository Store is busy; retry the command";

// `tk show` — section headers.

/// Section header for the item description body.
pub const show_section_description = "DESCRIPTION";

/// Section header for the Epic parent of a Ticket.
pub const show_section_parent = "PARENT";

/// Section header for children of an Epic.
pub const show_section_tickets = "TICKETS";

/// Section header for unresolved items blocking this item.
pub const show_section_blocked_by = "BLOCKED BY";

/// Section header for items this item is blocking.
pub const show_section_blocking = "BLOCKING";

/// Section header for unresolved external blockers.
pub const show_section_external_blockers = "EXTERNAL BLOCKERS";

// `tk next` — Repository Store reads.

/// Stderr line when no Repository Store exists for the current repository.
pub const next_missing_store = "tk next: Repository Store not initialized; run 'tk init'";

/// Stderr line when no ready Ticket exists in repository-wide selection.
pub const next_no_ready_ticket = "tk next: no ready Tickets";

/// Stderr line when no ready Ticket matches the active Workspace Scope.
pub const next_no_ready_ticket_in_scope = "tk next: no ready Tickets in Workspace Scope";

/// Stderr line when Workspace Scope no longer resolves to a Ticket or Epic.
pub const next_scope_not_found = "tk next: Workspace Scope does not resolve to a Ticket or Epic";

/// Stderr line when `tk.scope` holds a value that no longer matches any
/// `item_ids` row. The caller prints this with the stored value substituted.
pub const next_scope_unresolved_prefix = "tk next: Workspace Scope '";
pub const next_scope_unresolved_suffix = "' is not a known Display ID or Alias";

/// Stderr line for non-transient Repository Store read failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const next_read_failed = "tk next: failed to read Repository Store";

/// Stderr line when allocation fails during a `tk next` Repository Store read.
pub const next_out_of_memory = "tk next: out of memory";

/// Stderr line for busy/locked Repository Store reads.
pub const next_store_busy_retry = "tk next: Repository Store is busy; retry the command";

// `tk update` — current-state and Mutation Log write.

/// Stderr line when no Repository Store exists for the current repository.
pub const update_missing_store = "tk update: Repository Store not initialized; run 'tk init'";

/// Stderr prefix for unknown Display ID or Alias. Callers append
/// `"{s}" ++ update_id_not_found_suffix ++ "\n"` for the supplied id.
pub const update_id_not_found_prefix = "tk update: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const update_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line when no positional id was supplied.
pub const update_id_required = "tk update: an item ID argument is required";

/// Stderr line when `--priority` is supplied for an Epic (Epics have no
/// Priority field).
pub const update_priority_on_epic = "tk update: --priority cannot be set on an Epic";

/// Stderr line when `--parent` or `--no-parent` is supplied for an Epic
/// (container reassignment is only valid for Tickets).
pub const update_parent_on_epic = "tk update: --parent and --no-parent are only valid for Tickets";

/// Stderr line when both `--parent` and `--no-parent` are supplied.
pub const update_conflicting_parent_flags = "tk update: choose at most one of --parent or --no-parent";

/// Stderr line when `-m` and `-F` are both supplied.
pub const update_conflicting_message_flags = "tk update: choose at most one of -m/--message or -F/--file";

/// Stderr prefix for any `--parent` diagnostic (unknown id or wrong class).
/// Callers append `"{s}" ++ <variant suffix> ++ "\n"` for the supplied id.
pub const update_parent_prefix = "tk update: parent '";

/// Stderr suffix for an unknown `--parent` Display ID.
pub const update_parent_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr suffix when `--parent` resolves to a Ticket instead of an Epic.
pub const update_parent_not_epic_suffix = "' is a Ticket, not an Epic";

/// Stderr prefix for message file read failures. Callers append
/// `"{s}: {s}\n"` for the user-typed path and error name.
pub const update_file_read_prefix = "tk update: could not read message file ";

/// Stderr prefix for stdin read failures. Callers append `"{s}\n"` for the
/// error name.
pub const update_stdin_read_prefix = "tk update: could not read message from stdin: ";

/// Stderr line when message input has no title after normalization.
pub const update_empty_message = "tk update: Aborting update due to empty message.";

/// Stderr line when message input contains a NUL byte.
pub const update_nul_message = "tk update: message contains NUL byte";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const update_write_failed = "tk update: failed to update item";

/// Stderr line when allocation fails during a `tk update` Repository Store
/// write.
pub const update_out_of_memory = "tk update: out of memory";

/// Stderr line for busy/locked Repository Store writes.
pub const update_store_busy_retry = "tk update: Repository Store is busy; retry the command";

/// Stdout prefix for a successful Ticket update. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const update_success_ticket_prefix = "Updated Ticket: ";

/// Stdout prefix for a successful Epic update. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const update_success_epic_prefix = "Updated Epic: ";

/// Stderr line when `tk update` is invoked with no editing intent. At least
/// one of `-m`, `-F`, `--priority`, `--parent`, or `--no-parent` is required.
pub const update_no_changes_requested = "tk update: at least one of -m, -F, --priority, --parent, or --no-parent is required";

// `tk done` — minimum lifecycle status write.

/// Stderr line when no Repository Store exists for the current repository.
pub const done_missing_store = "tk done: Repository Store not initialized; run 'tk init'";

/// Stderr line when no positional id was supplied.
pub const done_id_required = "tk done: an item ID argument is required";

/// Stderr prefix for unknown Display ID or Alias. Callers append
/// `"{s}" ++ done_id_not_found_suffix ++ "\n"` for the supplied id.
pub const done_id_not_found_prefix = "tk done: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const done_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line for busy/locked Repository Store writes.
pub const done_store_busy_retry = "tk done: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk done` Repository Store write.
pub const done_out_of_memory = "tk done: out of memory";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const done_write_failed = "tk done: failed to mark item done";

/// Stdout prefix for a successful Ticket status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const done_success_ticket_prefix = "Marked Ticket done: ";

/// Stdout prefix for a successful Epic status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const done_success_epic_prefix = "Marked Epic done: ";

// `tk start` — symmetric lifecycle status write to `active`.

/// Stderr line when no Repository Store exists for the current repository.
pub const start_missing_store = "tk start: Repository Store not initialized; run 'tk init'";

/// Stderr line when no positional id was supplied.
pub const start_id_required = "tk start: an item ID argument is required";

/// Stderr prefix for unknown Display ID or Alias. Callers append
/// `"{s}" ++ start_id_not_found_suffix ++ "\n"` for the supplied id.
pub const start_id_not_found_prefix = "tk start: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const start_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line for busy/locked Repository Store writes.
pub const start_store_busy_retry = "tk start: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk start` Repository Store
/// write.
pub const start_out_of_memory = "tk start: out of memory";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const start_write_failed = "tk start: failed to mark item active";

/// Stdout prefix for a successful Ticket status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const start_success_ticket_prefix = "Marked Ticket active: ";

/// Stdout prefix for a successful Epic status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const start_success_epic_prefix = "Marked Epic active: ";

/// Stderr line when `tk start` refuses a done Ticket. ADR 0006: done is
/// terminal in v1.
pub const start_locked_done_ticket = "tk start: cannot start a done Ticket";

/// Stderr line when `tk start` refuses a done Epic. ADR 0006: done is
/// terminal in v1.
pub const start_locked_done_epic = "tk start: cannot start a done Epic";

// `tk stop` — symmetric lifecycle status write back to `open`.

/// Stderr line when no Repository Store exists for the current repository.
pub const stop_missing_store = "tk stop: Repository Store not initialized; run 'tk init'";

/// Stderr line when no positional id was supplied.
pub const stop_id_required = "tk stop: an item ID argument is required";

/// Stderr prefix for unknown Display ID or Alias. Callers append
/// `"{s}" ++ stop_id_not_found_suffix ++ "\n"` for the supplied id.
pub const stop_id_not_found_prefix = "tk stop: '";

/// Stderr suffix for unknown Display ID or Alias.
pub const stop_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line for busy/locked Repository Store writes.
pub const stop_store_busy_retry = "tk stop: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk stop` Repository Store
/// write.
pub const stop_out_of_memory = "tk stop: out of memory";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const stop_write_failed = "tk stop: failed to mark item open";

/// Stdout prefix for a successful Ticket status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const stop_success_ticket_prefix = "Marked Ticket open: ";

/// Stdout prefix for a successful Epic status write. Callers append
/// `"{s} - {s}\n"` for Display ID and title.
pub const stop_success_epic_prefix = "Marked Epic open: ";

/// Stderr line when `tk stop` refuses a done Ticket. ADR 0006: done is
/// terminal in v1.
pub const stop_locked_done_ticket = "tk stop: cannot stop a done Ticket";

/// Stderr line when `tk stop` refuses a done Epic. ADR 0006: done is
/// terminal in v1.
pub const stop_locked_done_epic = "tk stop: cannot stop a done Epic";

// `tk block` — Dependency creation.

/// Stderr line when no Repository Store exists for the current repository.
pub const block_missing_store = "tk block: Repository Store not initialized; run 'tk init'";

/// Stderr line when either Dependency argument is missing.
pub const block_args_required = "tk block: blocked and blocking item ID arguments are required";

/// Stderr prefix for an unknown Blocked Item Display ID or Alias. Callers
/// append `"{s}" ++ block_item_not_found_suffix ++ "\n"` for the supplied id.
pub const block_blocked_not_found_prefix = "tk block: blocked item '";

/// Stderr prefix for an unknown Blocking Item Display ID or Alias. Callers
/// append `"{s}" ++ block_item_not_found_suffix ++ "\n"` for the supplied id.
pub const block_blocking_not_found_prefix = "tk block: blocking item '";

/// Stderr suffix shared by role-specific unknown-ID diagnostics.
pub const block_item_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line when the Blocked Item and Blocking Item resolve to the same
/// Ticket or Epic.
pub const block_self_dependency = "tk block: an item cannot depend on itself";

/// Stderr prefix when the Blocked Item is already done. Callers append
/// `"{s}' is done\n"` for the supplied Display ID or Alias.
pub const block_blocked_done_prefix = "tk block: blocked item '";

/// Stderr prefix when the Blocking Item is already done. Callers append
/// `"{s}' is done\n"` for the supplied Display ID or Alias.
pub const block_blocking_done_prefix = "tk block: blocking item '";

/// Stderr line when a new Dependency would introduce a cycle.
pub const block_dependency_cycle = "tk block: Dependency would create a cycle";

/// Stderr prefix when a Backend Blocked Item references a Local Blocking Item.
/// Callers append `"{blocked}' cannot depend on Local blocking item '{blocking}'\n"`.
pub const block_backend_blocked_local_blocking_prefix = "tk block: Backend blocked item '";

/// Stderr prefix when a Backend Blocked Item references a Blocking Item from
/// another Backend kind. Callers append
/// `"{blocked}' cannot depend on blocking item '{blocking}' from another Backend kind\n"`.
pub const block_backend_kind_mismatch_prefix = "tk block: Backend blocked item '";

/// Stderr line for busy/locked Repository Store writes.
pub const block_store_busy_retry = "tk block: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk block` Repository Store
/// write.
pub const block_out_of_memory = "tk block: out of memory";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const block_write_failed = "tk block: failed to add Dependency";

/// Stdout prefix for successful Dependency creation. Callers append
/// `"{s} blocked by {s}\n"` for the Blocked and Blocking Display IDs.
pub const block_success_prefix = "Added Dependency: ";

// `tk unblock` — Dependency removal.

/// Stderr line when no Repository Store exists for the current repository.
pub const unblock_missing_store = "tk unblock: Repository Store not initialized; run 'tk init'";

/// Stderr line when either Dependency argument is missing.
pub const unblock_args_required = "tk unblock: blocked and blocking item ID arguments are required";

/// Stderr prefix for an unknown Blocked Item Display ID or Alias. Callers
/// append `"{s}" ++ unblock_item_not_found_suffix ++ "\n"` for the supplied id.
pub const unblock_blocked_not_found_prefix = "tk unblock: blocked item '";

/// Stderr prefix for an unknown Blocking Item Display ID or Alias. Callers
/// append `"{s}" ++ unblock_item_not_found_suffix ++ "\n"` for the supplied id.
pub const unblock_blocking_not_found_prefix = "tk unblock: blocking item '";

/// Stderr suffix shared by role-specific unknown-ID diagnostics.
pub const unblock_item_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line when the Blocked Item and Blocking Item resolve to the same
/// Ticket or Epic.
pub const unblock_self_dependency = "tk unblock: an item cannot depend on itself";

/// Stderr line for busy/locked Repository Store writes.
pub const unblock_store_busy_retry = "tk unblock: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk unblock` Repository Store
/// write.
pub const unblock_out_of_memory = "tk unblock: out of memory";

/// Stderr line for non-transient Repository Store write failures. The caller
/// appends `"\n{s}\n"` for the underlying `@errorName`.
pub const unblock_write_failed = "tk unblock: failed to remove Dependency";

/// Stdout prefix for successful Dependency removal. Callers append
/// `"{s} no longer blocked by {s}\n"` for the Blocked and Blocking Display IDs.
pub const unblock_success_prefix = "Removed Dependency: ";

// `tk worktree` and subcommands.

/// Stdout line printed by `tk worktree clear` on success and on the idempotent
/// no-op when no `tk.scope` was configured.
pub const worktree_cleared = "Workspace Scope cleared";

/// Stderr line for non-transient git failures during `tk worktree clear`. The
/// caller appends `"\n{s}\n"` for the underlying `@errorName`.
pub const worktree_clear_failed = "tk worktree clear: failed to update git worktree config";

/// Stdout prefix for successful `tk worktree set`. Callers append the
/// user-supplied Display ID or Alias.
pub const worktree_set_prefix = "Set Workspace Scope to ";

/// Stderr line when `tk worktree set` is called without an `<id>` positional.
pub const worktree_set_id_required = "tk worktree set: missing required <id>; usage: tk worktree set <id>";

/// Stderr prefix when the supplied id does not resolve through `item_ids`.
/// Callers append `"{s}" ++ worktree_set_id_not_found_suffix ++ "\n"` for the
/// supplied id.
pub const worktree_set_id_not_found_prefix = "tk worktree set: '";

/// Stderr suffix for the unknown-id diagnostic.
pub const worktree_set_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr line for non-transient git failures while writing the config. The
/// caller appends `"\n{s}\n"` for the underlying `@errorName`.
pub const worktree_set_failed = "tk worktree set: failed to update git worktree config";

/// Stderr line for a missing Repository Store during `tk worktree set`.
pub const worktree_set_missing_store = "tk worktree set: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk worktree set`.
pub const worktree_set_store_busy_retry = "tk worktree set: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk worktree set` read.
pub const worktree_set_out_of_memory = "tk worktree set: out of memory";

/// Stderr line for non-transient Repository Store read failures during
/// `tk worktree set`. Caller appends `"\n{s}\n"` for the underlying
/// `@errorName`.
pub const worktree_set_read_failed = "tk worktree set: failed to read Repository Store";

/// Stdout line printed by `tk worktree` (no subcommand) when no Workspace
/// Scope is configured or inferred.
pub const worktree_no_scope = "No Workspace Scope.";

/// Stderr prefix for a stored `tk.scope` value that no longer resolves. The
/// caller appends `"{s}" ++ worktree_status_unresolved_suffix ++ "\n"` for the
/// stored value.
pub const worktree_status_unresolved_prefix = "tk worktree: Workspace Scope '";

/// Stderr suffix for the unresolved-stored-value diagnostic.
pub const worktree_status_unresolved_suffix = "' is not a known Display ID or Alias";

/// Stderr line for a missing Repository Store during `tk worktree` status.
pub const worktree_status_missing_store = "tk worktree: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk worktree` status.
pub const worktree_status_store_busy_retry = "tk worktree: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk worktree` status read.
pub const worktree_status_out_of_memory = "tk worktree: out of memory";

/// Stderr line for non-transient Repository Store read failures during
/// `tk worktree` status.
pub const worktree_status_read_failed = "tk worktree: failed to read Repository Store";

/// Stderr line when `tk worktree start` is called without an `<id>` positional.
pub const worktree_start_id_required = "tk worktree start: missing required <id>; usage: tk worktree start <id> [path] [--no-status]";

/// Stderr prefix when the supplied id does not resolve.
pub const worktree_start_id_not_found_prefix = "tk worktree start: '";

/// Stderr suffix shared with `worktree set` for unknown-id diagnostics.
pub const worktree_start_id_not_found_suffix = "' is not a known Display ID or Alias";

/// Stderr prefix for an attempt to start a `done` Ticket. Caller appends
/// `"Ticket\n"` or `"Epic\n"` based on the resolved item class.
pub const worktree_start_locked_done_prefix = "tk worktree start: cannot start a done ";

/// Stderr line for non-transient git failures during `tk worktree start`.
pub const worktree_start_git_failed = "tk worktree start: failed to create worktree";

/// Stdout prefix for the success header. Caller appends
/// `"<Ticket|Epic>: <display-id> - <title>\n"`.
pub const worktree_start_success_prefix = "Created worktree for ";

/// Stderr line for a missing Repository Store during `tk worktree start`.
pub const worktree_start_missing_store = "tk worktree start: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk worktree start`.
pub const worktree_start_store_busy_retry = "tk worktree start: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk worktree start` write.
pub const worktree_start_out_of_memory = "tk worktree start: out of memory";

/// Stderr line for non-transient Repository Store write failures.
pub const worktree_start_write_failed = "tk worktree start: failed to update Repository Store";

// `tk remote` — Remote configuration commands.

/// Stdout line when no Remote is configured.
pub const remote_status_none = "No Remote configured.";

/// Stderr line for a missing Repository Store during `tk remote`.
pub const remote_status_missing_store = "tk remote: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk remote`.
pub const remote_status_store_busy_retry = "tk remote: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk remote` read.
pub const remote_status_out_of_memory = "tk remote: out of memory";

/// Stderr line for non-transient Repository Store read failures.
pub const remote_status_read_failed = "tk remote: failed to read Repository Store";

/// Stderr line when `tk remote set` is given an unknown backend kind.
pub const remote_set_unknown_kind_prefix = "tk remote set: unknown backend kind '";

/// Stderr suffix listing the known kinds.
pub const remote_set_unknown_kind_suffix = "'; expected 'github' or 'jira'";

/// Stderr line when required flags are missing for `tk remote set github`.
pub const remote_set_github_repo_required = "tk remote set github: missing required --repo <owner/name>";

/// Stderr line when --repo is malformed.
pub const remote_set_github_repo_malformed = "tk remote set github: --repo must be 'owner/name'";

/// Stderr line when required flags are missing for `tk remote set jira`.
pub const remote_set_jira_required = "tk remote set jira: missing required --site <url> and --project <key>";

/// Stderr prefix when the local Display ID prefix collides with the adapter's
/// namespace. Caller appends the local prefix.
pub const remote_set_prefix_collision_prefix = "tk remote set: local Display ID prefix '";

/// Stderr suffix shared with the prefix-collision diagnostic.
pub const remote_set_prefix_collision_suffix = "' collides with the adapter namespace; configurable prefix is tracked by tk-22";

/// Stderr line for a missing Repository Store during `tk remote set`.
pub const remote_set_missing_store = "tk remote set: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk remote set`.
pub const remote_set_store_busy_retry = "tk remote set: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk remote set` write.
pub const remote_set_out_of_memory = "tk remote set: out of memory";

/// Stderr line for non-transient Repository Store write failures during set.
pub const remote_set_write_failed = "tk remote set: failed to update Repository Store";

/// Stdout prefix when set succeeds. Caller appends e.g. "github (owner/repo)\n".
pub const remote_set_success_prefix = "Configured Remote: ";

/// Stderr prefix when clear is refused because pending or failed mutations exist.
/// Caller appends the count number.
pub const remote_clear_refused_prefix = "tk remote clear: ";

/// Stderr suffix shared with the clear-refusal diagnostic.
pub const remote_clear_refused_suffix = " Mutation(s) are pending or failed; run `tk sync` to apply them or `tk sync --skip <id>` to discard each one before clearing the Remote.";

/// Stderr line for a missing Repository Store during `tk remote clear`.
pub const remote_clear_missing_store = "tk remote clear: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk remote clear`.
pub const remote_clear_store_busy_retry = "tk remote clear: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during a `tk remote clear` write.
pub const remote_clear_out_of_memory = "tk remote clear: out of memory";

/// Stderr line for non-transient Repository Store write failures during clear.
pub const remote_clear_write_failed = "tk remote clear: failed to update Repository Store";

/// Stdout line on clear success.
pub const remote_clear_success = "Cleared Remote configuration.";

// `tk sync` — Mutation Log replay.

/// Stderr line when `tk sync` runs against an empty Remote configuration.
pub const sync_no_remote = "tk sync: no Remote configured; run 'tk remote set <kind>' first";

/// Stderr line when the configured Remote's adapter is not implemented.
pub const sync_adapter_not_implemented = "tk sync: the configured Remote's adapter is not implemented in this build";

/// Stderr line when `--skip` is given without an argument.
pub const sync_skip_requires_arg = "tk sync: --skip requires a Mutation Sequence";

/// Stderr line when `--skip` argument is not an integer.
pub const sync_skip_not_integer = "tk sync: --skip argument must be an integer";

/// Stderr prefix when `tk sync` receives an unknown argument. Caller appends
/// the argument and a newline.
pub const sync_unknown_arg_prefix = "tk sync: unknown argument '";

/// Stderr suffix for the unknown-argument diagnostic.
pub const sync_unknown_arg_suffix = "'";

/// Stderr prefix when a Mutation Pull fails. Caller appends the Diagnostic
/// message captured from the adapter or the SQL layer.
pub const sync_failure_prefix = "tk sync: ";

/// Stderr line for a missing Repository Store during `tk sync`.
pub const sync_missing_store = "tk sync: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk sync`.
pub const sync_store_busy_retry = "tk sync: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during `tk sync`.
pub const sync_out_of_memory = "tk sync: out of memory";

/// Stderr line for non-transient Repository Store failures during sync.
pub const sync_storage_failed = "tk sync: failed to update Repository Store";

/// Stderr prefix when a Pull or Apply collides with an existing Display ID.
/// Caller appends the colliding ID and a closing-quote-plus-message.
pub const sync_display_id_collision_prefix = "tk sync: Display ID '";

/// Stderr suffix shared with the Display ID collision diagnostic.
pub const sync_display_id_collision_suffix = "' already claimed by an existing Item";

/// Stderr prefix for the --skip wrong-state diagnostic. Caller appends the
/// target sequence and a closing message.
pub const sync_skip_not_failed_prefix = "tk sync --skip: Mutation ";

/// Stderr suffix for the --skip wrong-state diagnostic.
pub const sync_skip_not_failed_suffix = " is not in the failed state; --skip only abandons failed Mutations";

/// Stderr prefix for the --skip missing-sequence diagnostic.
pub const sync_skip_not_found_prefix = "tk sync --skip: Mutation ";

/// Stderr suffix for the --skip missing-sequence diagnostic.
pub const sync_skip_not_found_suffix = " not found";

/// Stderr line for schema-drift or enum-drift errors during sync.
pub const sync_schema_drift = "tk sync: Mutation Log row has an unrecognised mutation kind; this is a Ticket bug — please report it";

/// Stdout prefix for a successful sync summary. Caller appends counts.
pub const sync_summary_prefix = "Sync complete: ";

// `tk sync log` — read view.

/// Stdout line when the Mutation Log contains nothing matching the filter.
pub const sync_log_empty_default = "No Mutations recorded.";

/// Stdout line for the empty `--pending` filter.
pub const sync_log_empty_pending = "No pending Mutations.";

/// Stdout line for the empty `--failed` filter.
pub const sync_log_empty_failed = "No failed Mutations.";

/// Stdout line for the empty `--skipped` filter.
pub const sync_log_empty_skipped = "No skipped Mutations.";

/// Stderr prefix for `tk sync log <id>` when the sequence is missing.
pub const sync_log_not_found_prefix = "tk sync log: Mutation ";

/// Stderr suffix for the not-found diagnostic.
pub const sync_log_not_found_suffix = " not found";

/// Stderr line when `tk sync log <id>` argument is not a number.
pub const sync_log_id_not_numeric = "tk sync log: <id> must be a Mutation Sequence (use the integer from the list view)";

/// Stderr line for a missing Repository Store during `tk sync log`.
pub const sync_log_missing_store = "tk sync log: Repository Store not initialized; run 'tk init'";

/// Stderr line for busy/locked Repository Store during `tk sync log`.
pub const sync_log_store_busy_retry = "tk sync log: Repository Store is busy; retry the command";

/// Stderr line when allocation fails during `tk sync log`.
pub const sync_log_out_of_memory = "tk sync log: out of memory";

/// Stderr line for non-transient Repository Store failures during sync log.
pub const sync_log_storage_failed = "tk sync log: failed to read Repository Store";

// `tk manpage` — print or install the embedded `tk(1)` manpage.

/// Stdout prefix for a successful `tk manpage --install`. Callers append the
/// installed path followed by `\n`.
pub const manpage_install_success = "Installed manpage at ";

/// Stderr prefix for a target-path install failure (a rename/write/openDir
/// failure where the target path was successfully computed). Callers append
/// the target path, `": "`, an OS error reason, and
/// `manpage_install_failure_suffix`. The "left unchanged" clause in the
/// suffix is part of the contract: the staged tmp file is removed
/// best-effort, but the existing target is never deleted.
pub const manpage_install_failure_prefix = "tk manpage: install failed at ";

/// Trailing fragment appended to every target-path install-failure line so
/// the user knows the existing file at the path was not modified.
pub const manpage_install_failure_suffix = "; existing file (if any) left unchanged; remove manually if it is stale";

/// Stderr prefix for an install failure that happened before any target path
/// was computed (executable-path resolution failed). Callers append a
/// reason and a trailing newline. No "left unchanged" suffix because no
/// target was identified.
pub const manpage_install_exe_resolve_failure_prefix = "tk manpage: install failed: cannot resolve executable path: ";

/// Stderr line printed when `tk manpage --install` runs on Windows. Callers
/// append a trailing newline, matching the rest of the file's convention.
pub const manpage_skip_windows = "tk manpage: skipping install on Windows";
