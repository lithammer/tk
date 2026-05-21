//! Git-commit-style message parsing and loading shared by write commands.

const std = @import("std");
const Allocator = std.mem.Allocator;

const cli = @import("../cli.zig");

/// Parsed Ticket message. Title and body are allocator-owned.
pub const ParsedMessage = struct {
    title: []u8,
    body: []u8,

    /// Free title/body buffers returned by `parse`.
    pub fn deinit(self: ParsedMessage, gpa: Allocator) void {
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
pub fn parse(gpa: Allocator, raw: []const u8) ParseError!ParsedMessage {
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

fn normalizeLineEndings(gpa: Allocator, raw: []const u8) ![]u8 {
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

fn collectLines(gpa: Allocator, input: []const u8, lines: *std.ArrayList(Line)) !void {
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

/// Parse a message from a pre-split slice of paragraph strings.
///
/// Joins the paragraphs with double newlines and delegates to `parse`. Used by
/// write commands when message paragraphs come from repeated `-m` flags rather
/// than a single file or stdin blob.
pub fn parseFromParagraphs(gpa: Allocator, paragraphs: []const []const u8) ParseError!ParsedMessage {
    if (paragraphs.len == 0) return error.EmptyMessage;
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(gpa);
    for (paragraphs, 0..) |p, i| {
        if (i > 0) try buf.appendSlice(gpa, "\n\n");
        try buf.appendSlice(gpa, p);
    }
    return parse(gpa, buf.items);
}

/// Message source selected by a command after it has validated flag conflicts.
pub const Input = union(enum) {
    paragraphs: []const []const u8,
    file: []const u8,
};

/// Command-specific diagnostics for message input loading and parsing.
pub const InputMessages = struct {
    empty_message: []const u8,
    nul_message: []const u8,
    file_read_prefix: []const u8,
    stdin_read_prefix: []const u8,
};

/// Result of loading and parsing command message input.
pub const InputResult = union(enum) {
    parsed: ParsedMessage,
    user_error,
};

/// Load and parse a command message source, rendering command diagnostics.
pub fn readInput(deps: cli.Deps, input: Input, msgs: InputMessages) !InputResult {
    switch (input) {
        .paragraphs => |paragraphs| {
            const parsed = parseFromParagraphs(deps.gpa, paragraphs) catch |err| {
                try renderInputParseError(deps.stderr, msgs, err);
                return .user_error;
            };
            return .{ .parsed = parsed };
        },
        .file => |path| {
            const raw = if (std.mem.eql(u8, path, "-"))
                deps.stdin.allocRemaining(deps.gpa, .unlimited) catch |err| {
                    deps.stderr.print("{s}{s}\n", .{ msgs.stdin_read_prefix, @errorName(err) }) catch {};
                    return .user_error;
                }
            else
                deps.cwd.readFileAlloc(deps.io, path, deps.gpa, .unlimited) catch |err| {
                    deps.stderr.print("{s}{s}: {s}\n", .{ msgs.file_read_prefix, path, @errorName(err) }) catch {};
                    return .user_error;
                };
            defer deps.gpa.free(raw);

            const parsed = parse(deps.gpa, raw) catch |err| {
                try renderInputParseError(deps.stderr, msgs, err);
                return .user_error;
            };
            return .{ .parsed = parsed };
        },
    }
}

fn renderInputParseError(stderr: *std.Io.Writer, msgs: InputMessages, err: ParseError) !void {
    switch (err) {
        error.EmptyMessage => stderr.print("{s}\n", .{msgs.empty_message}) catch {},
        error.NulByte => stderr.print("{s}\n", .{msgs.nul_message}) catch {},
        error.OutOfMemory => return error.OutOfMemory,
    }
}

test "message: rejects empty and NUL-containing input" {
    const gpa = std.testing.allocator;

    try std.testing.expectError(error.EmptyMessage, parse(gpa, " \n\t\r\n"));
    try std.testing.expectError(error.NulByte, parse(gpa, "Title\x00Body"));
}

test "message: parseFromParagraphs joins paragraphs and parses normally" {
    const gpa = std.testing.allocator;

    const paras = [_][]const u8{ "Update title", "Body paragraph one", "Body paragraph two" };
    const parsed = try parseFromParagraphs(gpa, &paras);
    defer parsed.deinit(gpa);

    try std.testing.expectEqualStrings("Update title", parsed.title);
    try std.testing.expectEqualStrings("Body paragraph one\n\nBody paragraph two", parsed.body);
}

test "message: parseFromParagraphs with single paragraph yields empty body" {
    const gpa = std.testing.allocator;

    const paras = [_][]const u8{"Just a title"};
    const parsed = try parseFromParagraphs(gpa, &paras);
    defer parsed.deinit(gpa);

    try std.testing.expectEqualStrings("Just a title", parsed.title);
    try std.testing.expectEqualStrings("", parsed.body);
}

test "message: parseFromParagraphs with no paragraphs returns EmptyMessage" {
    const gpa = std.testing.allocator;

    try std.testing.expectError(error.EmptyMessage, parseFromParagraphs(gpa, &.{}));
}
