//! Item Status for Tickets and Epics.

const std = @import("std");

/// Lifecycle state shared by Tickets and Epics.
pub const ItemStatus = enum {
    open,
    active,
    done,

    /// Default Item Status for newly-created local work.
    pub const default: ItemStatus = .open;

    /// SQLite storage and CLI rendering string.
    pub fn text(self: ItemStatus) []const u8 {
        return switch (self) {
            .open => "open",
            .active => "active",
            .done => "done",
        };
    }

    /// Compact tree glyph used by `tk list` and `tk show` rendering.
    pub fn glyph(self: ItemStatus) []const u8 {
        return switch (self) {
            .open => "○",
            .active => "◐",
            .done => "✓",
        };
    }

    /// Write the CLI rendering string for use with the `{f}` specifier.
    /// Single-sources the unstyled representation on `text()`; the tree
    /// `glyph()` is a separate presentation and is intentionally not used here.
    pub fn format(self: ItemStatus, writer: *std.Io.Writer) std.Io.Writer.Error!void {
        try writer.writeAll(self.text());
    }
};

test "ItemStatus.format writes text() via {f}" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();
    try buf.writer.print("{f}", .{ItemStatus.active});
    try std.testing.expectEqualStrings("active", buf.written());
}
