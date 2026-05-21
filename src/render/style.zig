//! Style: a paired ANSI SGR open/close byte sequence describing one visual
//! attribute (bold, a foreground color, etc.) that the Styler may emit
//! around text. Composable at comptime so palette.zig can express styles
//! like `red().bold()` without hand-concatenating SGR codes.

const std = @import("std");

/// Paired ANSI SGR open/close byte sequence. Empty `open`/`close` means
/// "no styling," which is the contract of `none()` and also the runtime
/// shape when the active color mode is `no_color`.
///
/// `Style.bold` is a chaining operator: it appends its open SGR code to
/// `open` and *prepends* its close to `close`, so `red().bold()` yields
/// `[31m[1m … [22m[39m` — opens in declaration order, closes in reverse
/// so attributes nest properly.
pub const Style = struct {
    open: []const u8,
    close: []const u8,

    /// Append bold to this Style.
    pub fn bold(self: Style) Style {
        return .{
            .open = self.open ++ "\x1b[1m",
            .close = "\x1b[22m" ++ self.close,
        };
    }
};

/// Bold attribute. The close (`22`) resets bold *and* dim — they share an
/// SGR family, so a bold span inside a dim outer (or vice versa) leaks.
pub fn bold() Style {
    return .{ .open = "\x1b[1m", .close = "\x1b[22m" };
}

pub fn red() Style {
    return .{ .open = "\x1b[31m", .close = "\x1b[39m" };
}

pub fn green() Style {
    return .{ .open = "\x1b[32m", .close = "\x1b[39m" };
}

pub fn yellow() Style {
    return .{ .open = "\x1b[33m", .close = "\x1b[39m" };
}

pub fn blue() Style {
    return .{ .open = "\x1b[34m", .close = "\x1b[39m" };
}

pub fn magenta() Style {
    return .{ .open = "\x1b[35m", .close = "\x1b[39m" };
}

/// Dim attribute. Shares its close (`22`) with `bold()` — see the nesting
/// constraint on `Style`'s doc comment.
pub fn dim() Style {
    return .{ .open = "\x1b[2m", .close = "\x1b[22m" };
}

/// Empty style. Identity under chaining: `none().bold()` equals `bold()`.
pub fn none() Style {
    return .{ .open = "", .close = "" };
}

test "bold() emits SGR 1 open and 22 close" {
    const s = bold();
    try std.testing.expectEqualStrings("\x1b[1m", s.open);
    try std.testing.expectEqualStrings("\x1b[22m", s.close);
}

test "red().bold() chains opens forward, closes in reverse" {
    const s = comptime red().bold();
    try std.testing.expectEqualStrings("\x1b[31m\x1b[1m", s.open);
    try std.testing.expectEqualStrings("\x1b[22m\x1b[39m", s.close);
}

test "none() is empty; chaining onto none() equals the leaf style" {
    const empty = none();
    try std.testing.expectEqualStrings("", empty.open);
    try std.testing.expectEqualStrings("", empty.close);

    const chained = comptime none().bold();
    const leaf = bold();
    try std.testing.expectEqualStrings(leaf.open, chained.open);
    try std.testing.expectEqualStrings(leaf.close, chained.close);
}
