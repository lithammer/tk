const std = @import("std");
const cli = @import("../cli.zig");
const SliceArgIter = @import("arg_iter.zig").SliceArgIter;

pub const Section = struct {
    name: []const u8,
    body: []const u8,
};

const PRELUDE = "";

pub fn parseTxtar(allocator: std.mem.Allocator, data: []const u8) ![]Section {
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

pub fn serializeTxtar(allocator: std.mem.Allocator, sections: []const Section) ![]u8 {
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

pub fn tokenizeLine(
    allocator: std.mem.Allocator,
    line: []const u8,
    env: std.StringHashMap([]const u8),
) ![][]const u8 {
    var tokens: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (tokens.items) |t| allocator.free(t);
        tokens.deinit(allocator);
    }

    var i: usize = 0;

    while (i < line.len) {
        const c = line[i];

        if (c == ' ' or c == '\t' or c == '\r') {
            i += 1;
            continue;
        }

        if (c == '#') break;

        var token: std.ArrayList(u8) = .empty;
        errdefer token.deinit(allocator);

        while (i < line.len) {
            const ch = line[i];

            if (ch == ' ' or ch == '\t' or ch == '\r' or ch == '#') break;

            if (ch == '\'') {
                i += 1;
                while (i < line.len) {
                    const qc = line[i];
                    if (qc == '\'') {
                        if (i + 1 < line.len and line[i + 1] == '\'') {
                            try token.append(allocator, '\'');
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        try token.append(allocator, qc);
                        i += 1;
                    }
                }
                continue;
            }

            if (ch == '$') {
                i += 1;
                var name_buf: std.ArrayList(u8) = .empty;
                defer name_buf.deinit(allocator);

                if (i < line.len and line[i] == '{') {
                    i += 1;
                    while (i < line.len and line[i] != '}') {
                        try name_buf.append(allocator, line[i]);
                        i += 1;
                    }
                    if (i < line.len) i += 1;
                } else {
                    while (i < line.len and isVarChar(line[i])) {
                        try name_buf.append(allocator, line[i]);
                        i += 1;
                    }
                }

                const name = name_buf.items;
                if (env.get(name)) |val| {
                    try token.appendSlice(allocator, val);
                } else {
                    try token.append(allocator, '$');
                    try token.appendSlice(allocator, name);
                }
                continue;
            }

            try token.append(allocator, ch);
            i += 1;
        }

        try tokens.append(allocator, try token.toOwnedSlice(allocator));
    }

    return tokens.toOwnedSlice(allocator);
}

fn isVarChar(c: u8) bool {
    return (c >= 'a' and c <= 'z') or (c >= 'A' and c <= 'Z') or (c >= '0' and c <= '9') or c == '_';
}

fn getEnvFlag(name: []const u8) bool {
    const val = std.testing.environ.getPosix(name) orelse return false;
    return std.mem.eql(u8, val, "1");
}

const ScriptResult = struct {
    stdout: std.ArrayList(u8),
    stderr: std.ArrayList(u8),
    last_exit: u8,

    fn deinit(self: *ScriptResult, allocator: std.mem.Allocator) void {
        self.stdout.deinit(allocator);
        self.stderr.deinit(allocator);
    }
};

fn validateInputPath(rel: []const u8) !void {
    if (rel.len == 0) return error.InvalidInputPath;
    if (rel[0] == '/') return error.InvalidInputPath;
    var it = std.mem.splitScalar(u8, rel, '/');
    while (it.next()) |segment| {
        if (std.mem.eql(u8, segment, "..")) return error.InvalidInputPath;
    }
}

fn materializeInputs(tmp_dir: std.Io.Dir, sections: []const Section) !void {
    for (sections) |sec| {
        if (!std.mem.startsWith(u8, sec.name, "input/")) continue;
        const rel = sec.name["input/".len..];
        try validateInputPath(rel);
        if (std.fs.path.dirname(rel)) |dir| {
            try tmp_dir.createDirPath(std.testing.io, dir);
        }
        try tmp_dir.writeFile(std.testing.io, .{ .sub_path = rel, .data = sec.body });
    }
}

fn executeScript(
    allocator: std.mem.Allocator,
    sections: []const Section,
    env: std.StringHashMap([]const u8),
) !ScriptResult {
    var script_body: []const u8 = "";
    for (sections) |sec| {
        if (std.mem.eql(u8, sec.name, "script")) {
            script_body = sec.body;
            break;
        }
    }

    var result = ScriptResult{
        .stdout = .empty,
        .stderr = .empty,
        .last_exit = 0,
    };
    errdefer result.deinit(allocator);

    var line_it = std.mem.splitScalar(u8, script_body, '\n');
    while (line_it.next()) |line| {
        const trimmed = std.mem.trimEnd(u8, line, "\r");
        const argv = try tokenizeLine(allocator, trimmed, env);
        defer {
            for (argv) |arg| allocator.free(arg);
            allocator.free(argv);
        }

        if (argv.len == 0 or !std.mem.eql(u8, argv[0], "tk")) continue;

        var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stdout_buf.deinit();
        var stderr_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stderr_buf.deinit();

        const deps = cli.Deps{
            .stdout = &stdout_buf.writer,
            .stderr = &stderr_buf.writer,
            .gpa = allocator,
        };

        var iter = SliceArgIter{ .items = argv[1..] };
        result.last_exit = cli.runArgv(deps, &iter) catch |err| blk: {
            try result.stderr.appendSlice(allocator, "internal error: ");
            try result.stderr.appendSlice(allocator, @errorName(err));
            try result.stderr.append(allocator, '\n');
            break :blk 3;
        };

        try result.stdout.appendSlice(allocator, stdout_buf.written());
        try result.stderr.appendSlice(allocator, stderr_buf.written());
    }

    return result;
}

fn rewriteSections(
    allocator: std.mem.Allocator,
    sections: []const Section,
    result: ScriptResult,
) ![]u8 {
    const new_exit = try std.fmt.allocPrint(allocator, "{d}\n", .{result.last_exit});
    defer allocator.free(new_exit);

    var new_sections: std.ArrayList(Section) = .empty;
    defer new_sections.deinit(allocator);

    for (sections) |sec| {
        if (std.mem.eql(u8, sec.name, "expected/stdout")) {
            try new_sections.append(allocator, .{ .name = sec.name, .body = result.stdout.items });
        } else if (std.mem.eql(u8, sec.name, "expected/stderr")) {
            try new_sections.append(allocator, .{ .name = sec.name, .body = result.stderr.items });
        } else if (std.mem.eql(u8, sec.name, "expected/exit")) {
            try new_sections.append(allocator, .{ .name = sec.name, .body = new_exit });
        } else {
            try new_sections.append(allocator, sec);
        }
    }

    return serializeTxtar(allocator, new_sections.items);
}

fn replaceWork(allocator: std.mem.Allocator, text: []const u8, work_path: []const u8) ![]u8 {
    return std.mem.replaceOwned(u8, allocator, text, work_path, "$WORK");
}

fn printMismatch(
    allocator: std.mem.Allocator,
    label: []const u8,
    expected: []const u8,
    actual: []const u8,
    work_path: []const u8,
) void {
    const disp_actual = replaceWork(allocator, actual, work_path) catch actual;
    defer if (disp_actual.ptr != actual.ptr) allocator.free(disp_actual);
    const disp_expected = replaceWork(allocator, expected, work_path) catch expected;
    defer if (disp_expected.ptr != expected.ptr) allocator.free(disp_expected);
    std.debug.print("\n--- {s} mismatch ---\nexpected:\n{s}\nactual:\n{s}\n", .{ label, disp_expected, disp_actual });
}

fn compareAndReport(
    allocator: std.mem.Allocator,
    sections: []const Section,
    result: ScriptResult,
    work_path: []const u8,
) !void {
    var expected_stdout: []const u8 = "";
    var expected_stderr: []const u8 = "";
    var expected_exit_str: []const u8 = "0\n";

    for (sections) |sec| {
        if (std.mem.eql(u8, sec.name, "expected/stdout")) {
            expected_stdout = sec.body;
        } else if (std.mem.eql(u8, sec.name, "expected/stderr")) {
            expected_stderr = sec.body;
        } else if (std.mem.eql(u8, sec.name, "expected/exit")) {
            expected_exit_str = sec.body;
        }
    }

    const expected_exit = try std.fmt.parseInt(u8, std.mem.trimEnd(u8, expected_exit_str, " \t\r\n"), 10);

    var fail = false;

    if (!std.mem.eql(u8, result.stdout.items, expected_stdout)) {
        printMismatch(allocator, "stdout", expected_stdout, result.stdout.items, work_path);
        fail = true;
    }

    if (!std.mem.eql(u8, result.stderr.items, expected_stderr)) {
        printMismatch(allocator, "stderr", expected_stderr, result.stderr.items, work_path);
        fail = true;
    }

    if (result.last_exit != expected_exit) {
        std.debug.print("\n--- exit code mismatch: expected {d}, got {d} ---\n", .{ expected_exit, result.last_exit });
        fail = true;
    }

    if (fail) return error.ScenarioFailed;
}

pub fn runScenario(
    allocator: std.mem.Allocator,
    fixture_path: ?[]const u8,
    txtar_bytes: []const u8,
) !void {
    const updating = getEnvFlag("TK_UPDATE");
    const keep_work = getEnvFlag("TK_TESTWORK");

    const sections = try parseTxtar(allocator, txtar_bytes);
    defer allocator.free(sections);

    var tmp = std.testing.tmpDir(.{});
    defer if (!keep_work) tmp.cleanup();

    const work_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    defer allocator.free(work_path);

    if (keep_work) {
        std.debug.print("WORK={s}\n", .{work_path});
    }

    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", work_path);

    try materializeInputs(tmp.dir, sections);

    var result = try executeScript(allocator, sections, env);
    defer result.deinit(allocator);

    if (updating) {
        const rewritten = try rewriteSections(allocator, sections, result);
        defer allocator.free(rewritten);

        const path = fixture_path orelse {
            std.debug.print("\nTK_UPDATE=1 set but no fixture_path provided; rewrite discarded.\n", .{});
            return;
        };
        try std.Io.Dir.cwd().writeFile(std.testing.io, .{ .sub_path = path, .data = rewritten });
        std.debug.print("TK_UPDATE: wrote {s}\n", .{path});
        return;
    }

    try compareAndReport(allocator, sections, result, work_path);
}

pub fn rewriteScenarioBytes(allocator: std.mem.Allocator, txtar_bytes: []const u8) ![]u8 {
    const sections = try parseTxtar(allocator, txtar_bytes);
    defer allocator.free(sections);

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const work_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    defer allocator.free(work_path);

    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", work_path);

    try materializeInputs(tmp.dir, sections);

    var result = try executeScript(allocator, sections, env);
    defer result.deinit(allocator);

    return rewriteSections(allocator, sections, result);
}

test "tokenizer: basic words" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk prime", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("tk", result[0]);
    try std.testing.expectEqualStrings("prime", result[1]);
}

test "tokenizer: single quotes" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk add -m 'first paragraph'", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 4), result.len);
    try std.testing.expectEqualStrings("first paragraph", result[3]);
}

