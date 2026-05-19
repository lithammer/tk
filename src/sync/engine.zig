//! Sync engine orchestration.
//!
//! `runSync` is the single entry point for `tk sync` and `tk promote`. It
//! composes the type-erased Backend Adapter from `src/remote/` with the SQL
//! helpers in `src/store/sync.zig`:
//!
//! 1. Pull. `adapter.pullBackendItems` returns a snapshot slice (or
//!    `PullError.PullFailed` + Diagnostic) which the engine merges via
//!    `store_sync.mergeBackendSnapshots`. Pull is one transaction; on
//!    success the slice is freed regardless of merge outcome.
//! 2. Apply loop. `store_sync.loadApplicableMutations` returns
//!    pending+failed MutationViews in sequence order. The engine hands each
//!    one to `adapter.applyMutation`, then persists the outcome via
//!    `store_sync.applyMutationOutcome` in a per-mutation transaction.
//!    On the first `.failure` Outcome the loop stops.
//! 3. Optional pre-sync skip. `opts.skip_mutation_id` first transitions
//!    one failed Mutation to skipped via `store_sync.markMutationSkipped`
//!    in its own transaction. The skip commits even if Pull or Apply
//!    subsequently aborts.
//!
//! The engine is backend-blind: it never imports `src/remote/github.zig` or
//! peers. The Adapter trait is the only seam.

const std = @import("std");
const Allocator = std.mem.Allocator;
const zqlite = @import("zqlite");

const adapter_mod = @import("../remote/adapter.zig");
const Adapter = adapter_mod.Adapter;
const ApplyError = adapter_mod.ApplyError;
const PullError = adapter_mod.PullError;
const Diagnostic = @import("../domain/diagnostic.zig").Diagnostic;
const store_sync = @import("../store/sync.zig");
const BackendItemSnapshot = @import("../domain/backend_item_snapshot.zig").BackendItemSnapshot;
const MutationView = @import("../domain/mutation_view.zig").MutationView;
const outcome_mod = @import("../domain/outcome.zig");
const Outcome = outcome_mod.Outcome;

/// Caller-supplied options for `runSync`.
pub const RunSyncOptions = struct {
    /// When non-null, the engine first marks the named Mutation skipped
    /// (must currently be in `failed` state) before invoking Pull / Apply.
    /// The skip commits in its own transaction so it persists even if the
    /// sync run subsequently aborts.
    skip_mutation_id: ?i64 = null,
    /// Source of randomness for newly-discovered Backend items' internal
    /// `items.id` values. Production callers pass `std.crypto.random` (or
    /// a `std.Random.DefaultPrng.init(seed)` for tests).
    random: std.Random,
};

/// Summary of one sync run for the calling command to render.
pub const SyncReport = struct {
    /// Number of `BackendItemSnapshot`s the adapter returned from Pull. Zero
    /// when the adapter sent an empty list (a valid no-op).
    pulled_count: usize,
    /// Number of Mutations that transitioned to `applied` during this run.
    applied_count: usize,
    /// When non-null, the sync stopped because this Mutation's Apply
    /// returned `Outcome.failure { detail }`. The `mutations.failure_json`
    /// row now records the detail; the caller should render the sequence
    /// to the user.
    stopped_at_sequence: ?i64,
    /// When the pre-loop skip succeeded, the sequence that was transitioned.
    /// Non-null even if Pull or Apply subsequently aborted.
    skipped_sequence: ?i64,
};

/// Error set returned by `runSync`.
///
/// Covers the union of the Adapter trait's error sets, the SQL boundary's
/// errors, and the load-mutations decode errors. Catastrophic env failures
/// (`ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`) and Pull failures
/// (`PullFailed` carrying a Diagnostic) bubble out unchanged so the calling
/// command can dispatch on the tag for its stderr rendering.
pub const RunSyncError =
    PullError ||
    ApplyError ||
    store_sync.MergeError ||
    store_sync.LoadApplicableError ||
    store_sync.ApplyMutationOutcomeError ||
    store_sync.MarkSkippedError;

