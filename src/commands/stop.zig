//! `tk stop` — symmetric lifecycle: move an active Ticket or Epic back to open.

const std = @import("std");
const Allocator = std.mem.Allocator;

const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const ItemStatus = @import("../domain/status.zig").ItemStatus;

/// Dispatcher metadata for `tk stop`.
pub const meta: cli.CommandMeta = .{
    .name = "stop",
    .description = "Move an active Ticket or Epic back to open",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\<str>
    \\
);

/// Parse `tk stop` args, resolve the Display ID or Alias, and move it to open.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &params, clap.parsers.default, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .{ .subcommand = meta.name },
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    const id = res.positionals[0] orelse {
        deps.stderr.writeAll(messages.stop_id_required ++ "\n") catch {};
        return 2;
    };

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    const resolved = (repository.resolveItemRef(store, deps.gpa, id) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.stop_id_not_found_prefix ++ "{s}" ++ messages.stop_id_not_found_suffix ++ "\n", .{id}) catch {};
        return 1;
    };
    defer resolved.deinit(deps.gpa);

    const outcome = repository.setItemStatus(store, deps.gpa, deps.clock, .{
        .id = resolved.id,
        .status = ItemStatus.open,
    }) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    switch (outcome) {
        .ok => |item| {
            defer item.deinit(deps.gpa);
            const prefix: []const u8 = switch (item.item_class) {
                .ticket => messages.stop_success_ticket_prefix,
                .epic => messages.stop_success_epic_prefix,
            };
            deps.stdout.print("{s}{s} - {s}\n", .{ prefix, item.display_id, item.title }) catch {};
        },
        // Race window: the resolved row may have been deleted between
        // `resolveItemRef` and the BEGIN IMMEDIATE inside `setItemStatus`.
        .not_found => {
            deps.stderr.print(messages.stop_id_not_found_prefix ++ "{s}" ++ messages.stop_id_not_found_suffix ++ "\n", .{id}) catch {};
            return 1;
        },
        // ADR 0006: `done` is terminal in v1. Surface the typed outcome as a
        // dedicated diagnostic and exit 1 without touching current state.
        .locked_done => |item_class| {
            const line: []const u8 = switch (item_class) {
                .ticket => messages.stop_locked_done_ticket,
                .epic => messages.stop_locked_done_epic,
            };
            deps.stderr.print("{s}\n", .{line}) catch {};
            return 1;
        },
    }

    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk stop - move an active Ticket or Epic back to open
        \\
        \\Usage:
        \\  tk stop <id> [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
    );
}

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.stop_store_busy_retry,
    .out_of_memory = messages.stop_out_of_memory,
    .fallback = messages.stop_write_failed,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "stop",
    .missing_store = messages.stop_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const zqlite = @import("zqlite");
const init_command = @import("init.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

const StoreFixture = struct {
    tmp_store: TmpStore,
    cwd: std.Io.Dir,
    rev_parse: []u8,

    fn init(gpa: Allocator) !StoreFixture {
        var tmp_store = try TmpStore.init(gpa, "project");
        errdefer tmp_store.deinit(gpa);
        var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
        errdefer cwd.close(std.testing.io);
        const rev_parse = try tmp_store.gitRevParseStdout(gpa);
        errdefer gpa.free(rev_parse);

        {
            var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
            defer h.deinit();
            try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
            try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
        }

        return .{ .tmp_store = tmp_store, .cwd = cwd, .rev_parse = rev_parse };
    }

    fn deinit(self: *StoreFixture, gpa: Allocator) void {
        gpa.free(self.rev_parse);
        self.cwd.close(std.testing.io);
        self.tmp_store.deinit(gpa);
    }
};

fn expectRevParse(h: *Harness, fixture: StoreFixture) !void {
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
}

test "stop: --help prints usage and exits 0" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"--help"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk stop") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk stop <id>") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "stop: requires a positional id" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.stop_id_required ++ "\n", h.stderr());
}

test "stop: reports missing store as exit 1" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.stop_missing_store ++ "\n", h.stderr());
}

test "stop: reports unknown id as exit 1" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.init(gpa, &.{"no-such-id"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.stop_id_not_found_prefix ++ "no-such-id" ++ messages.stop_id_not_found_suffix ++ "\n",
        h.stderr(),
    );
}

test "stop: resolves Alias and moves a local active Ticket back to open" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Local Ticket",
        .status = "active",
        .created_seq = 1,
    });
    try TmpStore.insertAlias(conn, "old-1", "t1");

    var h = Harness.init(gpa, &.{"old-1"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Marked Ticket open: project-1 - Local Ticket\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "stop: moves a local active Epic back to open" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Local Epic",
        .status = "active",
        .created_seq = 1,
    });

    var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Marked Epic open: project-1 - Local Epic\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
    const row = (try conn.row("select status from items where id = 'e1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
}

test "stop: Backend Ticket success emits set_item_status Mutation" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt1",
        .display = "GH#7",
        .title = "Backend Ticket",
        .status = "active",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "7",
        .created_seq = 1,
    });

    var h = Harness.init(gpa, &.{"GH#7"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Marked Ticket open: GH#7 - Backend Ticket\n", h.stdout());
    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));
    const row = (try conn.row("select mutation_type, payload_json from mutations where sequence = 1", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("set_item_status", row.text(0));
    try std.testing.expectEqualStrings("{\"status\":\"open\"}", row.text(1));
    const item_row = (try conn.row("select status from items where id = 'bt1'", .{})) orelse return error.ExpectedRow;
    defer item_row.deinit();
    try std.testing.expectEqualStrings("open", item_row.text(0));
}

test "stop: already-open target is a no-op" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt1",
        .display = "GH#7",
        .title = "Backend Ticket",
        .status = "open",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "7",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    var h = Harness.init(gpa, &.{"GH#7"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Marked Ticket open: GH#7 - Backend Ticket\n", h.stdout());
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const row = (try conn.row("select updated_at from items where id = 'bt1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(0));
}

test "stop: done Ticket cannot be stopped (locked_done)" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Local Ticket",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.stop_locked_done_ticket ++ "\n", h.stderr());

    const row = (try conn.row("select status, updated_at from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "stop: done Epic cannot be stopped (locked_done)" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Local Epic",
        .status = "done",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });

    var h = Harness.init(gpa, &.{"project-1"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.stop_locked_done_epic ++ "\n", h.stderr());

    const row = (try conn.row("select status, updated_at from items where id = 'e1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("done", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "stop: forced write failure reports diagnostic and rolls back" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt1",
        .display = "GH#7",
        .title = "Backend Ticket",
        .status = "active",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "7",
        .created_seq = 1,
        .updated_at = "2026-01-01T00:00:00.000Z",
    });
    try conn.execNoArgs(
        \\create trigger fail_set_item_status_mutation
        \\before insert on mutations
        \\when new.mutation_type = 'set_item_status'
        \\begin
        \\  select raise(abort, 'forced set_item_status failure');
        \\end
    );

    var h = Harness.init(gpa, &.{"GH#7"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try expectRevParse(&h, fixture);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.stop_write_failed) != null);

    const row = (try conn.row("select status, updated_at from items where id = 'bt1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("active", row.text(0));
    try std.testing.expectEqualStrings("2026-01-01T00:00:00.000Z", row.text(1));
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
    const seq = (try conn.row("select value from sequences where name = 'mutation_seq'", .{})) orelse return error.ExpectedRow;
    defer seq.deinit();
    try std.testing.expectEqual(@as(i64, 0), seq.int(0));
}
