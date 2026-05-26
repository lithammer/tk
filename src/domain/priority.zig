//! Priority is a local-only Ticket ranking.

const std = @import("std");

/// Local-only Ticket ranking. Lower numbers sort before higher numbers.
pub const Priority = enum {
    P0,
    P1,
    P2,
    P3,
    P4,

    /// Default Priority for newly-created local Tickets.
    pub const default: Priority = .P2;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: Priority) []const u8 {
        return switch (self) {
            .P0 => "P0",
            .P1 => "P1",
            .P2 => "P2",
            .P3 => "P3",
            .P4 => "P4",
        };
    }

    /// Write the CLI rendering string for use with the `{f}` specifier.
    /// Single-sources the unstyled representation on `text()`; styled render
    /// sites still wrap `text()` through the Styler.
    pub fn format(self: Priority, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.writeAll(self.text());
    }
};

test "Priority.format writes text() via {f}" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();
    try buf.writer.print("{f}", .{Priority.P1});
    try std.testing.expectEqualStrings("P1", buf.written());
}