/// Run one sync against a configured Adapter.
///
/// `pull_diag` / `apply_diag` / `merge_diag` / `skip_diag` are optional
/// pointers the engine populates on the failure paths so the caller can
/// render the captured stderr or SQL errmsg on its own stderr.
pub fn runSync(
    conn: zqlite.Conn,
    gpa: Allocator,
    adapter: Adapter,
    now: []const u8,
    opts: RunSyncOptions,
    diag: ?*Diagnostic,
) RunSyncError!SyncReport {
    var report: SyncReport = .{
        .pulled_count = 0,
        .applied_count = 0,
        .stopped_at_sequence = null,
        .skipped_sequence = null,
    };

    // Step 1: pre-sync skip (own transaction).
    if (opts.skip_mutation_id) |seq| {
        try store_sync.markMutationSkipped(conn, seq, now, diag);
        report.skipped_sequence = seq;
    }

    // Step 2: Pull and merge.
    const snapshots = try adapter.pullBackendItems(gpa, diag);
    defer {
        for (snapshots) |snap| snap.deinit(gpa);
        gpa.free(snapshots);
    }
    report.pulled_count = snapshots.len;
    // Skip the merge transaction entirely when the pull was empty so an idle
    // sync (the common case) doesn't acquire a write lock for a no-op.
    if (snapshots.len > 0) {
        try store_sync.mergeBackendSnapshots(conn, gpa, opts.random, snapshots, now, diag);
    }

    // Step 3: Apply loop.
    const views = try store_sync.loadApplicableMutations(conn, gpa);
    defer {
        for (views) |v| store_sync.deinitMutationView(v, gpa);
        gpa.free(views);
    }

    for (views) |view| {
        const outcome = try adapter.applyMutation(gpa, view, now);
        // Free `Outcome.failure.detail` once we're done with the outcome —
        // applyMutationOutcome reads it but does not take ownership.
        defer switch (outcome) {
            .success => {},
            .failure => |f| f.deinit(gpa),
        };
        try store_sync.applyMutationOutcome(conn, gpa, view.sequence, outcome, now, diag);
        switch (outcome) {
            .success => report.applied_count += 1,
            .failure => {
                report.stopped_at_sequence = view.sequence;
                return report;
            },
        }
    }

    return report;
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const FakeAdapter = @import("../remote/fake.zig").FakeAdapter;
const PullResponse = @import("../remote/fake.zig").PullResponse;
const ApplyResponse = @import("../remote/fake.zig").ApplyResponse;
const ApplyCall = @import("../remote/fake.zig").ApplyCall;
const migrations = @import("../store/migrations.zig");
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

fn openMemDb() !zqlite.Conn {
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    errdefer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    return conn;
}

test "runSync: empty queue and empty Pull is a no-op" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
        .last_applied_sequence = 0,
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);

    try std.testing.expectEqual(@as(usize, 0), report.pulled_count);
    try std.testing.expectEqual(@as(usize, 0), report.applied_count);
    try std.testing.expectEqual(@as(?i64, null), report.stopped_at_sequence);
    try std.testing.expectEqual(@as(?i64, null), report.skipped_sequence);
}

test "runSync: Pull inserts a discovered Backend item" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });

    const scripted = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "42",
            .display_id = "gh-42",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Discovered",
            .body = "",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    const pull_script = [_]PullResponse{.{ .snapshots = &scripted }};
    const apply_script = [_]ApplyResponse{};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);
    try std.testing.expectEqual(@as(usize, 1), report.pulled_count);

    // Row landed.
    const row = (try conn.row(
        "select title from items where backend_kind = 'github' and backend_key = '42'",
        .{},
    )) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("Discovered", row.text(0));
}

test "runSync: Apply success transitions mutation and advances cursor" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Old",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 5,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"New\",\"body\":\"\"}",
        .state = "pending",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{.success};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);
    try std.testing.expectEqual(@as(usize, 1), report.applied_count);
    try std.testing.expectEqual(@as(?i64, null), report.stopped_at_sequence);

    const mut_row = (try conn.row("select state from mutations where sequence = 5", .{})) orelse return error.ExpectedRow;
    defer mut_row.deinit();
    try std.testing.expectEqualStrings("applied", mut_row.text(0));

    const cursor_row = (try conn.row(
        "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
        .{},
    )) orelse return error.ExpectedRow;
    defer cursor_row.deinit();
    try std.testing.expectEqual(@as(i64, 5), cursor_row.int(0));

    // FakeAdapter saw the call with the decoded payload.
    try std.testing.expectEqual(@as(usize, 1), fake.captured_applies.items.len);
    try std.testing.expectEqual(@as(i64, 5), fake.captured_applies.items[0].sequence);
    try std.testing.expect(std.mem.indexOf(u8, fake.captured_applies.items[0].payload_text, "\"title\":\"New\"") != null);
}

