//! Backend Adapter trait.
//!
//! The type-erased `Adapter` struct mirrors `proc.Runner` (see
//! `src/proc/runner.zig`): a `*anyopaque` context and a `VTable` of function
//! pointers, with thin `pub` methods that thunk through the vtable. Concrete
//! adapters (`github`, `jira`, and the test `FakeAdapter`) plug into this
//! trait via `adapter()` helpers exactly like `RealRunner` / `FakeRunner` do
//! for `Runner`.
//!
//! The contract types this trait exchanges with callers — `MutationView`,
//! `BackendItemSnapshot`, `Outcome`, `Receipt`, `Failure`, `MutationPayload` —
//! live in `src/domain/` because they are pure data shared by `store/`,
//! `remote/`, and the future `sync/` engine. This module keeps only the trait
//! itself plus its environment-failure error sets (which are inherently tied
//! to the `proc.Runner` boundary every v1 adapter uses).

const std = @import("std");
const Allocator = std.mem.Allocator;

const Diagnostic = @import("../store/diagnostic.zig").Diagnostic;
const RunnerError = @import("../proc/runner.zig").Error;
const BackendItemSnapshot = @import("../domain/backend_item_snapshot.zig").BackendItemSnapshot;
const MutationView = @import("../domain/mutation_view.zig").MutationView;
const Outcome = @import("../domain/outcome.zig").Outcome;

/// Error set returned by `Adapter.applyMutation`.
///
/// Aliased to `proc.Runner.Error` because every v1 adapter (GitHub via `gh`,
/// Jira via `acli`) reaches the backend through the injectable subprocess
/// runner — the env-failure vocabulary is therefore exactly the runner's
/// (`ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`). Mutation-level
/// rejection — non-zero exit, refused write, validation failure — flows
/// through the typed `Outcome.failure` arm so the engine can persist the
/// failure record in `mutations.failure_json` without conflating it with
/// adapter unavailability.
pub const ApplyError = RunnerError;

/// Error set returned by `Adapter.pullBackendItems`.
///
/// Extends `proc.Runner.Error` with `PullFailed` for adapter-level rejection
/// (the CLI ran but exited non-zero). `PullFailed` is paired with the
/// optional `?*Diagnostic` out-param: the adapter captures CLI stderr into
/// the diagnostic before returning the bare tag. The engine renders
/// `diag.message()` and stops sync — pull is all-or-nothing in v1 because
/// the snapshot model assumes a consistent backend view.
pub const PullError = RunnerError || error{PullFailed};

/// Type-erased Backend Adapter.
///
/// Modelled on `proc.Runner` (see `src/proc/runner.zig`): commands and the
/// sync engine consume this trait so tests can substitute a `FakeAdapter`
/// without spawning real backend CLIs.
pub const Adapter = struct {
    /// Implementation-owned pointer passed back to the vtable.
    context: *anyopaque,
    /// Adapter operations for the concrete implementation behind `context`.
    vtable: *const VTable,

    /// Adapter implementation hooks.
    pub const VTable = struct {
        pullBackendItems: *const fn (
            context: *anyopaque,
            gpa: Allocator,
            diag: ?*Diagnostic,
        ) PullError![]BackendItemSnapshot,
        applyMutation: *const fn (
            context: *anyopaque,
            gpa: Allocator,
            view: MutationView,
            now: []const u8,
        ) ApplyError!Outcome,
    };

    /// Fetch the current snapshot of backend-owned Items.
    ///
    /// On `PullError.PullFailed`, the optional `?*Diagnostic` carries CLI
    /// stderr captured by the adapter; the engine renders `diag.message()`
    /// on the next stderr line and stops the sync.
    pub fn pullBackendItems(
        self: Adapter,
        gpa: Allocator,
        diag: ?*Diagnostic,
    ) PullError![]BackendItemSnapshot {
        return self.vtable.pullBackendItems(self.context, gpa, diag);
    }

    /// Apply one pending Mutation Log entry to the backend.
    ///
    /// Returns `Outcome.success` (a `Receipt`) or `Outcome.failure` (a
    /// `Failure` whose `detail` is owned by `gpa`). Environment failures
    /// arrive as bare `ApplyError` tags via the error union.
    pub fn applyMutation(
        self: Adapter,
        gpa: Allocator,
        view: MutationView,
        now: []const u8,
    ) ApplyError!Outcome {
        return self.vtable.applyMutation(self.context, gpa, view, now);
    }
};
