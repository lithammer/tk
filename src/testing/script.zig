const std = @import("std");
const cli = @import("../cli.zig");
const proc = @import("../proc/runner.zig");
const clock_mod = @import("../clock.zig");
const txtar = @import("txtar.zig");
const SliceArgIter = @import("arg_iter.zig").SliceArgIter;

const Section = txtar.Section;

pub fn freeTokens(allocator: std.mem.Allocator, tokens: []const []const u8) void {
    for (tokens) |t| allocator.free(t);
    allocator.free(tokens);
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
                    while (i < line.len and (std.ascii.isAlphanumeric(line[i]) or line[i] == '_')) {
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

fn getEnvFlag(name: []const u8) bool {
    const val = std.testing.environ.getPosix(name) orelse return false;
    return std.mem.eql(u8, val, "1");
}

const ScriptResult = struct {
    stdout: std.ArrayList(u8),
    stderr: std.ArrayList(u8),
    /// Exit code of the last `tk` command in the script. Intermediate non-zero
    /// exits are intentionally not surfaced; golden-file fixtures pin the
    /// final command's status, mirroring how a shell pipeline's `$?` works.
    final_exit: u8,

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
        if (!std.mem.startsWith(u8, sec.name, txtar.section_input_prefix)) continue;
        const rel = sec.name[txtar.section_input_prefix.len..];
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
    cwd: std.Io.Dir,
) !ScriptResult {
    const script_body = if (txtar.findSection(sections, txtar.section_script)) |sec| sec.body else "";

    var result = ScriptResult{
        .stdout = .empty,
        .stderr = .empty,
        .final_exit = 0,
    };
    errdefer result.deinit(allocator);

    var real_runner = proc.RealRunner.init(std.testing.io);
    var fake_clock = clock_mod.FakeClock.init(0);

    var line_it = std.mem.splitScalar(u8, script_body, '\n');
    while (line_it.next()) |line| {
        const trimmed = std.mem.trimEnd(u8, line, "\r");
        const argv = try tokenizeLine(allocator, trimmed, env);
        defer freeTokens(allocator, argv);

        if (argv.len == 0 or !std.mem.eql(u8, argv[0], "tk")) continue;

        var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stdout_buf.deinit();
        var stderr_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stderr_buf.deinit();

        const deps = cli.Deps{
            .stdout = &stdout_buf.writer,
            .stderr = &stderr_buf.writer,
            .gpa = allocator,
            .cwd = cwd,
            .runner = real_runner.runner(),
            .clock = fake_clock.clock(),
        };

        var iter = SliceArgIter{ .items = argv[1..] };
        result.final_exit = cli.runArgv(deps, &iter) catch |err| blk: {
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
    const new_exit = try std.fmt.allocPrint(allocator, "{d}\n", .{result.final_exit});
    defer allocator.free(new_exit);

    var new_sections: std.ArrayList(Section) = .empty;
    defer new_sections.deinit(allocator);

    for (sections) |sec| {
        const body = if (std.mem.eql(u8, sec.name, txtar.section_expected_stdout))
            result.stdout.items
        else if (std.mem.eql(u8, sec.name, txtar.section_expected_stderr))
            result.stderr.items
        else if (std.mem.eql(u8, sec.name, txtar.section_expected_exit))
            new_exit
        else
            sec.body;
        try new_sections.append(allocator, .{ .name = sec.name, .body = body });
    }

    return txtar.serialize(allocator, new_sections.items);
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
    quiet: bool,
) !void {
    const expected_stdout = if (txtar.findSection(sections, txtar.section_expected_stdout)) |s| s.body else "";
    const expected_stderr = if (txtar.findSection(sections, txtar.section_expected_stderr)) |s| s.body else "";
    const expected_exit_str = if (txtar.findSection(sections, txtar.section_expected_exit)) |s| s.body else "0\n";

    const expected_exit = try std.fmt.parseInt(u8, std.mem.trimEnd(u8, expected_exit_str, " \t\r\n"), 10);

    var fail = false;

    if (!std.mem.eql(u8, result.stdout.items, expected_stdout)) {
        if (!quiet) printMismatch(allocator, "stdout", expected_stdout, result.stdout.items, work_path);
        fail = true;
    }

    if (!std.mem.eql(u8, result.stderr.items, expected_stderr)) {
        if (!quiet) printMismatch(allocator, "stderr", expected_stderr, result.stderr.items, work_path);
        fail = true;
    }

    if (result.final_exit != expected_exit) {
        if (!quiet) std.debug.print("\n--- exit code mismatch: expected {d}, got {d} ---\n", .{ expected_exit, result.final_exit });
        fail = true;
    }

    if (fail) return error.ScenarioFailed;
}

pub const RunOptions = struct {
    update: bool = false,
    keep_work: bool = false,
    quiet: bool = false,
};

pub fn runScenario(
    allocator: std.mem.Allocator,
    fixture_path: ?[]const u8,
    txtar_bytes: []const u8,
) !void {
    return runScenarioWith(allocator, fixture_path, txtar_bytes, .{
        .update = getEnvFlag("TK_UPDATE"),
        .keep_work = getEnvFlag("TK_TESTWORK"),
        .quiet = false,
    });
}

const Staged = struct {
    sections: []Section,
    work_path: [:0]const u8,
    tmp: std.testing.TmpDir,
    result: ScriptResult,

    fn deinit(self: *Staged, allocator: std.mem.Allocator, keep_work: bool) void {
        self.result.deinit(allocator);
        allocator.free(self.work_path);
        allocator.free(self.sections);
        if (!keep_work) self.tmp.cleanup();
    }
};

fn stage(allocator: std.mem.Allocator, txtar_bytes: []const u8) !Staged {
    const sections = try txtar.parse(allocator, txtar_bytes);
    errdefer allocator.free(sections);

    var tmp = std.testing.tmpDir(.{});
    errdefer tmp.cleanup();

    const work_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    errdefer allocator.free(work_path);

    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", work_path);

    try materializeInputs(tmp.dir, sections);

    const result = try executeScript(allocator, sections, env, tmp.dir);
    return .{ .sections = sections, .work_path = work_path, .tmp = tmp, .result = result };
}

pub fn runScenarioWith(
    allocator: std.mem.Allocator,
    fixture_path: ?[]const u8,
    txtar_bytes: []const u8,
    opts: RunOptions,
) !void {
    var staged = try stage(allocator, txtar_bytes);
    defer staged.deinit(allocator, opts.keep_work);

    if (opts.keep_work) std.debug.print("WORK={s}\n", .{staged.work_path});

    if (opts.update) {
        const rewritten = try rewriteSections(allocator, staged.sections, staged.result);
        defer allocator.free(rewritten);

        const path = fixture_path orelse {
            std.debug.print("\nTK_UPDATE=1 set but no fixture_path provided; rewrite discarded.\n", .{});
            return;
        };
        try std.Io.Dir.cwd().writeFile(std.testing.io, .{ .sub_path = path, .data = rewritten });
        std.debug.print("TK_UPDATE: wrote {s}\n", .{path});
        return;
    }

    try compareAndReport(allocator, staged.sections, staged.result, staged.work_path, opts.quiet);
}

pub fn rewriteScenarioBytes(allocator: std.mem.Allocator, txtar_bytes: []const u8) ![]u8 {
    var staged = try stage(allocator, txtar_bytes);
    defer staged.deinit(allocator, false);
    return rewriteSections(allocator, staged.sections, staged.result);
}

test "tokenizer: basic words" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk prime", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("tk", result[0]);
    try std.testing.expectEqualStrings("prime", result[1]);
}

test "tokenizer: single quotes" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk add -m 'first paragraph'", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 4), result.len);
    try std.testing.expectEqualStrings("first paragraph", result[3]);
}

test "tokenizer: doubled quote escape" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk add -m 'don''t forget'", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 4), result.len);
    try std.testing.expectEqualStrings("don't forget", result[3]);
}

test "tokenizer: comment" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk x #comment", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 2), result.len);
}

