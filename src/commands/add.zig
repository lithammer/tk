//! `tk add` — create a local Ticket from git-commit-style message input.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const message = @import("message.zig");
const repository = @import("../store/repository.zig");
const Priority = @import("../domain/priority.zig").Priority;
const TicketKind = @import("../domain/ticket_kind.zig").TicketKind;

/// Dispatcher metadata for `tk add`.
pub const meta: cli.CommandMeta = .{
    .name = "add",
    .description = "Create a local task Ticket",
};

const parsers = .{
    .str = clap.parsers.string,
    .PRIORITY = clap.parsers.enumeration(Priority),
};

const params = clap.parseParamsComptime(
    \\-h, --help              Display this help and exit.
    \\-m, --message <str>...  Message paragraph (repeatable; -m title -m body ...).
    \\-F, --file <str>...     Read the message from a file, or '-' for stdin.
    \\--bug                   Create a bug Ticket.
    \\--epic                  Create an Epic.
    \\--parent <str>          Place the new Ticket under an Epic by Display ID or Alias.
    \\--priority <PRIORITY>   Set Priority (P0..P4). Tickets only.
    \\
);

/// Parse `tk add` flags and create a local task Ticket.
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

    if (res.args.bug != 0 and res.args.epic != 0) {
        deps.stderr.writeAll(messages.add_conflicting_class_flags ++ "\n") catch {};
        return 2;
    }
    if (res.args.epic != 0 and res.args.priority != null) {
        deps.stderr.writeAll(messages.add_priority_on_epic ++ "\n") catch {};
        return 2;
    }
    if (res.args.epic != 0 and res.args.parent != null) {
        deps.stderr.writeAll(messages.add_parent_on_epic ++ "\n") catch {};
        return 2;
    }

    if (res.args.file.len > 1) {
        deps.stderr.writeAll(messages.add_repeated_file_flags ++ "\n") catch {};
        return 2;
    }
    const file_arg: ?[]const u8 = if (res.args.file.len == 1) res.args.file[0] else null;

    if (res.args.message.len > 0 and file_arg != null) {
        deps.stderr.writeAll(messages.add_conflicting_message_flags ++ "\n") catch {};
        return 2;
    }

    const input: message.Input = if (res.args.message.len > 0)
        .{ .paragraphs = res.args.message }
    else if (file_arg) |path|
        .{ .file = path }
    else {
        deps.stderr.writeAll(messages.add_message_required ++ "\n") catch {};
        return 2;
    };

    const parsed_msg = switch (try message.readInput(deps, input, input_msgs)) {
        .parsed => |pm| pm,
        .user_error => return 1,
    };
    defer parsed_msg.deinit(deps.gpa);

    const ticket_kind: TicketKind = if (res.args.bug != 0) .bug else TicketKind.default;
    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    var parent_ref: ?repository.ResolvedItemRefWithDisplay = null;
    defer if (parent_ref) |ref| ref.deinit(deps.gpa);
    if (res.args.parent) |parent_display| {
        const parent_outcome = repository.resolveAsEpicWithDisplay(store, deps.gpa, parent_display) catch |err| {
            renderStorageError(deps, err);
            return 1;
        };
        switch (parent_outcome) {
            .epic => |ref| parent_ref = ref,
            .not_found => {
                deps.stderr.print(
                    messages.add_parent_prefix ++ "{s}" ++ messages.add_parent_not_found_suffix ++ "\n",
                    .{parent_display},
                ) catch {};
                return 1;
            },
            .not_an_epic => |ref| {
                defer ref.deinit(deps.gpa);
                deps.stderr.print(
                    messages.add_parent_prefix ++ "{s}" ++ messages.add_parent_not_epic_suffix ++ "\n",
                    .{parent_display},
                ) catch {};
                return 1;
            },
        }
    }

    if (res.args.epic != 0) {
        const created = repository.createLocalEpic(store, deps.gpa, deps.clock, deps.random, .{
            .title = parsed_msg.title,
            .body = parsed_msg.body,
        }) catch |err| {
            renderStorageError(deps, err);
            return 1;
        };
        defer created.deinit(deps.gpa);

        deps.stdout.print(messages.add_created_epic_prefix ++ "{s} - {s}\n", .{ created.display_id, created.title }) catch {};
        deps.stdout.print(messages.add_status_label ++ "{s}\n", .{created.status.text()}) catch {};
        return 0;
    }

    const created = repository.createLocalTicket(store, deps.gpa, deps.clock, deps.random, .{
        .kind = ticket_kind,
        .priority = res.args.priority orelse Priority.default,
        .parent_id = if (parent_ref) |ref| ref.id else null,
        .title = parsed_msg.title,
        .body = parsed_msg.body,
    }) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    defer created.deinit(deps.gpa);

    deps.stdout.print(messages.add_created_ticket_prefix ++ "{s} - {s}\n", .{ created.display_id, created.title }) catch {};
    deps.stdout.print(messages.add_kind_label ++ "{s}\n", .{created.kind.text()}) catch {};
    deps.stdout.print(messages.add_priority_label ++ "{s}\n", .{created.priority.text()}) catch {};
    deps.stdout.print(messages.add_status_label ++ "{s}\n", .{created.status.text()}) catch {};
    if (parent_ref) |ref| {
        deps.stdout.print(messages.add_parent_label ++ "{s}\n", .{ref.display_id}) catch {};
    }
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk add - create a local task Ticket
        \\
        \\Creates a local task Ticket with Ticket Kind task and Priority P2 from
        \\git-commit-style message input. The first paragraph becomes the title;
        \\later paragraphs become the body.
        \\
        \\Usage:
        \\  tk add (-m <paragraph>... | -F <file | ->) [options]
        \\
        \\Options:
        \\
    );
    try deps.stdout.writeAll(
        \\  -h, --help              Display this help and exit.
        \\  -m, --message <text>    Message paragraph (repeatable).
        \\  -F, --file <file | ->   Read the message from a file, or '-' for stdin.
        \\  --bug                   Create a bug Ticket.
        \\  --epic                  Create an Epic.
        \\  --parent <id>           Place the new Ticket under an Epic.
        \\  --priority <P0..P4>     Set Priority. Tickets only.
        \\
    );
}

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.add_store_busy_retry,
    .out_of_memory = messages.add_out_of_memory,
    .fallback = messages.add_create_failed,
};

