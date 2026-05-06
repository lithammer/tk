const std = @import("std");
const clap = @import("clap");

pub const Deps = struct {
    stdout: *std.Io.Writer,
    stderr: *std.Io.Writer,
    gpa: std.mem.Allocator,
};

pub const CommandMeta = struct {
    name: [:0]const u8,
    description: []const u8,
};

const all_commands = .{
    @import("commands/prime.zig"),
};

pub const SubCommand = blk: {
    const Tag = std.math.IntFittingRange(0, all_commands.len -| 1);
    var names: [all_commands.len][]const u8 = undefined;
    var values: [all_commands.len]Tag = undefined;
    for (all_commands, 0..) |cmd, i| {
        names[i] = cmd.meta.name;
        values[i] = @intCast(i);
    }
    break :blk @Enum(Tag, .exhaustive, &names, &values);
};

const VERSION = "v0.0.1";

const top_flag_text =
    \\-h, --help     Display this help and exit.
    \\-v, --version  Print version and exit.
    \\
;
const top_flags = clap.parseParamsComptime(top_flag_text);
const top_params = clap.parseParamsComptime(top_flag_text ++
    \\<command>
    \\
);
const top_parsers = .{ .command = clap.parsers.enumeration(SubCommand) };

const help_options: clap.HelpOptions = .{
    .description_on_new_line = false,
    .description_indent = 2,
    .indent = 2,
    .spacing_between_parameters = 0,
};

pub fn runArgv(deps: Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    // Treat every clap parse error as exit 2. The contract pins
    // error.OutOfMemory as exit 3, but Zig 0.16 with clap v0.12.0 and our
    // `enumeration` + `terminating_positional` config has OOM compiled out
    // of the inferred error set; if a future command's parser becomes able
    // to allocate fallibly, this catch must be split to let OOM propagate.
    var res = clap.parseEx(clap.Help, &top_params, top_parsers, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
        .terminating_positional = 0,
    }) catch |err| {
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeTopLevelHelp(deps) catch {};
        return 0;
    }
    if (res.args.version != 0) {
        deps.stdout.print(VERSION ++ "\n", .{}) catch {};
        return 0;
    }

    const subcmd = res.positionals[0] orelse {
        deps.stderr.writeAll("tk: missing subcommand; run 'tk --help' for usage\n") catch {};
        return 2;
    };

    return switch (subcmd) {
        inline else => |tag| blk: {
            inline for (all_commands) |cmd| {
                if (comptime std.mem.eql(u8, cmd.meta.name, @tagName(tag))) {
                    break :blk cmd.run(deps, args_iter);
                }
            }
            unreachable;
        },
    };
}

fn writeTopLevelHelp(deps: Deps) !void {
    try deps.stdout.writeAll(
        \\tk - an agent-first CLI for tracking work items
        \\
        \\Usage:
        \\  tk <command> [options]
        \\  tk [-h | --help]
        \\  tk [-v | --version]
        \\
        \\Commands:
        \\
    );
    inline for (all_commands) |cmd| {
        try deps.stdout.print("  {s:<10} {s}\n", .{ cmd.meta.name, cmd.meta.description });
    }
    try deps.stdout.writeAll(
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &top_flags, help_options);
    try deps.stdout.writeAll(
        \\
        \\Run 'tk <command> --help' for command-specific help.
        \\
    );
}

const Harness = @import("testing/test_cli.zig").Harness;

test "runArgv routes prime" {
    var h = Harness.init(std.testing.allocator, &.{"prime"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(h.stdout().len > 0);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv returns 2 on unknown subcommand" {
    var h = Harness.init(std.testing.allocator, &.{"bogus"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv returns 2 on missing subcommand" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv prints version" {
    var h = Harness.init(std.testing.allocator, &.{"--version"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings("v0.0.1\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv prints help" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Commands:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--version") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}
