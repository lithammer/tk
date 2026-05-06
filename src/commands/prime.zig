const std = @import("std");
const cli = @import("../cli.zig");

const prime_md_bytes = @embedFile("prime_md");

pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    _ = args_iter;
    const trimmed = std.mem.trimEnd(u8, prime_md_bytes, " \t\r\n");
    try deps.stdout.print("{s}\n", .{trimmed});
    return 0;
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
    var noop_iter = SliceArgIter{ .items = &.{} };

    const code = try run(deps, &noop_iter);

    try std.testing.expectEqual(@as(u8, 0), code);
    const written = stdout_buf.written();
    try std.testing.expect(written.len > 0);
    try std.testing.expectEqualStrings(expected_trimmed, written[0 .. written.len - 1]);
    try std.testing.expectEqual('\n', written[written.len - 1]);
    try std.testing.expectEqualStrings("", stderr_buf.written());
}
