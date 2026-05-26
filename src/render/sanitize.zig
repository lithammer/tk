//! Terminal rendering sanitizers for Repository Store text fields.
//!
//! Stored titles and bodies remain byte-for-byte. These helpers sit at
//! output boundaries so user/Remote-controlled text cannot emit bytes that
//! terminals interpret as SGR, OSC/APC, cursor movement, bell, or line editing
//! controls, while still making those bytes visible to developers.

const std = @import("std");

const TextShape = enum { line, body };
const Replacement = union(enum) { clean, space, escape, skip };

/// Write a single-line Repository Store field with terminal control bytes
/// rendered inert. CR/LF/Tab fold to spaces so titles, summaries, and blocker
/// reasons cannot change the surrounding row layout; other control bytes render
/// as lowercase `\xNN` text.
pub fn writeSanitizedLine(writer: *std.Io.Writer, text: []const u8) !void {
    try writeSanitized(writer, text, .line);
}

/// Write a multi-line Repository Store body with terminal control bytes
/// rendered inert. LF and Tab remain layout bytes for descriptions; CRLF is
/// normalized to LF, and all other control bytes render as lowercase `\xNN`
/// text.
pub fn writeSanitizedBody(writer: *std.Io.Writer, text: []const u8) !void {
    try writeSanitized(writer, text, .body);
}

fn writeSanitized(writer: *std.Io.Writer, text: []const u8, shape: TextShape) !void {
    var clean_start: usize = 0;
    var i: usize = 0;
    while (i < text.len) : (i += 1) {
        const replacement = classify(text, i, shape);
        switch (replacement) {
            .clean => {},
            .space => {
                try writer.writeAll(text[clean_start..i]);
                try writer.writeByte(' ');
                clean_start = i + 1;
            },
            .escape => {
                try writer.writeAll(text[clean_start..i]);
                try writeHex(writer, text[i]);
                clean_start = i + 1;
            },
            .skip => {
                try writer.writeAll(text[clean_start..i]);
                clean_start = i + 1;
            },
        }
    }
    try writer.writeAll(text[clean_start..]);
}

fn classify(text: []const u8, index: usize, shape: TextShape) Replacement {
    const byte = text[index];
    return switch (shape) {
        .line => if (byte == '\r' or byte == '\n' or byte == '\t')
            .space
        else if (std.ascii.isControl(byte))
            .escape
        else
            .clean,
        .body => if (byte == '\r')
            if (index + 1 < text.len and text[index + 1] == '\n') .skip else .escape
        else if (byte == '\n' or byte == '\t')
            .clean
        else if (std.ascii.isControl(byte))
            .escape
        else
            .clean,
    };
}

fn writeHex(writer: *std.Io.Writer, byte: u8) !void {
    const hex = std.fmt.bytesToHex([1]u8{byte}, .lower);
    const escaped = [_]u8{ '\\', 'x', hex[0], hex[1] };
    try writer.writeAll(&escaped);
}

test "writeSanitizedLine: folds whitespace and escapes controls" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();

    try writeSanitizedLine(&buf.writer, "Hello\r\n\tWorld!\x1b[31mBold\x07");
    try std.testing.expectEqualStrings("Hello   World!\\x1b[31mBold\\x07", buf.written());
}

test "writeSanitizedBody: preserves layout whitespace and escapes controls" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();

    const input = "Line 1\r\n\tLine 2\x1b[31mRed\x7f\rStandalone";
    try writeSanitizedBody(&buf.writer, input);
    try std.testing.expectEqualStrings("Line 1\n\tLine 2\\x1b[31mRed\\x7f\\x0dStandalone", buf.written());
}

test "writeSanitizedLine: writes clean text unchanged" {
    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();

    try writeSanitizedLine(&buf.writer, "plain title");
    try std.testing.expectEqualStrings("plain title", buf.written());
}
