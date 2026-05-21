const std = @import("std");
const Allocator = std.mem.Allocator;

/// One parsed txtar section.
pub const Section = struct {
    /// Section name between `-- ` and ` --`; empty for a prelude.
    name: []const u8,
    /// Section body bytes, borrowed from the original txtar input.
    body: []const u8,
};

/// Scenario section containing testscript-style commands.
pub const section_script = "script";
/// Expected aggregate stdout from all `tk` commands in the script.
pub const section_expected_stdout = "expected/stdout";
/// Expected aggregate stderr from all `tk` commands in the script.
pub const section_expected_stderr = "expected/stderr";
/// Expected exit code from the final `tk` command in the script.
pub const section_expected_exit = "expected/exit";
/// Prefix for fixture files materialized under `$WORK`.
pub const section_input_prefix = "input/";

/// Return the first section with `name`, or null when absent.
pub fn findSection(sections: []const Section, name: []const u8) ?*const Section {
    for (sections) |*sec| {
        if (std.mem.eql(u8, sec.name, name)) return sec;
    }
    return null;
}

const PRELUDE = "";

/// Parse txtar bytes into borrowed sections.
///
/// The returned slice is owned by `allocator`, while section names and bodies
/// point into `data`; callers must keep `data` alive for as long as they use
/// the parsed sections.
pub fn parse(allocator: Allocator, data: []const u8) ![]Section {
    var sections: std.ArrayList(Section) = .empty;
    errdefer sections.deinit(allocator);

    var pos: usize = 0;
    var current_name: ?[]const u8 = PRELUDE;
    var current_start: usize = 0;

    while (pos < data.len) {
        const line_end = std.mem.indexOfScalarPos(u8, data, pos, '\n') orelse data.len;
        const line = data[pos..line_end];
        const after_line = if (line_end < data.len) line_end + 1 else data.len;

        if (isSectionHeader(line)) |name| {
            if (current_name) |cn| {
                if (cn.len > 0 or pos > 0) {
                    try sections.append(allocator, .{ .name = cn, .body = data[current_start..pos] });
                }
            }
            current_name = name;
            current_start = after_line;
        }
        pos = after_line;
    }

    if (current_name) |cn| {
        if (cn.len > 0 or current_start < data.len) {
            try sections.append(allocator, .{ .name = cn, .body = data[current_start..] });
        }
    }

    return sections.toOwnedSlice(allocator);
}

fn isSectionHeader(line: []const u8) ?[]const u8 {
    const trimmed = std.mem.trimEnd(u8, line, " \t\r");
    if (trimmed.len < 7) return null;
    if (!std.mem.startsWith(u8, trimmed, "-- ")) return null;
    if (!std.mem.endsWith(u8, trimmed, " --")) return null;
    return trimmed[3 .. trimmed.len - 3];
}

/// Serialize sections back to txtar bytes, adding a trailing newline to each
/// non-empty section body when needed.
pub fn serialize(allocator: Allocator, sections: []const Section) ![]u8 {
    var buf: std.ArrayList(u8) = .empty;
    errdefer buf.deinit(allocator);
    for (sections) |sec| {
        if (sec.name.len == 0) {
            try buf.appendSlice(allocator, sec.body);
            continue;
        }
        try buf.print(allocator, "-- {s} --\n", .{sec.name});
        try buf.appendSlice(allocator, sec.body);
        if (sec.body.len > 0 and sec.body[sec.body.len - 1] != '\n') {
            try buf.append(allocator, '\n');
        }
    }
    return buf.toOwnedSlice(allocator);
}

test "parse two sections" {
    const allocator = std.testing.allocator;
    const input =
        \\-- script --
        \\tk prime
        \\-- expected/stdout --
        \\hello
        \\
    ;

    const sections = try parse(allocator, input);
    defer allocator.free(sections);

    try std.testing.expectEqual(@as(usize, 2), sections.len);
    try std.testing.expectEqualStrings("script", sections[0].name);
    try std.testing.expectEqualStrings("tk prime\n", sections[0].body);
    try std.testing.expectEqualStrings("expected/stdout", sections[1].name);
    try std.testing.expectEqualStrings("hello\n", sections[1].body);
}

test "serialize round-trip" {
    const allocator = std.testing.allocator;
    const input =
        \\-- script --
        \\tk prime
        \\-- expected/stdout --
        \\hello
        \\-- expected/exit --
        \\0
        \\
    ;

    const sections1 = try parse(allocator, input);
    defer allocator.free(sections1);
    const serialized = try serialize(allocator, sections1);
    defer allocator.free(serialized);
    try std.testing.expectEqualStrings(input, serialized);

    const sections2 = try parse(allocator, serialized);
    defer allocator.free(sections2);
    try std.testing.expectEqual(sections1.len, sections2.len);
    for (sections1, sections2) |a, b| {
        try std.testing.expectEqualStrings(a.name, b.name);
        try std.testing.expectEqualStrings(a.body, b.body);
    }
}

test "serializer ensures trailing newline on each body" {
    const allocator = std.testing.allocator;
    const sections = [_]Section{
        .{ .name = "a", .body = "no-newline" },
        .{ .name = "b", .body = "ends-with\n" },
    };
    const out = try serialize(allocator, &sections);
    defer allocator.free(out);
    try std.testing.expectEqualStrings("-- a --\nno-newline\n-- b --\nends-with\n", out);

    const reparsed = try parse(allocator, out);
    defer allocator.free(reparsed);
    try std.testing.expectEqual(@as(usize, 2), reparsed.len);
    try std.testing.expectEqualStrings("a", reparsed[0].name);
    try std.testing.expectEqualStrings("b", reparsed[1].name);
}

test "header `-- --` is not a section header" {
    const allocator = std.testing.allocator;
    const input =
        \\-- script --
        \\-- --
        \\tk prime
        \\
    ;
    const sections = try parse(allocator, input);
    defer allocator.free(sections);
    try std.testing.expectEqual(@as(usize, 1), sections.len);
    try std.testing.expectEqualStrings("script", sections[0].name);
}
