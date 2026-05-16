const std = @import("std");
const clap = @import("clap");
const proc = @import("proc/runner.zig");
const clock_mod = @import("clock.zig");

/// Runtime services available to command handlers.
///
/// Commands receive these explicitly instead of reaching for process globals so
/// unit tests and txtar scenarios can substitute writers, clocks, working
/// directories, and subprocess runners.
pub const Deps = struct {
    /// Primary command output. Commands write user data here.
    stdout: *std.Io.Writer,
    /// Diagnostics and usage errors. Commands avoid writing normal output here.
    stderr: *std.Io.Writer,
    /// Primary command input. Commands read this only when their contract
    /// explicitly accepts stdin, such as `tk add -F -`.
    stdin: *std.Io.Reader,
    /// Allocator used for parsing and short-lived command work.
    gpa: std.mem.Allocator,
    /// I/O implementation handle, threaded through filesystem and subprocess
    /// calls. Tests use `std.testing.io`; main uses the runtime's init.io.
    io: std.Io,
    /// User's working directory at invocation time. Passed as the cwd for
    /// subprocess discovery (e.g. `git rev-parse`); filesystem operations on
    /// absolute paths (the Repository Store location) bypass it.
    cwd: std.Io.Dir,
    /// Captured-output subprocess runner. Real impl wraps `std.process.run`;
    /// tests inject a `FakeRunner`.
    runner: proc.Runner,
    /// UTC millisecond clock. Tests inject a `FakeClock` so timestamps stay
    /// deterministic.
    clock: clock_mod.Clock,
    /// Random source used for opaque internal IDs.
    random: std.Random,
};

/// Metadata every command module exports for dispatcher registration and help.
pub const CommandMeta = struct {
    /// CLI spelling, also used as the generated `SubCommand` tag name.
    name: [:0]const u8,
    /// One-line description shown in `tk --help`.
    description: []const u8,
};

const all_commands = .{
    @import("commands/init.zig"),
    @import("commands/add.zig"),
    @import("commands/done.zig"),
    @import("commands/list.zig"),
    @import("commands/next.zig"),
    @import("commands/prime.zig"),
    @import("commands/show.zig"),
    @import("commands/update.zig"),
};

/// Top-level subcommand enum generated from `all_commands`.
///
/// Adding a command module to `all_commands` is the only dispatcher touchpoint:
/// the enum, zig-clap parser, help listing, and dispatch switch all derive from
/// this compile-time tuple.
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

/// Parse top-level args and dispatch to a command handler.
///
/// Returns Ticket's process exit code contract: 0 success, 1 logical failure,
/// 2 usage error, and propagated unexpected errors for `main.zig` to translate
/// to exit 3.
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
        // TODO(ticket-2): prefix clap diagnostics with the command name —
        // applies symmetrically here for top-level parse failures.
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

test "runArgv returns 2 on extra positional after a subcommand" {
    // Trailing positionals after a known subcommand surface through the
    // subcommand parser; this test pins the dispatcher contract once
    // (exit 2 + non-empty stderr) so individual commands don't each have
    // to retest it.
    var h = Harness.init(std.testing.allocator, &.{ "prime", "unexpected" });
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
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "init") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "add") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "done") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "list") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--version") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}
