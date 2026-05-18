//! `tk next` — select the next ready Ticket.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const worktree_scope = @import("../worktree/scope.zig");
const init_command = @import("init.zig");
const add_command = @import("add.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const zqlite = @import("zqlite");

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

    const raw = worktree_scope.readGitSide(deps.gpa, deps.runner, deps.cwd) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    defer worktree_scope.freeRaw(deps.gpa, raw);
    const scope_outcome = worktree_scope.resolveAgainstStore(store, deps.gpa, raw) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    var scope_payload: ?worktree_scope.Scope = null;
    defer if (scope_payload) |s| s.deinit(deps.gpa);
    const next_scope: repository.NextScope = switch (scope_outcome) {
        .none => .none,
        .scope => |s| blk: {
            scope_payload = s;
            break :blk .{ .display_arg = s.display_id };
        },
        .configured_unresolved => |stored| {
            defer deps.gpa.free(stored);
            deps.stderr.print(
                "{s}{s}{s}\n",
                .{ messages.next_scope_unresolved_prefix, stored, messages.next_scope_unresolved_suffix },
            ) catch {};
            return 1;
        },
    };

    const outcome = repository.nextReadyTicket(store, deps.gpa, .{ .scope = next_scope }) catch |err| {
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
            const message = if (scope_payload != null) messages.next_no_ready_ticket_in_scope else messages.next_no_ready_ticket;
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

test "next: configured Workspace Scope selects within scope and overrides default ordering" {
    const gpa = std.testing.allocator;
    var tmp_store = try TmpStore.init(gpa, "project");
    defer tmp_store.deinit(gpa);
    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
    defer cwd.close(std.testing.io);
    const rev_parse = try tmp_store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "First", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "t2", .display = "project-2", .title = "Second", .created_seq = 2 });

    // Without scope, default ordering picks `project-1` first. With
    // `tk.scope = project-2` configured, `tk next` must return `project-2`.
    var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try h.fake_runner.expect(
        &.{ "git", "config", "--worktree", "--get", "tk.scope" },
        .{ .exit_code = 0, .stdout = "project-2\n" },
    );
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("project-2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
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
        try expectNoWorkspaceScope(&h);

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
    try expectNoWorkspaceScope(&h);

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.next_no_ready_ticket ++ "\n", h.stderr());
}

/// Register the pair of git invocations `readGitSide` makes when no
/// Workspace Scope is configured. Each call exits non-zero, so both
/// `tk.scope` lookup and branch-name inference collapse to `null`.
fn expectNoWorkspaceScope(h: *Harness) !void {
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
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
