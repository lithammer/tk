//! Command-side Repository Store access: open once, resolve a Display ID or
//! Alias N times against that handle, and render the shared open / storage /
//! not-found diagnostics to the caller-owned stderr.
//!
//! This module is the command-side seam between "typed Repository Store
//! result" and "rendered failure + exit-1 signal". The store
//! (`store/repository.zig`) returns pure typed values — `OpenOutcome`,
//! `?ResolvedItemRef`, `ResolveEpicOutcome` — and never touches a writer;
//! the prologue that every item command used to copy-paste (the
//! `catch { renderStorageError } orelse { print not-found }` triad) lives
//! here behind one interface.
//!
//! Failure contract: `open` and every `resolve*` method render the failure to
//! stderr and return `null`; the caller does `… orelse return 1;`. Usage
//! errors (exit 2) happen before `open` and stay in the command. The composer
//! stops at "you have a resolved reference"; success and outcome rendering
//! (status transitions, dependency outcomes) stay command-owned.
//!
//! Message strings stay verbatim in `messages.zig` per
//! [ADR 0017](../../docs/adr/0017-keep-message-constants-verbatim.md);
//! commands pass them in by name, this module only composes and prints them.

const std = @import("std");
const Allocator = std.mem.Allocator;

const discovery = @import("../git/discovery.zig");
const messages = @import("../messages.zig");
const proc = @import("../proc/runner.zig");
const repository = @import("../store/repository.zig");

/// Not-found phrasing for a plain resolve. Rendered as
/// `print("{s}{s}{s}\n", .{ prefix, arg, suffix })`, byte-identical to the
/// pre-refactor `print(prefix ++ "{s}" ++ suffix ++ "\n", .{arg})`. The prefix
/// is per-call so a two-item command (`block`) can name the blocked vs the
/// blocking slot while sharing one suffix.
pub const NotFound = struct {
    prefix: []const u8,
    suffix: []const u8,
};

/// Not-found phrasing for an Epic resolve. A no-row miss and a wrong-class
/// miss say different things (`'…' is not a known Display ID or Alias` vs
/// `'…' is a Ticket, not an Epic`), so the Epic bundle carries both suffixes;
/// the prefix is shared.
pub const EpicNotFound = struct {
    prefix: []const u8,
    not_found_suffix: []const u8,
    not_epic_suffix: []const u8,
};

/// Command-prefixed message constants used by `renderStorageError`.
///
/// `fallback` is the generic non-transient diagnostic; the renderer appends
/// `"\n{s}\n"` for `@errorName(err)`. The three messages stay command-specific
/// so `messages.zig` remains the single source of truth for stable strings.
pub const StorageErrorMessages = struct {
    busy_retry: []const u8,
    out_of_memory: []const u8,
    fallback: []const u8,
};

/// Per-command message bundle for `open`. Each command declares one constant
/// at module level so the open-failure rendering pipeline is driven entirely
/// by the command's stable phrasing.
pub const OpenMessages = struct {
    /// Subcommand name as it appears in diagnostics, e.g. `"next"` or
    /// `"worktree set"`. Used by `renderOpenFailure` to format `tk <name>: …`.
    command_name: []const u8,
    /// Pre-formatted "Repository Store not initialized" line for this command.
    missing_store: []const u8,
    /// Storage-error triple used when `openExisting` raises an error.
    storage: StorageErrorMessages,
};

/// An opened Repository Store bound to the command's rendering context, ready
/// to resolve a Display ID or Alias N times against the same handle. The store
/// is open while this lives; call `close` exactly once (typically `defer`).
///
/// Built only via `open`, which has already rendered any open/storage failure
/// to stderr. Commands that only open and read (e.g. `tk list`, `tk next`)
/// use `r.store` directly and never call a resolve method.
pub const Resolver = struct {
    store: repository.Store,
    gpa: Allocator,
    stderr: *std.Io.Writer,
    storage: StorageErrorMessages,

    /// Close the underlying Repository Store connection.
    pub fn close(self: Resolver) void {
        self.store.close();
    }

    /// Resolve a Display ID or Alias to any item (Ticket or Epic). On a store
    /// error the storage diagnostic is rendered; on a no-row miss the `nf`
    /// not-found line is rendered. Either way returns `null` (caller:
    /// `orelse return 1`). On success the caller owns the returned reference
    /// and frees it with `ref.deinit(gpa)`.
    pub fn resolve(self: Resolver, arg: []const u8, nf: NotFound) ?repository.ResolvedItemRef {
        const maybe = repository.resolveItemRef(self.store, self.gpa, arg) catch |err| {
            renderStorageError(self.stderr, err, self.storage);
            return null;
        };
        return maybe orelse {
            self.stderr.print("{s}{s}{s}\n", .{ nf.prefix, arg, nf.suffix }) catch {};
            return null;
        };
    }

    /// Resolve a Display ID or Alias that must refer to an Epic. A no-row miss
    /// renders `nf.not_found_suffix`; a Ticket (wrong class) renders
    /// `nf.not_epic_suffix`. The wrong-class payload is freed internally.
    pub fn resolveEpic(self: Resolver, arg: []const u8, nf: EpicNotFound) ?repository.ResolvedItemRef {
        const epic_outcome = repository.resolveAsEpic(self.store, self.gpa, arg) catch |err| {
            renderStorageError(self.stderr, err, self.storage);
            return null;
        };
        switch (epic_outcome) {
            .epic => |ref| return ref,
            .not_found => {
                self.stderr.print("{s}{s}{s}\n", .{ nf.prefix, arg, nf.not_found_suffix }) catch {};
                return null;
            },
            .not_an_epic => |ref| {
                ref.deinit(self.gpa);
                self.stderr.print("{s}{s}{s}\n", .{ nf.prefix, arg, nf.not_epic_suffix }) catch {};
                return null;
            },
        }
    }

    /// Epic resolution that also returns the current Display ID for success
    /// diagnostics. Failure rendering matches `resolveEpic`.
    pub fn resolveEpicWithDisplay(self: Resolver, arg: []const u8, nf: EpicNotFound) ?repository.ResolvedItemRefWithDisplay {
        const epic_outcome = repository.resolveAsEpicWithDisplay(self.store, self.gpa, arg) catch |err| {
            renderStorageError(self.stderr, err, self.storage);
            return null;
        };
        switch (epic_outcome) {
            .epic => |ref| return ref,
            .not_found => {
                self.stderr.print("{s}{s}{s}\n", .{ nf.prefix, arg, nf.not_found_suffix }) catch {};
                return null;
            },
            .not_an_epic => |ref| {
                ref.deinit(self.gpa);
                self.stderr.print("{s}{s}{s}\n", .{ nf.prefix, arg, nf.not_epic_suffix }) catch {};
                return null;
            },
        }
    }
};