test "tokenizer: env expansion" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", "/tmp/test-work");

    const result = try tokenizeLine(allocator, "tk $WORK/file", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("/tmp/test-work/file", result[1]);
}

test "tokenizer: brace form expansion" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();
    try env.put("WORK", "/tmp/test-work");

    const result = try tokenizeLine(allocator, "tk ${WORK}/file", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("/tmp/test-work/file", result[1]);
}

test "tokenizer: undefined variable preserved" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const result = try tokenizeLine(allocator, "tk $NOPE/file", env);
    defer freeTokens(allocator, result);
    try std.testing.expectEqual(@as(usize, 2), result.len);
    try std.testing.expectEqualStrings("$NOPE/file", result[1]);
}

test "tokenizer: empty and comment-only lines return zero tokens" {
    const allocator = std.testing.allocator;
    var env = std.StringHashMap([]const u8).init(allocator);
    defer env.deinit();

    const empty = try tokenizeLine(allocator, "", env);
    defer freeTokens(allocator, empty);
    try std.testing.expectEqual(@as(usize, 0), empty.len);

    const comment = try tokenizeLine(allocator, "# only a comment", env);
    defer freeTokens(allocator, comment);
    try std.testing.expectEqual(@as(usize, 0), comment.len);
}

test "TK_UPDATE preserves section order" {
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

    const sections_before = try txtar.parse(allocator, fixture);
    defer allocator.free(sections_before);

    try std.testing.expectEqualStrings("input/notes.md", sections_before[1].name);

    const rewritten = try rewriteScenarioBytes(allocator, fixture);
    defer allocator.free(rewritten);

    const sections_after = try txtar.parse(allocator, rewritten);
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
    try std.testing.expectError(
        error.ScenarioFailed,
        runScenarioWith(allocator, null, fixture, .{ .quiet = true }),
    );
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

    try std.testing.expectError(
        error.ScenarioFailed,
        runScenarioWith(allocator, null, fixture, .{ .quiet = true }),
    );
}

test "validateInputPath: rejects parent escapes and absolute paths" {
    try std.testing.expectError(error.InvalidInputPath, validateInputPath(""));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("/abs"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("../escape"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("a/../b"));
    try validateInputPath("a/b/c");
    try validateInputPath("file.md");
}
