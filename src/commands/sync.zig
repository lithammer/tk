//! `tk sync` and `tk sync log` — Mutation Log replay and inspection.
//!
//! `tk sync` invokes the engine in src/sync/engine.zig with the Adapter
//! returned by src/remote/factory.zig. In tk-17 the only real adapter
//! kinds (github, jira) return error.NotImplemented at factory time; the
//! engine itself is exercised through the FakeAdapter inside engine.zig's
//! test block. Once real adapters land in their own slices this command
//! becomes the production entry point unchanged.
//!
//! `tk sync log` calls the read helpers in src/store/sync.zig
//! (listMutationLog / showMutationLog), keeping all SQL inside src/store/.

const std = @import("std");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const Diagnostic = @import("../domain/diagnostic.zig").Diagnostic;
const factory = @import("../remote/factory.zig");
const store_sync = @import("../store/sync.zig");
const sync_engine = @import("../sync/engine.zig");

/// Dispatcher metadata for `tk sync`.
pub const meta: cli.CommandMeta = .{
    .name = "sync",
    .description = "Apply pending Mutations through the configured Remote",
};

pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    if (args_iter.next()) |s| {
        if (std.mem.eql(u8, s, "-h") or std.mem.eql(u8, s, "--help")) {
            writeHelp(deps) catch {};
            return 0;
        }
        if (std.mem.eql(u8, s, "log")) {
            return try runLog(deps, args_iter);
        }
        return try runSync(deps, peekableArgs(s, args_iter));
    }
    return try runSync(deps, peekableArgs(null, args_iter));
}

/// Build a single-pass iterator that yields `pre` first (when non-null) and
/// then delegates to the underlying argv iterator. Lets the dispatcher peek
/// the first arg without forcing the subcommand handler to thread a
/// `?[]const u8` "already consumed" parameter alongside the iterator.
fn peekableArgs(pre: ?[]const u8, rest: anytype) PeekableArgs(@TypeOf(rest)) {
    return .{ .pre = pre, .rest = rest };
}

fn PeekableArgs(comptime Rest: type) type {
    return struct {
        pre: ?[]const u8,
        rest: Rest,

        pub fn next(self: *@This()) ?[]const u8 {
            if (self.pre) |p| {
                self.pre = null;
                return p;
            }
            return self.rest.next();
        }
    };
}

/// Dispatch one RunSyncError tag to a category-specific stderr message. The
/// Diagnostic carries the bare value (display ID for DisplayIdCollision,
/// captured CLI stderr for PullFailed, SQLite errmsg for storage errors) —
/// this helper wraps it in the right prose so a future reader looking at
/// `tk sync` output gets a sentence rather than a bare identifier.
fn renderSyncError(stderr: *std.Io.Writer, err: anyerror, diag: *const Diagnostic, skip_id: ?i64) void {
    switch (err) {
        error.DisplayIdCollision => {
            stderr.print(
                "{s}{s}{s}\n",
                .{ messages.sync_display_id_collision_prefix, diag.message(), messages.sync_display_id_collision_suffix },
            ) catch {};
        },
        error.MutationNotFailed => {
            const seq = skip_id orelse return;
            stderr.print(
                "{s}{d}{s}\n",
                .{ messages.sync_skip_not_failed_prefix, seq, messages.sync_skip_not_failed_suffix },
            ) catch {};
        },
        error.MutationNotFound => {
            const seq = skip_id orelse return;
            stderr.print(
                "{s}{d}{s}\n",
                .{ messages.sync_skip_not_found_prefix, seq, messages.sync_skip_not_found_suffix },
            ) catch {};
        },
        error.MutationTypeUnknown, error.MutationPayloadVariantMissing => {
            stderr.print("{s}\n", .{messages.sync_schema_drift}) catch {};
        },
        error.PullFailed => {
            stderr.print("{s}", .{messages.sync_failure_prefix}) catch {};
            if (diag.message().len > 0) {
                stderr.print("{s}\n", .{diag.message()}) catch {};
            } else {
                stderr.print("Pull failed\n", .{}) catch {};
            }
        },
        else => {
            // Env failures (ExecutableNotFound, SpawnFailed, OutOfMemory) and
            // generic SQLite errors fall through here. The Diagnostic captured
            // the SQLite errmsg before any rollback; render it when present
            // so the user sees the underlying cause beside the error tag.
            stderr.print("{s}{s}", .{ messages.sync_failure_prefix, @errorName(err) }) catch {};
            if (diag.message().len > 0) {
                stderr.print(" — {s}\n", .{diag.message()}) catch {};
            } else {
                stderr.writeAll("\n") catch {};
            }
        },
    }
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk sync - apply pending Mutations through the configured Remote
        \\
        \\Usage:
        \\  tk sync [--skip <mutation-id>]                          Run sync.
        \\  tk sync log [--pending | --failed | --skipped] [id]     Inspect log.
        \\
        \\Options for `sync`:
        \\  --skip <id>   Mark one failed Mutation skipped before running sync.
        \\
        \\Options for `sync log`:
        \\  --pending     Only pending Mutations.
        \\  --failed      Only failed Mutations.
        \\  --skipped     Only skipped Mutations.
        \\  [id]          Show one Mutation in detail (Mutation Sequence).
        \\
        \\Options:
        \\  -h, --help    Display this help and exit.
        \\
    );
}