test "tokenizer: doubled quote escape" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk add -m 'don''t forget'", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 4), result.len);
    try std.testing.expectEqualStrings("don't forget", result[3]);
}

test "tokenizer: comment" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk x #comment", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 2), result.len);
}

test "tokenizer: env expansion" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", "/tmp/test-work");

    const result = try tokenizeLine(allocator, "tk $WORK/file", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("/tmp/test-work/file", result[1]);
}

test "tokenizer: brace form expansion" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", "/tmp/test-work");

    const result = try tokenizeLine(allocator, "tk ${WORK}/file", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("/tmp/test-work/file", result[1]);
}

test "tokenizer: undefined variable preserved" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk $NOPE/file", env);
    defer {
        for (result) |t| allocator.free(t);
        allocator.free(result);
    }
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("$NOPE/file", result[1]);
}

test "tokenizer: empty and comment-only lines return zero tokens" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const empty = try tokenizeLine(allocator, "", env);
    defer {
        for (empty) |t| allocator.free(t);
        allocator.free(empty);
    }
    try std.testing.expectEqual(@as(usize, 0), empty.len);

    const comment = try tokenizeLine(allocator, "# only a comment", env);
    defer {
        for (comment) |t| allocator.free(t);
        allocator.free(comment);
    }
    try std.testing.expectEqual(@as(usize, 0), comment.len);
}