const input_msgs: message.InputMessages = .{
    .empty_message = messages.add_empty_message,
    .nul_message = messages.add_nul_message,
    .file_read_prefix = messages.add_file_read_prefix,
    .stdin_read_prefix = messages.add_stdin_read_prefix,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "add",
    .missing_store = messages.add_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
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
            \\Kind: task
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
            \\Kind: task
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

test "add: creates a local task Ticket from message paragraphs" {
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
        var h = Harness.initWith(gpa, &.{ "-m", "Write message flag", "-m", "Body paragraph." }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-1 - Write message flag
            \\Kind: task
            \\Priority: P2
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "add: creates a local task Ticket with requested Priority" {
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
        var h = Harness.initWith(gpa, &.{ "--priority", "P1", "-m", "Raise urgent follow-up" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-1 - Raise urgent follow-up
            \\Kind: task
            \\Priority: P1
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "add: creates a local bug Ticket" {
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
        var h = Harness.initWith(gpa, &.{ "--bug", "-m", "Investigate crash" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-1 - Investigate crash
            \\Kind: bug
            \\Priority: P2
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "add: creates a local Epic" {
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
        var h = Harness.initWith(gpa, &.{ "--epic", "-m", "Jira backend" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Epic: project-1 - Jira backend
            \\Status: open
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "add: rejects --bug and --epic together" {
    var h = Harness.init(std.testing.allocator, &.{ "--bug", "--epic", "-m", "Conflicting class" });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.add_conflicting_class_flags ++ "\n", h.stderr());
}

test "add: rejects --priority when creating an Epic" {
    var h = Harness.init(std.testing.allocator, &.{ "--epic", "--priority", "P1", "-m", "Epic title" });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.add_priority_on_epic ++ "\n", h.stderr());
}

test "add: creates a local Ticket under an Epic" {
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
        var h = Harness.initWith(gpa, &.{ "--epic", "-m", "Feature Epic" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{ "--parent", "project-1", "-m", "Build child Ticket" }, .{ .cwd = cwd });
        defer h.deinit();
        h.prng = std.Random.DefaultPrng.init(1);
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings(
            \\Created Ticket: project-2 - Build child Ticket
            \\Kind: task
            \\Priority: P2
            \\Status: open
            \\Parent: project-1
            \\
        , h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "add: rejects --parent when creating an Epic" {
    var h = Harness.init(std.testing.allocator, &.{ "--epic", "--parent", "project-1", "-m", "Nested Epic" });
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.add_parent_on_epic ++ "\n", h.stderr());
}

test "add: rejects --parent that resolves to nothing" {
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
        var h = Harness.initWith(gpa, &.{ "--parent", "missing-epic", "-m", "Child" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.add_parent_prefix ++ "missing-epic" ++ messages.add_parent_not_found_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "add: rejects --parent that resolves to a Ticket" {
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
        var h = Harness.initWith(gpa, &.{ "-m", "Existing Ticket" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{ "--parent", "project-1", "-m", "Child" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("", h.stdout());
        try std.testing.expectEqualStrings(
            messages.add_parent_prefix ++ "project-1" ++ messages.add_parent_not_epic_suffix ++ "\n",
            h.stderr(),
        );
    }
}

test "add: prints current parent Display ID when --parent uses an Alias" {
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
        var h = Harness.initWith(gpa, &.{ "--epic", "-m", "Current Epic" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    const parent_row = (try conn.row("select id from items where display_value = 'project-1'", .{})) orelse return error.ExpectedRow;
    const parent_id = try gpa.dupe(u8, parent_row.text(0));
    parent_row.deinit();
    defer gpa.free(parent_id);
    try TmpStore.insertAlias(conn, "legacy-epic", parent_id);

    {
        var h = Harness.initWith(gpa, &.{ "--parent", "legacy-epic", "-m", "Alias child" }, .{ .cwd = cwd });
        defer h.deinit();
        h.prng = std.Random.DefaultPrng.init(1);
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Parent: project-1\n") != null);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Parent: legacy-epic\n") == null);
        try std.testing.expectEqualStrings("", h.stderr());
    }
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
    try std.testing.expectEqualStrings(messages.add_repeated_file_flags ++ "\n", h.stderr());
}

test "add: help describes only the implemented slice" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk add (-m <paragraph>... | -F <file | ->)") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Ticket Kind task and Priority P2") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "<str>...") == null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "-m, --message") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--bug") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--epic") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--priority") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--parent") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}