const sync_storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.sync_store_busy_retry,
    .out_of_memory = messages.sync_out_of_memory,
    .fallback = messages.sync_storage_failed,
};

const sync_open_msgs: repository.OpenMessages = .{
    .command_name = "sync",
    .missing_store = messages.sync_missing_store,
    .storage = sync_storage_msgs,
};

const log_storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.sync_log_store_busy_retry,
    .out_of_memory = messages.sync_log_out_of_memory,
    .fallback = messages.sync_log_storage_failed,
};

const log_open_msgs: repository.OpenMessages = .{
    .command_name = "sync log",
    .missing_store = messages.sync_log_missing_store,
    .storage = log_storage_msgs,
};

fn runSync(deps: cli.Deps, args_iter: anytype) !u8 {
    var iter = args_iter;
    var skip_id: ?i64 = null;
    while (iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--skip")) {
            const v = iter.next() orelse {
                deps.stderr.print("{s}\n", .{messages.sync_skip_requires_arg}) catch {};
                return 2;
            };
            skip_id = std.fmt.parseInt(i64, v, 10) catch {
                deps.stderr.print("{s}\n", .{messages.sync_skip_not_integer}) catch {};
                return 2;
            };
        } else {
            deps.stderr.print(
                "{s}{s}{s}\n",
                .{ messages.sync_unknown_arg_prefix, arg, messages.sync_unknown_arg_suffix },
            ) catch {};
            return 2;
        }
    }

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, sync_open_msgs) orelse return 1;
    defer store.close();

    var now_buf: [24]u8 = undefined;
    const now = deps.clock.nowIso(&now_buf);

    // Commit the skip BEFORE opening the adapter so a broken Remote (e.g.
    // factory returning NotImplemented in this slice) cannot block the
    // operator from abandoning a failed Mutation. tk-17's resolved
    // design calls this out: markMutationSkipped commits independently of
    // any surrounding sync run.
    if (skip_id) |seq| {
        var skip_diag: Diagnostic = .{};
        store_sync.markMutationSkipped(store.conn, seq, now, &skip_diag) catch |err| {
            renderSyncError(deps.stderr, err, &skip_diag, skip_id);
            return 1;
        };
    }

    const adapter_opt = factory.openConfigured(store.conn, deps.gpa) catch |err| switch (err) {
        error.NotImplemented => {
            deps.stderr.print("{s}\n", .{messages.sync_adapter_not_implemented}) catch {};
            return 1;
        },
        else => {
            repository.renderStorageError(deps.stderr, err, sync_storage_msgs);
            return 1;
        },
    };
    const adapter = adapter_opt orelse {
        deps.stderr.print("{s}\n", .{messages.sync_no_remote}) catch {};
        return 1;
    };

    var diag: Diagnostic = .{};
    const report = sync_engine.runSync(
        store.conn,
        deps.gpa,
        adapter,
        now,
        .{ .random = deps.random },
        &diag,
    ) catch |err| {
        renderSyncError(deps.stderr, err, &diag, skip_id);
        return 1;
    };

    try deps.stdout.print(
        "{s}{d} pulled, {d} applied",
        .{ messages.sync_summary_prefix, report.pulled_count, report.applied_count },
    );
    if (skip_id) |seq| try deps.stdout.print(", skipped {d}", .{seq});
    if (report.stopped_at_sequence) |seq| try deps.stdout.print(", stopped at {d}", .{seq});
    try deps.stdout.writeAll(".\n");
    return 0;
}