test "txtar: parse two sections" {
    const allocator = std.testing.allocator;
    const input =
        \\-- script --
        \\tk prime
        \\-- expected/stdout --
        \\hello
        \\
    ;

    const sections = try parseTxtar(allocator, input);
    defer allocator.free(sections);

    try std.testing.expectEqual(@as(usize, 2), sections.len);
    try std.testing.expectEqualStrings("script", sections[0].name);
    try std.testing.expectEqualStrings("tk prime\n", sections[0].body);
    try std.testing.expectEqualStrings("expected/stdout", sections[1].name);
    try std.testing.expectEqualStrings("hello\n", sections[1].body);
}

test "txtar: serialize round-trip" {
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

    const sections1 = try parseTxtar(allocator, input);
    defer allocator.free(sections1);
    const serialized = try serializeTxtar(allocator, sections1);
    defer allocator.free(serialized);
    try std.testing.expectEqualStrings(input, serialized);

    const sections2 = try parseTxtar(allocator, serialized);
    defer allocator.free(sections2);
    try std.testing.expectEqual(sections1.len, sections2.len);
    for (sections1, sections2) |a, b| {
        try std.testing.expectEqualStrings(a.name, b.name);
        try std.testing.expectEqualStrings(a.body, b.body);
    }
}

test "txtar: serializer ensures trailing newline on each body" {
    const allocator = std.testing.allocator;
    const sections = [_]Section{
        .{ .name = "a", .body = "no-newline" },
        .{ .name = "b", .body = "ends-with\n" },
    };
    const out = try serializeTxtar(allocator, &sections);
    defer allocator.free(out);
    try std.testing.expectEqualStrings("-- a --\nno-newline\n-- b --\nends-with\n", out);

    const reparsed = try parseTxtar(allocator, out);
    defer allocator.free(reparsed);
    try std.testing.expectEqual(@as(usize, 2), reparsed.len);
    try std.testing.expectEqualStrings("a", reparsed[0].name);
    try std.testing.expectEqualStrings("b", reparsed[1].name);
}

