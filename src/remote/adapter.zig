//! Backend Adapter trait and contract types shared by adapters and the sync
//! engine.
//!
//! Types only — no logic. The type-erased `Adapter` struct mirrors the shape
//! of `proc.Runner` (see `src/proc/runner.zig`): a `*anyopaque` context and a
//! `VTable` of function pointers, with thin `pub` methods that thunk through
//! the vtable. Concrete adapters (`github`, `jira`, and the test `FakeAdapter`)
//! plug into this trait via `adapter()` helpers exactly like `RealRunner` /
//! `FakeRunner` do for `Runner`.
//!
//! Import direction: `remote` imports from `store` and `domain`, never the
//! other way around. The sync engine (slice 9) is the only module that
//! composes `Adapter` with the Mutation Log.

const std = @import("std");
const Allocator = std.mem.Allocator;

const Diagnostic = @import("../store/diagnostic.zig").Diagnostic;
const MutationPayload = @import("../store/mutations.zig").MutationPayload;
const MutationType = @import("../domain/mutation_type.zig").MutationType;
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;
const ItemStatus = @import("../domain/status.zig").ItemStatus;
const RunnerError = @import("../proc/runner.zig").Error;

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

/// Snapshot of one backend-owned Item observed during a pull.
///
/// Owned slices are allocated with the per-call `gpa` argument passed to
/// `pullBackendItems`. Callers free via `deinit(gpa)` once they have copied
/// the data they need into the Repository Store.
pub const BackendItemSnapshot = struct {
    /// Backend kind discriminator — `"github"` or `"jira"`.
    backend_kind: []const u8,
    /// Backend-native identifier (e.g. GitHub issue number or Jira issue key).
    backend_key: []const u8,
    /// Display ID assigned by the adapter.
    ///
    /// MUST be in a namespace that cannot collide with the local store prefix.
    /// The github adapter uses `gh-<issue-number>`; the jira adapter uses the
    /// natural Jira key (e.g. `PROJ-123`). After ticket-22 lands, the
    /// prefix-change command must revalidate against the configured adapter;
    /// until then this is enforced by `validateRemoteAgainstLocalPrefix` at
    /// `tk remote set` time.
    display_id: []const u8,
    /// Item Class (`.ticket` or `.epic`).
    item_class: ItemClass,
    /// Ticket Kind for tickets; `null` for epics.
    ticket_kind: ?TicketKind,
    /// Title rendered by the backend.
    title: []const u8,
    /// Body rendered by the backend (may be empty).
    body: []const u8,
    /// Item Status mapped from the backend's lifecycle state.
    status: ItemStatus,
    /// Reserved field — `backend_updated_at` (ISO-8601 string) is collected
    /// by adapters but ignored by the engine in v1. Kept on the snapshot so
    /// future change-detection slices can fill in without a contract churn.
    backend_updated_at: []const u8,

    /// Free each owned slice with the allocator passed to `pullBackendItems`.
    pub fn deinit(self: BackendItemSnapshot, gpa: Allocator) void {
        gpa.free(self.backend_kind);
        gpa.free(self.backend_key);
        gpa.free(self.display_id);
        gpa.free(self.title);
        gpa.free(self.body);
        gpa.free(self.backend_updated_at);
    }
};

/// Engine-built view of one pending Mutation Log row handed to an adapter.
///
/// The engine reads a `mutations` row plus the joined `items` snapshot and
/// projects it into this view before invoking `Adapter.applyMutation`. Only
/// the fields adapters need in v1 are included; richer columns can be added
/// without churning the row schema.
pub const MutationView = struct {
    /// `mutations.sequence` — monotonically increasing within a Repository
    /// Store, identifies this Mutation Log entry.
    sequence: i64,
    /// Typed Mutation kind from `MutationType`.
    mutation_type: MutationType,
    /// Internal stable `items.id` (NOT the Display ID — promote-safe).
    item_id: []const u8,
    /// Item Class of the target Item.
    item_class: ItemClass,
    /// Typed payload variant. Tag determined by `mutation_type`.
    payload: MutationPayload,
    /// Backend kind of the target Item, when known. `null` for items that
    /// have never reached the backend (local-origin pre-Promotion).
    backend_kind: ?[]const u8,
    /// Backend-native identifier of the target Item, when known. `null` for
    /// items that have never reached the backend (local-origin pre-Promotion).
    backend_key: ?[]const u8,
};

/// Result of `Adapter.applyMutation`.
///
/// Distinguishes per-Mutation success from per-Mutation rejection. Environment
/// failures (`ApplyError.ExecutableNotFound`, `.SpawnFailed`, `.OutOfMemory`)
/// are surfaced through the bare error union, not through this tagged union.
pub const Outcome = union(enum) {
    /// Mutation accepted by the backend.
    success: Receipt,
    /// Mutation rejected by the backend (non-zero exit, validation refusal).
    /// See `Failure.detail` for the ownership contract on the captured slice.
    failure: Failure,
};

/// Adapter-supplied evidence that a Mutation succeeded.
///
/// Intentionally empty for ticket-17 — the Promote slice grows it with the
/// backend-assigned identifiers (e.g. issue number, Jira key) returned by a
/// successful `promote_*` Mutation.
pub const Receipt = struct {};

/// Adapter-supplied evidence that a Mutation was rejected.
pub const Failure = struct {
    /// Human-readable failure detail captured from the adapter (typically the
    /// backend CLI's stderr).
    ///
    /// Ownership contract: the adapter MUST allocate `detail` via the per-call
    /// `gpa` argument passed to `applyMutation`. If copying from a runner-
    /// owned slice such as `proc.Result.stderr`, the adapter must
    /// `gpa.dupe` before returning — the runner's `Result.deinit(gpa)` frees
    /// its own stderr buffer before the engine writes `detail` to
    /// `mutations.failure_json`. The engine takes ownership on return and
    /// frees via a `defer` block.
    detail: []const u8,

    /// Free `detail` with the allocator passed to `applyMutation`.
    pub fn deinit(self: Failure, gpa: Allocator) void {
        gpa.free(self.detail);
    }
};

/// Type-erased Backend Adapter.
///
/// Modelled on `proc.Runner` (see `src/proc/runner.zig`): commands and the
/// sync engine consume this trait so tests can substitute a `FakeAdapter`
/// without spawning real backend CLIs.
///
/// Import direction: `remote → store → domain`, never reversed.
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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

test "Outcome.failure.deinit frees detail" {
    const gpa = std.testing.allocator;
    const detail = try gpa.dupe(u8, "backend rejected mutation");
    const failure = Failure{ .detail = detail };
    failure.deinit(gpa);
}

test "BackendItemSnapshot.deinit frees each owned slice" {
    const gpa = std.testing.allocator;
    const snapshot = BackendItemSnapshot{
        .backend_kind = try gpa.dupe(u8, "github"),
        .backend_key = try gpa.dupe(u8, "42"),
        .display_id = try gpa.dupe(u8, "gh-42"),
        .item_class = .ticket,
        .ticket_kind = .task,
        .title = try gpa.dupe(u8, "Title"),
        .body = try gpa.dupe(u8, "Body"),
        .status = .open,
        .backend_updated_at = try gpa.dupe(u8, "2026-05-19T00:00:00Z"),
    };
    snapshot.deinit(gpa);
}
