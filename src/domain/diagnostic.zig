//! Caller-owned scratch buffer for capturing a transient error message
//! before connection-level state (e.g. SQLite's per-connection errmsg
//! after rollback) clears it.
//!
//! Distinct from a Mutation Failure (CONTEXT.md), which is the persisted,
//! JSON-shaped, retry-classified record stored in `mutations.failure_json`
//! by the sync engine. A Diagnostic is ephemeral, string-shaped, and used
//! for human-readable diagnostics on the next stderr line.
//!
//! The single-producer pattern matches std.json.Scanner.Diagnostics and
//! std.zon.parse.Diagnostics: callers declare a Diagnostic on the stack,
//! pass `?*Diagnostic` into the fallible operation, and read back via
//! `.message()` after observing the error union.

const std = @import("std");

/// Stack-allocated, single-producer scratch buffer for capturing one
/// transient error string. Callers declare a Diagnostic on the stack,
/// pass `?*Diagnostic` into the fallible operation, and read back via
/// `.message()` after observing the error union.
pub const Diagnostic = struct {
    /// Fixed-size scratch storage. Bytes beyond the buffer length are
    /// truncated by `capture` without indication. Sized to fit a
    /// SQLite errmsg (typically under ~120 ASCII bytes in practice).
    buf: [256]u8 = undefined,
    /// Number of bytes captured by the most recent `capture` call.
    /// Zero before any capture and after `capture("")`.
    len: usize = 0,

    /// Bytes captured by the most recent `capture` call.
    /// Empty when no message has been captured.
    pub fn message(self: *const Diagnostic) []const u8 {
        return self.buf[0..self.len];
    }

    /// Copy `text` into the internal buffer, truncating if longer than
    /// the buffer can hold. Overwrites any previous capture, including
    /// resetting to empty when `text` is empty.
    pub fn capture(self: *Diagnostic, text: []const u8) void {
        const n = @min(text.len, self.buf.len);
        @memcpy(self.buf[0..n], text[0..n]);
        self.len = n;
    }
};

test "Diagnostic: empty by default" {
    var d: Diagnostic = .{};
    try std.testing.expectEqualStrings("", d.message());
}

test "Diagnostic: capture stores text" {
    var d: Diagnostic = .{};
    d.capture("table items already exists");
    try std.testing.expectEqualStrings("table items already exists", d.message());
}

test "Diagnostic: capture truncates oversize text" {
    var d: Diagnostic = .{};
    var long_buf: [512]u8 = undefined;
    @memset(long_buf[0..], 'x');
    d.capture(long_buf[0..]);
    try std.testing.expectEqual(@as(usize, 256), d.message().len);
}

test "Diagnostic: capture overwrites previous capture" {
    var d: Diagnostic = .{};
    d.capture("first");
    d.capture("second");
    try std.testing.expectEqualStrings("second", d.message());
}

test "Diagnostic: capture(empty) clears previous capture" {
    var d: Diagnostic = .{};
    d.capture("first");
    d.capture("");
    try std.testing.expectEqualStrings("", d.message());
}
