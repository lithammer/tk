//! Git-commit-style message parsing shared by write commands.

const std = @import("std");

/// Parsed Ticket message. Title and body are allocator-owned.
pub const ParsedMessage = struct {
    title: []u8,
    body: []u8,

    /// Free title/body buffers returned by `parse`.
    pub fn deinit(self: ParsedMessage, gpa: std.mem.Allocator) void {
        gpa.free(self.title);
        gpa.free(self.body);
    }
};

pub const ParseError = error{
    EmptyMessage,
    NulByte,
    OutOfMemory,
};

const Line = struct {
    start: usize,
    end: usize,
};

/// Parse raw message bytes into a normalized title/body pair.
///
/// The first paragraph becomes the title. Later paragraphs become the body.
/// Line endings are normalized to LF. Title lines are trimmed and folded with
/// single spaces; body text is otherwise preserved after trimming outer blank
/// lines.
pub fn parse(gpa: std.mem.Allocator, raw: []const u8) ParseError!ParsedMessage {
    if (std.mem.indexOfScalar(u8, raw, 0) != null) return error.NulByte;

    const normalized = try normalizeLineEndings(gpa, raw);
    defer gpa.free(normalized);

    var lines: std.ArrayList(Line) = .empty;
    defer lines.deinit(gpa);
    try collectLines(gpa, normalized, &lines);

    const first = firstNonBlank(normalized, lines.items) orelse return error.EmptyMessage;
    const last = lastNonBlank(normalized, lines.items) orelse unreachable;

    var title_lines_end = first;
    while (title_lines_end <= last and !isBlankLine(normalized[lines.items[title_lines_end].start..lines.items[title_lines_end].end])) {
        title_lines_end += 1;
    }

    var title: std.ArrayList(u8) = .empty;
    errdefer title.deinit(gpa);
    for (lines.items[first..title_lines_end], 0..) |line, i| {
        const trimmed = std.mem.trim(u8, normalized[line.start..line.end], " \t");
        if (i > 0) try title.append(gpa, ' ');
        try title.appendSlice(gpa, trimmed);
    }
    if (title.items.len == 0) return error.EmptyMessage;

    var body_start_line = title_lines_end;
    while (body_start_line <= last and isBlankLine(normalized[lines.items[body_start_line].start..lines.items[body_start_line].end])) {
        body_start_line += 1;
    }

    const body = if (body_start_line <= last)
        try gpa.dupe(u8, normalized[lines.items[body_start_line].start..lines.items[last].end])
    else
        try gpa.dupe(u8, "");

    return .{
        .title = try title.toOwnedSlice(gpa),
        .body = body,
    };
}

fn normalizeLineEndings(gpa: std.mem.Allocator, raw: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);

    var i: usize = 0;
    while (i < raw.len) {
        if (raw[i] == '\r') {
            try out.append(gpa, '\n');
            i += 1;
            if (i < raw.len and raw[i] == '\n') i += 1;
            continue;
        }
        try out.append(gpa, raw[i]);
        i += 1;
    }

    return out.toOwnedSlice(gpa);
}

fn collectLines(gpa: std.mem.Allocator, input: []const u8, lines: *std.ArrayList(Line)) !void {
    var start: usize = 0;
    var i: usize = 0;
    while (i < input.len) : (i += 1) {
        if (input[i] == '\n') {
            try lines.append(gpa, .{ .start = start, .end = i });
            start = i + 1;
        }
    }
    if (start < input.len) {
        try lines.append(gpa, .{ .start = start, .end = input.len });
    }
}

fn isBlankLine(line: []const u8) bool {
    return std.mem.trim(u8, line, " \t").len == 0;
}

fn firstNonBlank(input: []const u8, lines: []const Line) ?usize {
    for (lines, 0..) |line, i| {
        if (!isBlankLine(input[line.start..line.end])) return i;
    }
    return null;
}

fn lastNonBlank(input: []const u8, lines: []const Line) ?usize {
    var i = lines.len;
    while (i > 0) {
        i -= 1;
        const line = lines[i];
        if (!isBlankLine(input[line.start..line.end])) return i;
    }
    return null;
}

test "message: folds title lines and preserves normalized body text" {
    const gpa = std.testing.allocator;

    const parsed = try parse(gpa, "\n" ++
        "  First title line  \n" ++
        "Second title line\n" ++
        "\n" ++
        "Body line one  \n" ++
        "\n" ++
        "\n" ++
        "Body line two\t\n" ++
        "\n");
    defer parsed.deinit(gpa);

    try std.testing.expectEqualStrings("First title line Second title line", parsed.title);
    try std.testing.expectEqualStrings("Body line one  \n\n\nBody line two\t", parsed.body);
}

test "message: normalizes CRLF and CR line endings" {
    const gpa = std.testing.allocator;

    const parsed = try parse(gpa, "Title\r\n\rBody\r\n");
    defer parsed.deinit(gpa);

    try std.testing.expectEqualStrings("Title", parsed.title);
    try std.testing.expectEqualStrings("Body", parsed.body);
}

test "message: rejects empty and NUL-containing input" {
    const gpa = std.testing.allocator;

    try std.testing.expectError(error.EmptyMessage, parse(gpa, " \n\t\r\n"));
    try std.testing.expectError(error.NulByte, parse(gpa, "Title\x00Body"));
}
