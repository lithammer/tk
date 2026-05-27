//! `tk update` — update the title, body, priority, or parent of a Ticket or Epic.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const message = @import("message.zig");
const repository = @import("../store/repository.zig");
const resolver = @import("resolver.zig");
const Priority = @import("../domain/priority.zig").Priority;
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const output = @import("output.zig");

/// Dispatcher metadata for `tk update`.
pub const meta: cli.CommandMeta = .{
    .name = "update",
    .description = "Update the title, body, priority, or parent of a Ticket or Epic",
};

const parsers = .{
    .str = clap.parsers.string,
    .PRIORITY = clap.parsers.enumeration(Priority),
};

const params = clap.parseParamsComptime(
    \\-h, --help              Display this help and exit.
    \\-m, --message <str>...  Message paragraph (repeatable; -m title -m body ...).
    \\-F, --file <str>        Read the message from a file, or '-' for stdin.
    \\--priority <PRIORITY>   Set Priority (P0..P4). Tickets only.
    \\--parent <str>          Set the containing Epic by Display ID or Alias. Tickets only.
    \\--no-parent             Remove the Ticket from its current Epic. Tickets only.
    \\<str>
    \\
);

/// Parse `tk update` args, apply field changes, and write the result.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &params, parsers, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .{ .subcommand = meta.name },
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    // Argument-only mutex checks (no store required).
    if (res.args.message.len > 0 and res.args.file != null) {
        deps.stderr.writeAll(messages.update_conflicting_message_flags ++ "\n") catch {};
        return 2;
    }
    if (res.args.parent != null and res.args.@"no-parent" != 0) {
        deps.stderr.writeAll(messages.update_conflicting_parent_flags ++ "\n") catch {};
        return 2;
    }

    const id = res.positionals[0] orelse {
        deps.stderr.writeAll(messages.update_id_required ++ "\n") catch {};
        return 2;
    };

    const has_message_input = res.args.message.len > 0 or res.args.file != null;
    const has_parent_op = res.args.parent != null or res.args.@"no-parent" != 0;
    if (!has_message_input and res.args.priority == null and !has_parent_op) {
        deps.stderr.writeAll(messages.update_no_changes_requested ++ "\n") catch {};
        return 2;
    }

    var parsed_msg: ?message.ParsedMessage = null;
    defer if (parsed_msg) |pm| pm.deinit(deps.gpa);

    if (res.args.message.len > 0) {
        parsed_msg = switch (try message.readInput(deps, .{ .paragraphs = res.args.message }, input_msgs)) {
            .parsed => |pm| pm,
            .user_error => return 1,
        };
    } else if (res.args.file) |path| {
        parsed_msg = switch (try message.readInput(deps, .{ .file = path }, input_msgs)) {
            .parsed => |pm| pm,
            .user_error => return 1,
        };
    }

    const r = resolver.open(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer r.close();
    const store = r.store;

    // Resolve the item id.
    const resolved = r.resolve(id, .{
        .prefix = messages.update_id_not_found_prefix,
        .suffix = messages.update_id_not_found_suffix,
    }) orelse return 1;
    defer resolved.deinit(deps.gpa);

    // Class-level validation (requires resolved class).
    if (resolved.item_class == .epic) {
        if (res.args.priority != null) {
            deps.stderr.writeAll(messages.update_priority_on_epic ++ "\n") catch {};
            return 2;
        }
        if (res.args.parent != null or res.args.@"no-parent" != 0) {
            deps.stderr.writeAll(messages.update_parent_on_epic ++ "\n") catch {};
            return 2;
        }
    }

    // Resolve --parent if supplied.
    var parent_op: repository.ParentOp = .unchanged;
    var parent_ref: ?repository.ResolvedItemRef = null;
    defer if (parent_ref) |ref| ref.deinit(deps.gpa);

    if (res.args.parent) |parent_display| {
        const epic = r.resolveEpic(parent_display, .{
            .prefix = messages.update_parent_prefix,
            .not_found_suffix = messages.update_parent_not_found_suffix,
            .not_epic_suffix = messages.update_parent_not_epic_suffix,
        }) orelse return 1;
        parent_op = .{ .set = epic.id };
        parent_ref = epic;
    } else if (res.args.@"no-parent" != 0) {
        parent_op = .clear;
    }

    const req: repository.UpdateRequest = .{
        .id = resolved.id,
        .item_class = resolved.item_class,
        .title = if (parsed_msg) |pm| pm.title else null,
        .body = if (parsed_msg) |pm| pm.body else null,
        .priority = res.args.priority,
        .parent = parent_op,
    };

    const outcome = repository.updateItem(store, deps.gpa, deps.clock, req) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    switch (outcome) {
        .ok => |updated| {
            defer updated.deinit(deps.gpa);
            const prefix: []const u8 = switch (updated.item_class) {
                .ticket => messages.update_success_ticket_prefix,
                .epic => messages.update_success_epic_prefix,
            };
            output.writeItemTitleLine(deps.stdout, prefix, updated.display_id, updated.title) catch {};
        },
        // Race window: the resolved row may have been deleted between
        // `resolveItemRef` and the BEGIN IMMEDIATE inside `updateItem`.
        .not_found => {
            deps.stderr.print(messages.update_id_not_found_prefix ++ "{s}" ++ messages.update_id_not_found_suffix ++ "\n", .{id}) catch {};
            return 1;
        },
    }
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk update - update a Ticket or Epic
        \\
        \\Usage:
        \\  tk update <id> [options]
        \\
        \\Options:
        \\  -h, --help              Display this help and exit.
        \\  -m, --message <str>     Message paragraph (repeatable).
        \\  -F, --file <file | ->   Read the message from a file, or '-' for stdin.
        \\  --priority <P0..P4>     Set Priority. Tickets only.
        \\  --parent <id>           Set the containing Epic. Tickets only.
        \\  --no-parent             Remove the Ticket from its Epic. Tickets only.
        \\
    );
}