fn runLog(deps: cli.Deps, args_iter: anytype) !u8 {
    var filter: store_sync.LogListFilter = .default;
    var id_arg: ?[]const u8 = null;
    while (args_iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--pending")) {
            filter = .pending;
        } else if (std.mem.eql(u8, arg, "--failed")) {
            filter = .failed;
        } else if (std.mem.eql(u8, arg, "--skipped")) {
            filter = .skipped;
        } else if (id_arg == null) {
            id_arg = arg;
        }
    }

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, log_open_msgs) orelse return 1;
    defer store.close();

    if (id_arg) |id_str| {
        const seq = std.fmt.parseInt(i64, id_str, 10) catch {
            deps.stderr.print("{s}\n", .{messages.sync_log_id_not_numeric}) catch {};
            return 2;
        };
        const detail = store_sync.showMutationLog(store.conn, deps.gpa, seq) catch |err| switch (err) {
            error.MutationNotFound => {
                deps.stderr.print(
                    "{s}{d}{s}\n",
                    .{ messages.sync_log_not_found_prefix, seq, messages.sync_log_not_found_suffix },
                ) catch {};
                return 1;
            },
            else => {
                repository.renderStorageError(deps.stderr, err, log_storage_msgs);
                return 1;
            },
        };
        defer detail.deinit(deps.gpa);

        try deps.stdout.print("Mutation {d}  [{s}]\n", .{ detail.sequence, detail.state });
        try deps.stdout.print("Type:       {s}\n", .{detail.mutation_type});
        try deps.stdout.print("Target:     {s} ({s})\n", .{ detail.target_display_id, detail.item_class });
        try deps.stdout.print("Created:    {s}\n", .{detail.created_at});
        try deps.stdout.print("Updated:    {s}\n", .{detail.state_changed_at});
        try deps.stdout.print("Payload:    {s}\n", .{detail.payload_json});
        if (detail.failure_detail) |d| {
            try deps.stdout.writeAll("Failure:\n  ");
            try deps.stdout.print("{s}\n", .{d});
        }
        return 0;
    }

    const rows = store_sync.listMutationLog(store.conn, deps.gpa, filter) catch |err| {
        repository.renderStorageError(deps.stderr, err, log_storage_msgs);
        return 1;
    };
    defer {
        for (rows) |r| r.deinit(deps.gpa);
        deps.gpa.free(rows);
    }

    if (rows.len == 0) {
        const empty_msg = switch (filter) {
            .default => messages.sync_log_empty_default,
            .pending => messages.sync_log_empty_pending,
            .failed => messages.sync_log_empty_failed,
            .skipped => messages.sync_log_empty_skipped,
        };
        try deps.stdout.print("{s}\n", .{empty_msg});
        return 0;
    }

    for (rows) |row| {
        try deps.stdout.print(
            "{d} {s} {s} {s} {s}\n",
            .{ row.sequence, row.state, row.mutation_type, row.target_display_id, row.created_at },
        );
        if (row.failure_detail) |d| {
            try deps.stdout.print("  └─ {s}\n", .{d});
        }
    }
    return 0;
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const init_command = @import("init.zig");
const zqlite = @import("zqlite");

const StoreFixture = struct {
    tmp_store: TmpStore,
    cwd: std.Io.Dir,
    rev_parse: []u8,

    fn init(gpa: std.mem.Allocator) !StoreFixture {
        var tmp_store = try TmpStore.init(gpa, "project");
        errdefer tmp_store.deinit(gpa);
        var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
        errdefer cwd.close(std.testing.io);
        const rev_parse = try tmp_store.gitRevParseStdout(gpa);
        errdefer gpa.free(rev_parse);

        {
            var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
            defer h.deinit();
            try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
            try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
        }

        return .{ .tmp_store = tmp_store, .cwd = cwd, .rev_parse = rev_parse };
    }

    fn deinit(self: *StoreFixture, gpa: std.mem.Allocator) void {
        gpa.free(self.rev_parse);
        self.cwd.close(std.testing.io);
        self.tmp_store.deinit(gpa);
    }
};

test "tk sync: no Remote configured returns 1 with diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.sync_no_remote) != null);
}

