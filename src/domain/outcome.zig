//! Per-Mutation Apply outcome — the typed result returned by
//! `Adapter.applyMutation` and consumed by the sync engine when it persists
//! `mutations.state` and `mutations.failure_json`.
//!
//! Lives in `src/domain/` because it is pure data with no SQLite, filesystem,
//! Git, or subprocess dependencies. `Receipt` is empty in ticket-17; the
//! Promote slice grows it with backend-assigned identifiers returned by a
//! successful `promote_*` Mutation.

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Distinguishes per-Mutation success from per-Mutation rejection.
///
/// Environment failures (`ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`)
/// are surfaced through the bare `ApplyError` union, not through this tagged
/// union — that split is documented in `docs/adr/0009-sync-failure-taxonomy.md`.
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

test "Outcome.failure.deinit frees detail" {
    const gpa = std.testing.allocator;
    const detail = try gpa.dupe(u8, "backend rejected mutation");
    const failure = Failure{ .detail = detail };
    failure.deinit(gpa);
}