const storage_msgs: resolver.StorageErrorMessages = .{
    .busy_retry = messages.update_store_busy_retry,
    .out_of_memory = messages.update_out_of_memory,
    .fallback = messages.update_write_failed,
};

const input_msgs: message.InputMessages = .{
    .empty_message = messages.update_empty_message,
    .nul_message = messages.update_nul_message,
    .file_read_prefix = messages.update_file_read_prefix,
    .stdin_read_prefix = messages.update_stdin_read_prefix,
};

const open_msgs: resolver.OpenMessages = .{
    .command_name = "update",
    .missing_store = messages.update_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    resolver.renderStorageError(deps.stderr, err, storage_msgs);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const zqlite = @import("zqlite");
const init_command = @import("init.zig");
const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;

test "update: --help prints usage and exits 0" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"--help"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    const out = h.stdout();
    try std.testing.expect(std.mem.indexOf(u8, out, "tk update") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "-m, --message") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "-F, --file") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "--priority") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "--parent") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "--no-parent") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "update: requires a positional id" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.update_id_required ++ "\n", h.stderr());
}

test "update: rejects -m and -F together" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{ "tk-1", "-m", "title", "-F", "file.md" }, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.update_conflicting_message_flags ++ "\n", h.stderr());
}

test "update: rejects --parent and --no-parent together" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{ "tk-1", "--parent", "epic-1", "--no-parent" }, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.update_conflicting_parent_flags ++ "\n", h.stderr());
}

test "update: updates a local Ticket via -m flags" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Old title",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "-m", "New title" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "project-1") != null);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "New title") != null);
        try std.testing.expectEqualStrings("", h.stderr());
    }

    const row = (try conn.row("select title from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("New title", row.text(0));

    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "update: updates a Backend Ticket and emits a Mutation" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt1",
        .display = "GH#5",
        .title = "Backend ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "5",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "GH#5", "-m", "Updated title" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));
}

test "update: reports unknown id as exit 1 with diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    {
        var h = Harness.init(gpa, &.{ "no-such-id", "-m", "Title" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.update_id_not_found_prefix ++ "no-such-id" ++ messages.update_id_not_found_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "update: reports missing store as exit 1" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    var h = Harness.init(gpa, &.{ "project-1", "-m", "Title" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.update_missing_store ++ "\n", h.stderr());
}

test "update: rejects --priority on an Epic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "--priority", "P0" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(messages.update_priority_on_epic ++ "\n", h.stderr());
    }
}

test "update: rejects --parent on an Epic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "--parent", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(messages.update_parent_on_epic ++ "\n", h.stderr());
    }
}

test "update: rejects --no-parent on an Epic target" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "--no-parent" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(messages.update_parent_on_epic ++ "\n", h.stderr());
    }
}

