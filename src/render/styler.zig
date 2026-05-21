//! Styler: runtime policy gate for the comptime-composed Styles in
//! palette.zig. Carries one resolved color mode per output stream
//! (stdout, stderr); commands styling output reach for a sub-styler via
//! `styler.forStdout()` / `styler.forStderr()` and treat the returned
//! handle as the policy-aware emitter.
//!
//! The color mode is `std.Io.Terminal.Mode` (`no_color`, `escape_codes`,
//! `windows_api`). tk emits SGR escape codes only, so anything other than
//! `.escape_codes` produces empty open/close bytes. `Mode.detect` (called
//! from `main.zig` when building `cli.Deps`) resolves
//! `--color=auto|always|never` plus `NO_COLOR` and `CLICOLOR_FORCE`; the
//! `.windows_api` arm is intentionally treated as `.no_color` here since
//! the Windows console-attribute path is incompatible with our paired
//! open/close emitter. See
//! docs/adr/0014-comptime-style-emitter-with-runtime-policy.md.

const std = @import("std");
const style_mod = @import("style.zig");
const palette = @import("palette.zig");
const Style = style_mod.Style;

/// Per-stream color mode. Reuses stdlib's `std.Io.Terminal.Mode` so the
/// resolution chain (`NO_COLOR`, `CLICOLOR_FORCE`, TTY, Windows VT enable)
/// lives once in stdlib rather than re-rolled here.
pub const Mode = std.Io.Terminal.Mode;

/// Process-wide styler carried on cli.Deps. Holds per-stream modes so a
/// piped stdout (`tk list | less`) does not silence color on stderr.
pub const Styler = struct {
    stdout: Mode,
    stderr: Mode,

    /// Sub-styler bound to the stdout mode. Use when wrapping content
    /// destined for `deps.stdout`.
    pub fn forStdout(self: Styler) SubStyler {
        return .{ .mode = self.stdout };
    }

    /// Sub-styler bound to the stderr mode. Use when wrapping content
    /// destined for `deps.stderr`. The stderr palette currently has no
    /// entries — only the plumbing is in place.
    pub fn forStderr(self: Styler) SubStyler {
        return .{ .mode = self.stderr };
    }
};

/// Policy-aware emitter for one output stream. Returned by
/// `Styler.forStdout()` / `Styler.forStderr()`; callers use `wrap` for
/// single-span styled text or `open`/`close` to bracket multi-write rows.
pub const SubStyler = struct {
    mode: Mode,

    /// Wrap `text` in `style`'s SGR open/close. The returned Styled has a
    /// `format` method, so call sites slot it into `print()` as a `{f}`
    /// argument. When the mode is not `.escape_codes`, the wrapper's
    /// open/close are empty and `format` writes only the text —
    /// byte-identical to plain output.
    pub fn wrap(self: SubStyler, style: Style, text: []const u8) Styled {
        return .{
            .open = self.open(style),
            .text = text,
            .close = self.close(style),
        };
    }

    /// Bare SGR open bytes for `style`. Use to bracket an outer span that
    /// covers multiple writes (e.g. dim the whole row for a blocked item)
    /// while inner `wrap` calls handle individual spans. Returns "" unless
    /// the mode is `.escape_codes`.
    pub fn open(self: SubStyler, style: Style) []const u8 {
        return if (self.mode == .escape_codes) style.open else "";
    }

    /// Closing SGR bytes for `style`. Pair with a prior `open(style)` at
    /// the end of a multi-write outer span. Returns "" unless the mode is
    /// `.escape_codes`.
    pub fn close(self: SubStyler, style: Style) []const u8 {
        return if (self.mode == .escape_codes) style.close else "";
    }
};

/// Format-value returned by `SubStyler.wrap`. Implements `format` so it
/// composes with `print()` format strings as `{f}`.
pub const Styled = struct {
    open: []const u8,
    text: []const u8,
    close: []const u8,

    /// Write `open ++ text ++ close` to `writer`. Open/close are already
    /// mode-gated by `wrap`, so this method does not consult the mode.
    pub fn format(self: Styled, writer: *std.Io.Writer) !void {
        try writer.writeAll(self.open);
        try writer.writeAll(self.text);
        try writer.writeAll(self.close);
    }
};

test "SubStyler.open emits style.open when mode is escape_codes, empty otherwise" {
    const fake: Style = .{ .open = "[OPEN]", .close = "[CLOSE]" };
    const on = SubStyler{ .mode = .escape_codes };
    const off = SubStyler{ .mode = .no_color };
    try std.testing.expectEqualStrings("[OPEN]", on.open(fake));
    try std.testing.expectEqualStrings("", off.open(fake));
}