test "runSync: Apply recorded_failure transitions to failed and stops" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t2",
        .display = "gh-2",
        .title = "T2",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 2,
        .mutation_type = "update_ticket",
        .item_id = "t2",
        .payload_json = "{\"title\":\"B\",\"body\":\"\"}",
        .state = "pending",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{
        .{ .recorded_failure = "HTTP 422: title required" },
    };
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);
    try std.testing.expectEqual(@as(usize, 0), report.applied_count);
    try std.testing.expectEqual(@as(?i64, 1), report.stopped_at_sequence);

    // Sequence 1 failed, sequence 2 still pending.
    const r1 = (try conn.row("select state, failure_json from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("failed", r1.text(0));
    try std.testing.expect(std.mem.indexOf(u8, r1.text(1), "title required") != null);

    const r2 = (try conn.row("select state from mutations where sequence = 2", .{})) orelse return error.ExpectedRow;
    defer r2.deinit();
    try std.testing.expectEqualStrings("pending", r2.text(0));

    // Adapter consumed exactly one apply script entry.
    try std.testing.expectEqual(@as(usize, 1), fake.apply_index);
}

test "runSync: Apply env_failure propagates and leaves row pending" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{.{ .env_failure = error.ExecutableNotFound }};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    try std.testing.expectError(
        error.ExecutableNotFound,
        runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null),
    );

    // Row stays pending; engine wrote no outcome.
    const r1 = (try conn.row("select state from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("pending", r1.text(0));
}

test "runSync: Pull recorded_failure propagates with Diagnostic; Apply not invoked" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });

    const pull_script = [_]PullResponse{.{ .recorded_failure = "gh: HTTP 502" }};
    const apply_script = [_]ApplyResponse{};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    var diag: Diagnostic = .{};
    try std.testing.expectError(
        error.PullFailed,
        runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, &diag),
    );
    try std.testing.expect(std.mem.indexOf(u8, diag.message(), "HTTP 502") != null);

    // Apply was never invoked.
    try std.testing.expectEqual(@as(usize, 0), fake.captured_applies.items.len);

    // Row still pending.
    const r1 = (try conn.row("select state from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer r1.deinit();
    try std.testing.expectEqualStrings("pending", r1.text(0));
}

test "runSync: --skip transitions failed mutation before Pull" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 7,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"old\"}",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(
        conn,
        gpa,
        fake.adapter(),
        "2026-05-19T00:00:00Z",
        .{ .skip_mutation_id = 7, .random = prng.random() },
        null,
    );
    try std.testing.expectEqual(@as(?i64, 7), report.skipped_sequence);

    const r = (try conn.row("select state from mutations where sequence = 7", .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("skipped", r.text(0));
}

test "runSync: --skip on non-failed mutation returns MutationNotFailed and aborts" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 7,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "pending",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    try std.testing.expectError(
        error.MutationNotFailed,
        runSync(
            conn,
            gpa,
            fake.adapter(),
            "2026-05-19T00:00:00Z",
            .{ .skip_mutation_id = 7, .random = prng.random() },
            null,
        ),
    );

    // No Pull was invoked because skip aborted before reaching Pull.
    try std.testing.expectEqual(@as(usize, 0), fake.pull_index);

    // Row still pending.
    const r = (try conn.row("select state from mutations where sequence = 7", .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("pending", r.text(0));
}

test "runSync: failed mutation retried successfully transitions to applied" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "T1",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 3,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
        .state = "failed",
        .failure_json = "{\"detail\":\"prior\"}",
    });

    const empty: []const BackendItemSnapshot = &.{};
    const pull_script = [_]PullResponse{.{ .snapshots = empty }};
    const apply_script = [_]ApplyResponse{.success};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    const report = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);
    try std.testing.expectEqual(@as(usize, 1), report.applied_count);

    const r = (try conn.row("select state, failure_json from mutations where sequence = 3", .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("applied", r.text(0));
    try std.testing.expectEqual(@as(?[]const u8, null), r.nullableText(1));
}

test "runSync: Pull snapshot for item with pending mutation is skipped" {
    const gpa = std.testing.allocator;
    var prng = std.Random.DefaultPrng.init(0);

    const conn = try openMemDb();
    defer conn.close();

    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "gh-1",
        .title = "Local Edit",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureRemote(conn, .{
        .backend_kind = "github",
        .config_json = "{\"owner\":\"o\",\"repo\":\"r\"}",
    });
    try TmpStore.insertFixtureMutation(conn, .{
        .sequence = 1,
        .mutation_type = "update_ticket",
        .item_id = "t1",
        .payload_json = "{\"title\":\"Local Edit\",\"body\":\"\"}",
        .state = "pending",
    });

    const scripted = [_]BackendItemSnapshot{
        .{
            .backend_kind = "github",
            .backend_key = "1",
            .display_id = "gh-1",
            .item_class = .ticket,
            .ticket_kind = .task,
            .title = "Stale Backend View",
            .body = "",
            .status = .open,
            .backend_updated_at = "2026-05-19T00:00:00Z",
        },
    };
    const pull_script = [_]PullResponse{.{ .snapshots = &scripted }};
    const apply_script = [_]ApplyResponse{.success};
    var fake = FakeAdapter.init(gpa, &pull_script, &apply_script);
    defer fake.deinit();

    _ = try runSync(conn, gpa, fake.adapter(), "2026-05-19T00:00:00Z", .{ .random = prng.random() }, null);

    // Title was NOT overwritten by Pull (Scenario B skip).
    const r = (try conn.row("select title from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer r.deinit();
    try std.testing.expectEqualStrings("Local Edit", r.text(0));
}
