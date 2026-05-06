const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");

const prime_md_bytes = @embedFile("prime_md");

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

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

    const trimmed = std.mem.trimEnd(u8, prime_md_bytes, " \t\r\n");
    try deps.stdout.print("{s}\n", .{trimmed});
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk prime - print agent workflow context
        \\
        \\Prints the embedded workflow briefing to stdout. Designed to be invoked by
        \\an agent harness (e.g. Claude Code's SessionStart hook) at session start.
        \\
        \\Usage:
        \\  tk prime [options]
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &params, .{
        .description_on_new_line = false,
        .description_indent = 2,
        .indent = 2,
        .spacing_between_parameters = 0,
    });
}

test "prime writes embedded markdown with one trailing newline" {
    const expected_trimmed = std.mem.trimEnd(u8, prime_md_bytes, " \t\r\n");

    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = cli.Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    const SliceArgIter = @import("../testing/arg_iter.zig").SliceArgIter;
    var iter = SliceArgIter{ .items = &.{} };

    const code = try run(deps, &iter);

    try std.testing.expectEqual(@as(u8, 0), code);
    const written = stdout_buf.written();
    try std.testing.expect(written.len > 0);
    try std.testing.expectEqualStrings(expected_trimmed, written[0 .. written.len - 1]);
    try std.testing.expectEqual('\n', written[written.len - 1]);
    try std.testing.expectEqualStrings("", stderr_buf.written());
}

test "prime rejects unknown flag" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = cli.Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    const SliceArgIter = @import("../testing/arg_iter.zig").SliceArgIter;
    var iter = SliceArgIter{ .items = &.{"--bad-flag"} };

    const code = try run(deps, &iter);

    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(stderr_buf.written().len > 0);
}

test "prime rejects extra positional" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = cli.Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    const SliceArgIter = @import("../testing/arg_iter.zig").SliceArgIter;
    var iter = SliceArgIter{ .items = &.{"unexpected"} };

    const code = try run(deps, &iter);

    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(stderr_buf.written().len > 0);
}

test "prime --help prints help to stdout, exits 0" {
    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    const deps = cli.Deps{
        .stdout = &stdout_buf.writer,
        .stderr = &stderr_buf.writer,
        .gpa = std.testing.allocator,
    };

    const SliceArgIter = @import("../testing/arg_iter.zig").SliceArgIter;
    var iter = SliceArgIter{ .items = &.{"--help"} };

    const code = try run(deps, &iter);

    const out = stdout_buf.written();
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, out, "tk prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, out, "Options:") != null);
    try std.testing.expectEqualStrings("", stderr_buf.written());
}
