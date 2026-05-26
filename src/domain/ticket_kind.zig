//! Ticket Kind classifies Tickets as tasks or bugs.

const std = @import("std");

/// The category of a Ticket.
pub const TicketKind = enum {
    task,
    bug,

    /// Default Ticket Kind for `tk add` until `--bug` is implemented.
    pub const default: TicketKind = .task;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: TicketKind) []const u8 {
        return switch (self) {
            .task => "task",
            .bug => "bug",
        };
    }

    /// Write the CLI rendering string for use with the `{f}` specifier.
    /// Single-sources the unstyled representation on `text()`; styled render
    /// sites still wrap `text()` through the Styler.
    pub fn format(self: TicketKind, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.writeAll(self.text());
    }
};

test "TicketKind.format writes text() via {f}" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();
    try buf.writer.print("{f}", .{TicketKind.bug});
    try std.testing.expectEqualStrings("bug", buf.written());
}