test "SubStyler.close emits style.close when mode is escape_codes, empty otherwise" {
    const fake: Style = .{ .open = "[OPEN]", .close = "[CLOSE]" };
    const on = SubStyler{ .mode = .escape_codes };
    const off = SubStyler{ .mode = .no_color };
    try std.testing.expectEqualStrings("[CLOSE]", on.close(fake));
    try std.testing.expectEqualStrings("", off.close(fake));
}

test "forStderr uses stderr mode independently of stdout" {
    const styler: Styler = .{ .stdout = .no_color, .stderr = .escape_codes };
    const fake: Style = .{ .open = "[OPEN]", .close = "[CLOSE]" };

    var stdout_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stdout_buf.deinit();
    var stderr_buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer stderr_buf.deinit();

    try styler.forStdout().wrap(fake, "TEXT").format(&stdout_buf.writer);
    try styler.forStderr().wrap(fake, "TEXT").format(&stderr_buf.writer);

    try std.testing.expectEqualStrings("TEXT", stdout_buf.written());
    try std.testing.expectEqualStrings("[OPEN]TEXT[CLOSE]", stderr_buf.written());
}

test "palette emits expected bytes under each mode" {
    const Case = struct {
        name: []const u8,
        style: Style,
        on_open: []const u8,
        on_close: []const u8,
    };
    const cases = [_]Case{
        .{ .name = "header", .style = palette.header, .on_open = "\x1b[1m", .on_close = "\x1b[22m" },
        .{ .name = "id_epic", .style = palette.id_epic, .on_open = "", .on_close = "" },
        .{ .name = "id_ticket", .style = palette.id_ticket, .on_open = "", .on_close = "" },
        .{ .name = "kind_bug", .style = palette.kind_bug, .on_open = "\x1b[31m", .on_close = "\x1b[39m" },
        .{ .name = "kind_epic", .style = palette.kind_epic, .on_open = "\x1b[35m", .on_close = "\x1b[39m" },
        .{ .name = "status_open", .style = palette.status_open, .on_open = "", .on_close = "" },
        .{ .name = "status_active", .style = palette.status_active, .on_open = "\x1b[33m", .on_close = "\x1b[39m" },
        .{ .name = "status_done", .style = palette.status_done, .on_open = "\x1b[32m", .on_close = "\x1b[39m" },
        .{ .name = "blocked", .style = palette.blocked, .on_open = "", .on_close = "" },
        .{ .name = "blocked_row", .style = palette.blocked_row, .on_open = "\x1b[2m", .on_close = "\x1b[22m" },
        .{ .name = "separator", .style = palette.separator, .on_open = "\x1b[2m", .on_close = "\x1b[22m" },
        .{ .name = "priority_p0", .style = palette.priority_p0, .on_open = "\x1b[31m", .on_close = "\x1b[39m" },
        .{ .name = "priority_p1", .style = palette.priority_p1, .on_open = "\x1b[33m", .on_close = "\x1b[39m" },
        .{ .name = "priority_p2", .style = palette.priority_p2, .on_open = "", .on_close = "" },
        .{ .name = "priority_p3", .style = palette.priority_p3, .on_open = "", .on_close = "" },
        .{ .name = "priority_p4", .style = palette.priority_p4, .on_open = "", .on_close = "" },
    };

    const on: SubStyler = .{ .mode = .escape_codes };
    const off: SubStyler = .{ .mode = .no_color };

    for (cases) |c| {
        errdefer std.debug.print("palette entry: {s}\n", .{c.name});
        try std.testing.expectEqualStrings(c.on_open, on.open(c.style));
        try std.testing.expectEqualStrings(c.on_close, on.close(c.style));
        try std.testing.expectEqualStrings("", off.open(c.style));
        try std.testing.expectEqualStrings("", off.close(c.style));
    }
}

test "Styler.forStdout().wrap elides open + close when mode is no_color" {
    const styler: Styler = .{ .stdout = .no_color, .stderr = .no_color };
    const fake: Style = .{ .open = "[OPEN]", .close = "[CLOSE]" };
    const styled = styler.forStdout().wrap(fake, "TEXT");

    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();
    try styled.format(&buf.writer);

    try std.testing.expectEqualStrings("TEXT", buf.written());
}

test "Styler.forStdout().wrap emits open + text + close when mode is escape_codes" {
    const styler: Styler = .{ .stdout = .escape_codes, .stderr = .no_color };
    const fake: Style = .{ .open = "[OPEN]", .close = "[CLOSE]" };
    const styled = styler.forStdout().wrap(fake, "TEXT");

    var buf: std.Io.Writer.Allocating = .init(std.testing.allocator);
    defer buf.deinit();
    try styled.format(&buf.writer);

    try std.testing.expectEqualStrings("[OPEN]TEXT[CLOSE]", buf.written());
}