test "tk sync: github Remote returns adapter NotImplemented in tk-17" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try TmpStore.insertFixtureRemote(conn, .{
            .backend_kind = "github",
            .config_json = "{\"repo\":\"o/r\"}",
        });
    }

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.sync_adapter_not_implemented) != null);
}

test "tk sync log: empty store prints default empty message" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{"log"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.sync_log_empty_default) != null);
}

test "tk sync log: lists default-view rows with failed continuation line" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
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
            .sequence = 1,
            .mutation_type = "update_ticket",
            .item_id = "t1",
            .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
            .state = "pending",
        });
        try TmpStore.insertFixtureMutation(conn, .{
            .sequence = 2,
            .mutation_type = "set_item_status",
            .item_id = "t1",
            .payload_json = "{\"status\":\"done\"}",
            .state = "failed",
            .failure_json = "{\"detail\":\"HTTP 422\"}",
        });
    }

    var h = Harness.initWith(gpa, &.{"log"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));

    const out = h.stdout();
    try std.testing.expect(std.mem.indexOf(u8, out, "1 pending update_ticket gh-1") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "2 failed set_item_status gh-1") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "  └─ HTTP 422") != null);
}

test "tk sync log <id>: shows detail for the matching sequence" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
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
            .sequence = 5,
            .mutation_type = "set_item_status",
            .item_id = "t1",
            .payload_json = "{\"status\":\"done\"}",
            .state = "failed",
            .failure_json = "{\"detail\":\"HTTP 422\"}",
        });
    }

    var h = Harness.initWith(gpa, &.{ "log", "5" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));

    const out = h.stdout();
    try std.testing.expect(std.mem.indexOf(u8, out, "Mutation 5  [failed]") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Type:       set_item_status") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Target:     gh-1 (ticket)") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "HTTP 422") != null);
}

test "tk sync log <id>: missing sequence returns 1 with diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "log", "999" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "Mutation 999 not found") != null);
}

test "tk sync --skip <id>: invalid value returns 2 with diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "--skip", "abc" }, .{ .cwd = fixture.cwd });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.sync_skip_not_integer) != null);
}

test "tk sync --skip <id>: non-failed mutation renders MutationNotFailed prose" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    // Configure a Remote (otherwise sync exits before reaching the engine).
    {
        var h_setup = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
        defer h_setup.deinit();
    }
    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try TmpStore.insertFixtureItem(conn, .{
            .id = "t1",
            .display = "gh-1",
            .title = "T",
            .origin = "backend",
            .backend_kind = "github",
            .backend_key = "1",
            .created_seq = 1,
        });
        try TmpStore.insertFixtureRemote(conn, .{
            .backend_kind = "github",
            .config_json = "{\"repo\":\"o/r\"}",
        });
        // Seed a PENDING (not failed) mutation; --skip should refuse.
        try TmpStore.insertFixtureMutation(conn, .{
            .sequence = 5,
            .mutation_type = "update_ticket",
            .item_id = "t1",
            .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
            .state = "pending",
        });
    }

    var h = Harness.initWith(gpa, &.{ "--skip", "5" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    // The renderSyncError MutationNotFailed branch produces a human sentence
    // including the target sequence, not the bare error tag.
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "Mutation 5") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "not in the failed state") != null);
}

test "tk sync --skip <id>: failed mutation is transitioned to skipped" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try TmpStore.insertFixtureItem(conn, .{
            .id = "t1",
            .display = "gh-1",
            .title = "T",
            .origin = "backend",
            .backend_kind = "github",
            .backend_key = "1",
            .created_seq = 1,
        });
        try TmpStore.insertFixtureRemote(conn, .{
            .backend_kind = "github",
            .config_json = "{\"repo\":\"o/r\"}",
        });
        try TmpStore.insertFixtureMutation(conn, .{
            .sequence = 5,
            .mutation_type = "update_ticket",
            .item_id = "t1",
            .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
            .state = "failed",
            .failure_json = "{\"detail\":\"prior\"}",
        });
    }

    var h = Harness.initWith(gpa, &.{ "--skip", "5" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    // The factory returns error.NotImplemented for the github kind in this
    // slice, so the run returns 1 — but the --skip step commits before Pull
    // is attempted, so the row should still transition.
    _ = try run(h.deps(), &h.iter);

    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadOnly | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    const row = (try conn.row("select state from mutations where sequence = 5", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("skipped", row.text(0));
}
