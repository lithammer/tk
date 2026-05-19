//! Snapshot of one backend-owned Item observed during a Backend Pull.
//!
//! Lives in `src/domain/` because it is pure data with no SQLite, filesystem,
//! Git, or subprocess dependencies. Consumed by `store.mergeBackendSnapshots`
//! and produced by `Adapter.pullBackendItems`.

const std = @import("std");
const Allocator = std.mem.Allocator;

const ItemClass = @import("item_class.zig").ItemClass;
const ItemStatus = @import("status.zig").ItemStatus;
const TicketKind = @import("ticket_kind.zig").TicketKind;

/// One backend-owned Item snapshot returned from a Backend Pull.
///
/// Owned slices are allocated with the per-call `gpa` argument passed to
/// `Adapter.pullBackendItems`. Callers free via `deinit(gpa)` once they have
/// copied the data they need into the Repository Store.
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
