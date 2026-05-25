const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const embed = @import("../embed.zig");

const prime_md_bytes = @embedFile("prime.md");
comptime {
    embed.assertNoCR(prime_md_bytes);
}
const prime_output: []const u8 = std.mem.trimEnd(u8, prime_md_bytes, " \t\r\n") ++ "\n";

/// Dispatcher metadata for `tk prime`.
pub const meta: cli.CommandMeta = .{
    .name = "prime",
    .description = "Print agent workflow context to stdout",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\
);

/// Print the embedded agent briefing with exactly one trailing newline.
///
/// `tk prime` deliberately has no Repository Store precondition; it is safe for
/// agent session-start hooks before `tk init` has run.
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

    try deps.stdout.writeAll(prime_output);
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

const Harness = @import("../testing/test_cli.zig").Harness;

test "prime: writes embedded markdown with one trailing newline" {
    var h = Harness.init(std.testing.allocator, &.{}, .{});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expectEqualStrings(prime_output, h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "prime: rejects unknown flag" {
    var h = Harness.init(std.testing.allocator, &.{"--bad-flag"}, .{});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "prime: --help prints help to stdout, exits 0" {
    var h = Harness.init(std.testing.allocator, &.{"--help"}, .{});
    defer h.deinit();

    const code = try run(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "tk prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Options:") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}