/// Open the Repository Store for a command, rendering the standard
/// open-failure or storage-error diagnostic on any failure. Returns the bound
/// `Resolver` on success or `null` after the diagnostic is written; callers do
/// `open(...) orelse return 1;`.
///
/// `error.OutOfMemory` is rendered through `msgs.storage.out_of_memory` rather
/// than propagated. This matches every command's storage-error handling:
/// anticipated OOM exits 1 with a stable message; only unanticipated failures
/// reach the exit-3 catch-all in `main.zig`.
pub fn open(
    gpa: Allocator,
    runner: proc.Runner,
    cwd: std.Io.Dir,
    stderr: *std.Io.Writer,
    msgs: OpenMessages,
) ?Resolver {
    var outcome: repository.OpenOutcome = undefined;
    repository.openExisting(gpa, runner, cwd, &outcome) catch |err| {
        renderStorageError(stderr, err, msgs.storage);
        return null;
    };
    return switch (outcome) {
        .ok => |store| .{ .store = store, .gpa = gpa, .stderr = stderr, .storage = msgs.storage },
        else => {
            renderOpenFailure(stderr, gpa, msgs.command_name, msgs.missing_store, outcome);
            return null;
        },
    };
}

/// Render a non-`.ok` `OpenOutcome` as a command-prefixed stderr diagnostic.
///
/// The four failure arms share identical phrasing across commands; only the
/// command-prefixed missing-store sentence varies. Callers pass the
/// already-prefixed missing-store message (e.g. `messages.list_missing_store`)
/// so `messages.zig` remains the single source of stable strings.
pub fn renderOpenFailure(
    stderr: *std.Io.Writer,
    gpa: Allocator,
    command_name: []const u8,
    missing_store: []const u8,
    outcome: repository.OpenOutcome,
) void {
    switch (outcome) {
        .ok => unreachable,
        .discovery_failed => |inner| discovery.renderFailure(stderr, gpa, command_name, inner),
        .store_missing => stderr.print("{s}\n", .{missing_store}) catch {},
        .not_ticket_store => stderr.print("tk {s}: Repository Store is {s}\n", .{ command_name, messages.init_refuse_foreign }) catch {},
        .store_from_future_version => stderr.print("tk {s}: Repository Store was created by a {s}\n", .{ command_name, messages.init_refuse_future_version }) catch {},
    }
}

/// Render a Repository Store read or write failure as a stderr diagnostic.
///
/// Busy/locked errors and `error.OutOfMemory` get dedicated phrasing so we
/// only suggest a retry when one is plausible; everything else falls through
/// to `fallback ++ "\n{s}\n"` carrying the underlying `@errorName`.
pub fn renderStorageError(stderr: *std.Io.Writer, err: anyerror, msgs: StorageErrorMessages) void {
    if (isBusyError(err)) {
        stderr.print("{s}\n", .{msgs.busy_retry}) catch {};
        return;
    }
    if (err == error.OutOfMemory) {
        stderr.print("{s}\n", .{msgs.out_of_memory}) catch {};
        return;
    }
    stderr.print("{s}\n{s}\n", .{ msgs.fallback, @errorName(err) }) catch {};
}

/// Classify a Repository Store error as a transient SQLite busy/locked state
/// that a retry can clear. Shared by commands so the retry contract stays
/// uniform across writes and reads.
pub fn isBusyError(err: anyerror) bool {
    return switch (err) {
        error.Busy,
        error.BusyRecovery,
        error.BusySnapshot,
        error.BusyTimeout,
        error.Locked,
        error.LockedSharedCache,
        error.LockedVTab,
        => true,
        else => false,
    };
}
