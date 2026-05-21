//! Shared parsing and diagnostic rendering for clap-backed command handlers.

const std = @import("std");
const Allocator = std.mem.Allocator;

const clap = @import("clap");

const usage_hint_prefix = "run '";
const usage_hint_suffix = "' for usage";

/// The command whose clap parser is reporting a usage error.
pub const Command = union(enum) {
    top_level,
    subcommand: []const u8,
};

pub const ParseOptions = struct {
    stderr: *std.Io.Writer,
    allocator: Allocator,
    command: Command,
    terminating_positional: usize = std.math.maxInt(usize),
};

/// Parse args with clap. Usage errors render the standard two-line diagnostic
/// and return null; OutOfMemory propagates for `main.zig` to map to exit 3.
pub fn parseOrReportUsage(
    comptime Id: type,
    comptime params: []const clap.Param(Id),
    comptime value_parsers: anytype,
    iter: anytype,
    options: ParseOptions,
) !?clap.ResultEx(Id, params, value_parsers) {
    var diag: clap.Diagnostic = .{};
    return clap.parseEx(Id, params, value_parsers, iter, .{
        .diagnostic = &diag,
        .allocator = options.allocator,
        .terminating_positional = options.terminating_positional,
    }) catch |err| {
        if (err == error.OutOfMemory) return error.OutOfMemory;
        report(options.stderr, options.command, diag, err);
        return null;
    };
}

fn report(stderr: *std.Io.Writer, command: Command, diag: clap.Diagnostic, err: anyerror) void {
    writeCommand(stderr, command) catch {};
    stderr.writeAll(": ") catch {};
    diag.report(stderr, err) catch {};

    writeCommand(stderr, command) catch {};
    stderr.writeAll(": " ++ usage_hint_prefix) catch {};
    writeHelpInvocation(stderr, command) catch {};
    stderr.writeAll(usage_hint_suffix ++ "\n") catch {};
}

fn writeCommand(writer: *std.Io.Writer, command: Command) !void {
    switch (command) {
        .top_level => try writer.writeAll("tk"),
        .subcommand => |name| try writer.print("tk {s}", .{name}),
    }
}

fn writeHelpInvocation(writer: *std.Io.Writer, command: Command) !void {
    switch (command) {
        .top_level => try writer.writeAll("tk --help"),
        .subcommand => |name| try writer.print("tk {s} --help", .{name}),
    }
}

test "parse diagnostic prefixes clap invalid-argument output and usage hint" {
    var buf: [256]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buf);

    report(&writer, .{ .subcommand = "init" }, .{ .name = .{ .long = "bogus" } }, error.InvalidArgument);

    try std.testing.expectEqualStrings(
        "tk init: Invalid argument '--bogus'\n" ++
            "tk init: run 'tk init --help' for usage\n",
        writer.buffered(),
    );
}

test "parse diagnostic keeps generic clap error body verbatim" {
    var buf: [256]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buf);

    report(&writer, .top_level, .{}, error.NameNotPartOfEnum);

    try std.testing.expectEqualStrings(
        "tk: Error while parsing arguments: NameNotPartOfEnum\n" ++
            "tk: run 'tk --help' for usage\n",
        writer.buffered(),
    );
}

test "parseOrReportUsage returns null after rendering a standardized usage diagnostic" {
    const params = comptime clap.parseParamsComptime(
        \\-h, --help  Display this help and exit.
        \\
    );
    var iter = clap.args.SliceIterator{ .args = &.{"--bogus"} };
    var buf: [256]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buf);

    const res = try parseOrReportUsage(clap.Help, &params, clap.parsers.default, &iter, .{
        .stderr = &writer,
        .allocator = std.testing.allocator,
        .command = .{ .subcommand = "init" },
    });

    try std.testing.expect(res == null);
    try std.testing.expectEqualStrings(
        "tk init: Invalid argument '--bogus'\n" ++
            "tk init: run 'tk init --help' for usage\n",
        writer.buffered(),
    );
}
