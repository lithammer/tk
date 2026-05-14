//! `tk add` — create a local Ticket from a message file.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const message = @import("message.zig");
const repository = @import("../store/repository.zig");
const Priority = @import("../domain/priority.zig").Priority;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;

/// Dispatcher metadata for `tk add`.
pub const meta: cli.CommandMeta = .{
    .name = "add",
    .description = "Create a local task Ticket from a message file",
};

const params = clap.parseParamsComptime(
    \\-h, --help         Display this help and exit.
    \\-F, --file <str>...  Read the message from a file, or '-' for stdin.
    \\
);

/// Parse `tk add` flags and create a local task Ticket.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    var res = clap.parseEx(clap.Help, &params, clap.parsers.default, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
    }) catch |err| {
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    if (res.args.file.len != 1) {
        deps.stderr.writeAll("tk add: exactly one -F/--file is required\n") catch {};
        return 2;
    }
    const path = res.args.file[0];

    const raw = if (std.mem.eql(u8, path, "-"))
        deps.stdin.allocRemaining(deps.gpa, .unlimited) catch |err| {
            deps.stderr.print(messages.add_stdin_read_prefix ++ "{s}\n", .{@errorName(err)}) catch {};
            return 1;
        }
    else
        deps.cwd.readFileAlloc(deps.io, path, deps.gpa, .unlimited) catch |err| {
            deps.stderr.print(messages.add_file_read_prefix ++ "{s}: {s}\n", .{ path, @errorName(err) }) catch {};
            return 1;
        };
    defer deps.gpa.free(raw);

    const parsed = message.parse(deps.gpa, raw) catch |err| {
        switch (err) {
            error.EmptyMessage => deps.stderr.writeAll(messages.add_empty_message ++ "\n") catch {},
            error.NulByte => deps.stderr.writeAll(messages.add_nul_message ++ "\n") catch {},
            error.OutOfMemory => return error.OutOfMemory,
        }
        return 1;
    };
    defer parsed.deinit(deps.gpa);

    const open_outcome = repository.openExisting(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    const store = switch (open_outcome) {
        .ok => |store| store,
        else => {
            repository.renderOpenFailure(deps.stderr, deps.gpa, "add", messages.add_missing_store, open_outcome);
            return 1;
        },
    };
    defer store.close();

    const created = repository.createLocalTicket(store, deps.gpa, deps.clock, deps.random, .{
        .kind = TicketKind.default,
        .priority = Priority.default,
        .title = parsed.title,
        .body = parsed.body,
    }) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    defer created.deinit(deps.gpa);

    deps.stdout.print(messages.add_created_ticket_prefix ++ "{s} - {s}\n", .{ created.display_id, created.title }) catch {};
    deps.stdout.print(messages.add_priority_label ++ "{s}\n", .{created.priority.text()}) catch {};
    deps.stdout.print(messages.add_status_label ++ "{s}\n", .{created.status.text()}) catch {};
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk add - create a local task Ticket
        \\
        \\Creates a local task Ticket with Priority P2 from git-commit-style
        \\message input. The first paragraph becomes the title; later
        \\paragraphs become the body.
        \\
        \\Usage:
        \\  tk add -F <file | -> [options]
        \\  tk add --file <file | -> [options]
        \\
        \\Options:
        \\
    );
    try deps.stdout.writeAll(
        \\  -h, --help         Display this help and exit.
        \\  -F, --file <file | ->  Read the message from a file, or '-' for stdin.
        \\
    );
}

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    if (repository.isBusyError(err)) {
        deps.stderr.writeAll(messages.add_store_busy_retry ++ "\n") catch {};
        return;
    }
    if (err == error.OutOfMemory) {
        deps.stderr.writeAll(messages.add_out_of_memory ++ "\n") catch {};
        return;
    }
    deps.stderr.print(messages.add_create_failed ++ "\n{s}\n", .{@errorName(err)}) catch {};
}

