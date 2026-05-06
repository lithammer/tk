const std = @import("std");
const clap = @import("clap");
const commands = struct {
    const prime = @import("commands/prime.zig");
};

pub const Deps = struct {
    stdout: *std.Io.Writer,
    stderr: *std.Io.Writer,
    gpa: std.mem.Allocator,
};

pub const SubCommand = enum { prime };

const VERSION = "v0.0.1";

const top_params = clap.parseParamsComptime(
    \\-h, --help     Display this help and exit.
    \\-v, --version  Print version and exit.
    \\<command>
    \\
);
const top_parsers = .{ .command = clap.parsers.enumeration(SubCommand) };

pub fn runArgv(deps: Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
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
        clap.help(deps.stdout, clap.Help, &top_params, .{}) catch {};
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
        .prime => commands.prime.run(deps, args_iter),
    };
}

const SliceArgIter = @import("testing/arg_iter.zig").SliceArgIter;

test "runArgv routes prime" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    var iter = SliceArgIter{ .items = &.{"prime"} };
    const code = try runArgv(deps, &iter);

    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(stdout_buf.written().len > 0);
    try std.testing.expectEqualStrings("", stderr_buf.written());
}

test "runArgv returns 2 on unknown subcommand" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    var iter = SliceArgIter{ .items = &.{"bogus"} };
    const code = try runArgv(deps, &iter);

    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(stderr_buf.written().len > 0);
}

test "runArgv returns 2 on missing subcommand" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    var iter = SliceArgIter{ .items = &.{} };
    const code = try runArgv(deps, &iter);

    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(stderr_buf.written().len > 0);
}

test "runArgv prints version" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    var iter = SliceArgIter{ .items = &.{"--version"} };
    const code = try runArgv(deps, &iter);

    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings("v0.0.1\n", stdout_buf.written());
    try std.testing.expectEqualStrings("", stderr_buf.written());
}

test "runArgv prints help" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    var iter = SliceArgIter{ .items = &.{"--help"} };
    const code = try runArgv(deps, &iter);

    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(stdout_buf.written().len > 0);
    try std.testing.expectEqualStrings("", stderr_buf.written());
}