test "update: rejects --parent that resolves to nothing" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Ticket one",
        .created_seq = 1,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "--parent", "no-such-epic" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.update_parent_prefix ++ "no-such-epic" ++ messages.update_parent_not_found_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "update: rejects --parent that resolves to a Ticket instead of an Epic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Ticket one",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t2",
        .display = "project-2",
        .title = "Ticket two",
        .created_seq = 2,
    });

    {
        var h = Harness.init(gpa, &.{ "project-1", "--parent", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.update_parent_prefix ++ "project-2" ++ messages.update_parent_not_epic_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "update: --no-parent removes Ticket from its Epic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-2",
        .title = "Child ticket",
        .container_id = "e1",
        .created_seq = 2,
    });

    {
        var h = Harness.init(gpa, &.{ "project-2", "--no-parent" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    const row = (try conn.row("select container_id from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqual(@as(?[]const u8, null), row.nullableText(0));
}

test "update: --no-parent on a Backend Ticket emits remove_ticket_from_epic Mutation" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "e1",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt",
        .display = "GH#9",
        .title = "Backend ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "9",
        .container_id = "e1",
        .created_seq = 2,
    });

    {
        var h = Harness.init(gpa, &.{ "GH#9", "--no-parent" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    try std.testing.expectEqual(@as(i64, 1), try TmpStore.mutationCount(conn));
    const mrow = (try conn.row(
        "select mutation_type, payload_json from mutations where sequence = 1",
        .{},
    )) orelse return error.ExpectedRow;
    defer mrow.deinit();
    try std.testing.expectEqualStrings("remove_ticket_from_epic", mrow.text(0));
    try std.testing.expectEqualStrings("{\"epic_id\":\"e1\"}", mrow.text(1));
}

test "update: reads message from -F file" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Old",
        .created_seq = 1,
    });

    try cwd.writeFile(std.testing.io, .{ .sub_path = "msg.md", .data = "From file\n" });

    {
        var h = Harness.init(gpa, &.{ "project-1", "-F", "msg.md" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    const row = (try conn.row("select title from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("From file", row.text(0));
}

test "update: no editing intent is a usage error" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"project-1"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.update_no_changes_requested ++ "\n", h.stderr());
}

test "update: backend Ticket parent move emits remove and add Mutations" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "old-epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Old Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "new-epic",
        .display = "project-2",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "New Epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt",
        .display = "GH#5",
        .title = "Backend ticket",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "5",
        .container_id = "old-epic",
        .created_seq = 3,
    });

    {
        var h = Harness.init(gpa, &.{ "GH#5", "--parent", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    try std.testing.expectEqual(@as(i64, 2), try TmpStore.mutationCount(conn));

    const expected: [2][]const u8 = .{ "remove_ticket_from_epic", "add_ticket_to_epic" };
    var rows = try conn.rows(
        "select mutation_type from mutations order by sequence asc",
        .{},
    );
    defer rows.deinit();
    var i: usize = 0;
    while (rows.next()) |r| : (i += 1) {
        try std.testing.expect(i < expected.len);
        try std.testing.expectEqualStrings(expected[i], r.text(0));
    }
    if (rows.err) |err| return err;
    try std.testing.expectEqual(expected.len, i);
}

test "update: backend Ticket combined title + parent move emits three Mutations" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "old-epic",
        .display = "project-1",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "Old Epic",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "new-epic",
        .display = "project-2",
        .item_class = "epic",
        .ticket_kind = null,
        .priority = null,
        .title = "New Epic",
        .created_seq = 2,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "bt",
        .display = "GH#5",
        .title = "Old title",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "5",
        .container_id = "old-epic",
        .created_seq = 3,
    });

    {
        var h = Harness.init(gpa, &.{ "GH#5", "-m", "New title", "--parent", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });

        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stderr());
    }

    try std.testing.expectEqual(@as(i64, 3), try TmpStore.mutationCount(conn));

    const expected: [3][]const u8 = .{ "remove_ticket_from_epic", "add_ticket_to_epic", "update_ticket" };
    var rows = try conn.rows(
        "select mutation_type from mutations order by sequence asc",
        .{},
    );
    defer rows.deinit();
    var i: usize = 0;
    while (rows.next()) |r| : (i += 1) {
        try std.testing.expect(i < expected.len);
        try std.testing.expectEqualStrings(expected[i], r.text(0));
    }
    if (rows.err) |err| return err;
    try std.testing.expectEqual(expected.len, i);
}

test "update: maps busy and OOM storage errors to dedicated diagnostics" {
    var busy = Harness.init(std.testing.allocator, &.{}, .{});
    defer busy.deinit();
    renderStorageError(busy.deps(), error.Busy);
    try std.testing.expectEqualStrings(messages.update_store_busy_retry ++ "\n", busy.stderr());

    var oom = Harness.init(std.testing.allocator, &.{}, .{});
    defer oom.deinit();
    renderStorageError(oom.deps(), error.OutOfMemory);
    try std.testing.expectEqualStrings(messages.update_out_of_memory ++ "\n", oom.stderr());
}