const zqlite = @import("zqlite");
const init_command = @import("init.zig");
const migrations = @import("../store/migrations.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

test "add: creates current local Ticket state without a Mutation" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    try cwd.writeFile(std.testing.io, .{
        .sub_path = "followup.md",
        .data =
        \\Investigate flaky login retry
        \\
        \\Repro is intermittent on the staging cluster.
        \\
        ,
    });

    {
        var h = Harness.initWith(gpa, &.{ "-F", "followup.md" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-1 - Investigate flaky login retry
            \\Priority: P2
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    if (try conn.row(
        \\select display_value, item_class, ticket_kind, priority, title, body,
        \\       origin, backend_kind, backend_key, status, created_seq,
        \\       created_at, updated_at
        \\from items
    , .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("project-1", r.text(0));
        try std.testing.expectEqualStrings("ticket", r.text(1));
        try std.testing.expectEqualStrings("task", r.text(2));
        try std.testing.expectEqualStrings("P2", r.text(3));
        try std.testing.expectEqualStrings("Investigate flaky login retry", r.text(4));
        try std.testing.expectEqualStrings("Repro is intermittent on the staging cluster.", r.text(5));
        try std.testing.expectEqualStrings("local", r.text(6));
        try std.testing.expectEqual(null, r.nullableText(7));
        try std.testing.expectEqual(null, r.nullableText(8));
        try std.testing.expectEqualStrings("open", r.text(9));
        try std.testing.expectEqual(@as(i64, 1), r.int(10));
        try std.testing.expectEqualStrings(r.text(11), r.text(12));
    } else return error.ExpectedRow;

    if (try conn.row("select source from item_ids where value = 'project-1'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("display", r.text(0));
    } else return error.ExpectedRow;

    try std.testing.expectEqual(
        @as(?i64, 0),
        try migrations.queryOptionalInt(conn, "select count(*) from item_ids where source = 'alias'"),
    );
    try std.testing.expectEqual(
        @as(?i64, 0),
        try migrations.queryOptionalInt(conn, "select count(*) from mutations"),
    );
    try std.testing.expectEqual(
        @as(?i64, 0),
        try migrations.queryOptionalInt(conn, "select value from sequences where name = 'mutation_seq'"),
    );
}

test "add: reads message from stdin with --file -" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{ "--file", "-" }, .{
            .cwd = cwd,
            .stdin =
            \\Write stdin parity test
            \\
            \\Agents often stream command messages over stdin.
            \\
            ,
        });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-1 - Write stdin parity test
            \\Priority: P2
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();

    if (try conn.row("select body from items where display_value = 'project-1'", .{})) |r| {
        defer r.deinit();
        try std.testing.expectEqualStrings("Agents often stream command messages over stdin.", r.text(0));
    } else return error.ExpectedRow;
}

test "add: prefixes git rejection diagnostics" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    try cwd.writeFile(std.testing.io, .{
        .sub_path = "followup.md",
        .data = "Valid title\n",
    });

    var h = Harness.initWith(gpa, &.{ "-F", "followup.md" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{
        .exit_code = 128,
        .stderr = "fatal: not a git repository (or any of the parent directories): .git\n",
    });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk add: fatal: not a git repository (or any of the parent directories): .git\n",
        h.stderr(),
    );
}

test "add: accepts --file=<file> and reports a missing Repository Store" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    try cwd.writeFile(std.testing.io, .{
        .sub_path = "followup.md",
        .data = "Valid title\n",
    });

    var h = Harness.initWith(gpa, &.{"--file=followup.md"}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.add_missing_store ++ "\n", h.stderr());
}

test "add: validates message input before git discovery" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    try cwd.writeFile(std.testing.io, .{
        .sub_path = "empty.md",
        .data = " \n\t\n",
    });

    var h = Harness.initWith(gpa, &.{ "-F", "empty.md" }, .{ .cwd = cwd });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.add_empty_message ++ "\n", h.stderr());
}

test "add: maps busy, locked, and OOM storage errors to dedicated diagnostics" {
    var busy = Harness.init(std.testing.allocator, &.{});
    defer busy.deinit();
    renderStorageError(busy.deps(), error.Busy);
    try std.testing.expectEqualStrings(messages.add_store_busy_retry ++ "\n", busy.stderr());

    var locked = Harness.init(std.testing.allocator, &.{});
    defer locked.deinit();
    renderStorageError(locked.deps(), error.LockedSharedCache);
    try std.testing.expectEqualStrings(messages.add_store_busy_retry ++ "\n", locked.stderr());

    var oom = Harness.init(std.testing.allocator, &.{});
    defer oom.deinit();
    renderStorageError(oom.deps(), error.OutOfMemory);
    try std.testing.expectEqualStrings(messages.add_out_of_memory ++ "\n", oom.stderr());
}

test "add: rejects repeated file flags as a usage error" {
    var h = Harness.init(std.testing.allocator, &.{ "-F", "one.md", "--file", "two.md" });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expect(h.stderr().len > 0);
}

test "add: help describes only the implemented slice" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk add -F <file | ->") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Priority P2") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "<str>...") == null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--priority") == null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--parent") == null);
    try std.testing.expectEqualStrings("", h.stderr());
}
