//! `tk next` — select the next ready Ticket.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const init_command = @import("init.zig");
const add_command = @import("add.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

/// Dispatcher metadata for `tk next`.
pub const meta: cli.CommandMeta = .{
    .name = "next",
    .description = "Select the next ready Ticket",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

/// Parse `tk next` flags, read the Repository Store, and print one ready Ticket.
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

    const open_outcome = repository.openExisting(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    const store = switch (open_outcome) {
        .ok => |store| store,
        else => {
            repository.renderOpenFailure(deps.stderr, deps.gpa, "next", messages.next_missing_store, open_outcome);
            return 1;
        },
    };
    defer store.close();

    // Workspace Scope discovery has not landed yet, so the command currently
    // performs repository-wide selection. Keep the scoped diagnostic seam local
    // to this renderer for the future worktree slice.
    const applied_scope = false;
    const outcome = repository.nextReadyTicket(store, deps.gpa, .{}) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    switch (outcome) {
        .ticket => |ticket| {
            defer ticket.deinit(deps.gpa);
            try deps.stdout.print("{s}\n", .{ticket.display_id});
            return 0;
        },
        .no_ready_ticket => {
            const message = if (applied_scope) messages.next_no_ready_ticket_in_scope else messages.next_no_ready_ticket;
            deps.stderr.writeAll(message) catch {};
            deps.stderr.writeAll("\n") catch {};
            return 1;
        },
        .scope_not_found => {
            deps.stderr.writeAll(messages.next_scope_not_found ++ "\n") catch {};
            return 1;
        },
    }
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk next - select the next ready Ticket
        \\
        \\Usage:
        \\  tk next [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
    );
}

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, .{
        .busy_retry = messages.next_store_busy_retry,
        .out_of_memory = messages.next_out_of_memory,
        .fallback = messages.next_read_failed,
    });
}

test "next: prints the first ready Ticket from the Repository Store" {
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
        .sub_path = "item.md",
        .data = "Write next command\n",
    });

    {
        var h = Harness.initWith(gpa, &.{ "-F", "item.md" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try add_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-1\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "next: returns exit 1 when no ready Ticket exists" {
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

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_no_ready_ticket ++ "\n", h.stderr());
}

test "next: rejects explicit scope arguments" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"tk-1"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("Invalid argument 'tk-1'\n", h.stderr());
}

test "next: reports missing store after successful Git discovery" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_missing_store ++ "\n", h.stderr());
}