test "txtar: header `-- --` is not a section header" {
    const allocator = std.testing.allocator;
    const input =
        \\-- script --
        \\-- --
        \\tk prime
        \\
    ;
    const sections = try parseTxtar(allocator, input);
    defer allocator.free(sections);
    try std.testing.expectEqual(@as(usize, 1), sections.len);
    try std.testing.expectEqualStrings("script", sections[0].name);
}

test "txtar: TK_UPDATE preserves section order" {
    const allocator = std.testing.allocator;

    const prime_body = std.mem.trimEnd(u8, @embedFile("prime_md"), " \t\r\n");
    const expected_stdout = try std.fmt.allocPrint(allocator, "{s}\n", .{prime_body});
    defer allocator.free(expected_stdout);

    const fixture = try std.fmt.allocPrint(allocator,
        \\-- script --
        \\tk prime
        \\
        \\-- input/notes.md --
        \\This file must survive a TK_UPDATE rewrite unchanged
        \\and in its original section position.
        \\
        \\-- expected/stdout --
        \\WRONG OUTPUT
        \\-- expected/stderr --
        \\
        \\-- expected/exit --
        \\0
        \\
    , .{});
    defer allocator.free(fixture);

    const sections_before = try parseTxtar(allocator, fixture);
    defer allocator.free(sections_before);

    try std.testing.expectEqualStrings("input/notes.md", sections_before[1].name);

    const rewritten = try rewriteScenarioBytes(allocator, fixture);
    defer allocator.free(rewritten);

    const sections_after = try parseTxtar(allocator, rewritten);
    defer allocator.free(sections_after);

    try std.testing.expectEqual(sections_before.len, sections_after.len);
    try std.testing.expectEqualStrings("input/notes.md", sections_after[1].name);
    try std.testing.expectEqualStrings(sections_before[1].body, sections_after[1].body);

    var found_stdout = false;
    for (sections_after) |sec| {
        if (std.mem.eql(u8, sec.name, "expected/stdout")) {
            try std.testing.expectEqualStrings(expected_stdout, sec.body);
            found_stdout = true;
        }
    }
    try std.testing.expect(found_stdout);
}

test "runScenario: detects stdout mismatch" {
    const allocator = std.testing.allocator;
    const fixture =
        \\-- script --
        \\tk prime
        \\-- expected/stdout --
        \\WRONG
        \\-- expected/stderr --
        \\
        \\-- expected/exit --
        \\0
        \\
    ;
    try std.testing.expectError(error.ScenarioFailed, runScenario(allocator, null, fixture));
}

test "runScenario: detects exit-code mismatch" {
    const allocator = std.testing.allocator;
    const prime_body = std.mem.trimEnd(u8, @embedFile("prime_md"), " \t\r\n");
    const fixture = try std.fmt.allocPrint(allocator,
        \\-- script --
        \\tk prime
        \\-- expected/stdout --
        \\{s}
        \\-- expected/stderr --
        \\
        \\-- expected/exit --
        \\1
        \\
    , .{prime_body});
    defer allocator.free(fixture);

    try std.testing.expectError(error.ScenarioFailed, runScenario(allocator, null, fixture));
}

test "validateInputPath: rejects parent escapes and absolute paths" {
    try std.testing.expectError(error.InvalidInputPath, validateInputPath(""));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("/abs"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("../escape"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("a/../b"));
    try validateInputPath("a/b/c");
    try validateInputPath("file.md");
}
