const std = @import("std");
const Allocator = std.mem.Allocator;

const build_options = @import("build_options");
const cli = @import("../cli.zig");
const platform = @import("../platform.zig");
const fake_http = @import("../http/fake.zig");
const proc = @import("../proc/runner.zig");
const clock_mod = @import("../clock.zig");
const txtar = @import("txtar.zig");
const SliceArgIter = @import("arg_iter.zig").SliceArgIter;

const Section = txtar.Section;

/// Free a token slice returned by `tokenizeLine`.
pub fn freeTokens(allocator: Allocator, tokens: []const []const u8) void {
    for (tokens) |t| allocator.free(t);
    allocator.free(tokens);
}

/// Tokenize one `-- script --` line using the repo's testscript subset.
///
/// Whitespace separates args, single quotes preserve literal chunks, doubled
/// quotes inside single quotes produce a literal quote, `#` starts a comment,
/// and `$NAME` / `${NAME}` expand from `env`. Undefined variables are preserved
/// with their leading `$`.
pub fn tokenizeLine(
    allocator: Allocator,
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

fn getEnvFlag(comptime name: []const u8) bool {
    // Zig 0.16.0's `Environ.getPosix` does not compile on Windows because
    // `Environ.Block` resolves to `GlobalBlock`, which has no `view` method.
    // Mirror the OS dispatch used by `Environ.containsUnemptyConstant`.
    if (platform.is_windows) {
        const name_w = comptime std.unicode.wtf8ToWtf16LeStringLiteral(name);
        const one_w = comptime std.unicode.wtf8ToWtf16LeStringLiteral("1");
        const val = std.testing.environ.getWindows(name_w) orelse return false;
        return std.mem.eql(u16, val, one_w);
    }
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

    fn deinit(self: *ScriptResult, allocator: Allocator) void {
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

fn appendScriptError(
    allocator: Allocator,
    result: *ScriptResult,
    comptime fmt: []const u8,
    args: anytype,
) !void {
    const msg = try std.fmt.allocPrint(allocator, fmt ++ "\n", args);
    defer allocator.free(msg);
    try result.stderr.appendSlice(allocator, msg);
    result.final_exit = 3;
}

fn loadStdinSource(
    allocator: Allocator,
    cwd: std.Io.Dir,
    source: []const u8,
    result: *ScriptResult,
) !?[]u8 {
    if (std.mem.eql(u8, source, "stdout")) {
        return try allocator.dupe(u8, result.stdout.items);
    }
    if (std.mem.eql(u8, source, "stderr")) {
        return try allocator.dupe(u8, result.stderr.items);
    }

    validateInputPath(source) catch {
        try appendScriptError(allocator, result, "script: invalid stdin source: {s}", .{source});
        return null;
    };

    return cwd.readFileAlloc(std.testing.io, source, allocator, .unlimited) catch |err| switch (err) {
        error.FileNotFound => {
            try appendScriptError(allocator, result, "script: stdin source not found: {s}", .{source});
            return null;
        },
        else => {
            try appendScriptError(allocator, result, "script: stdin source read failed: {s}: {s}", .{ source, @errorName(err) });
            return null;
        },
    };
}

fn executeScript(
    allocator: Allocator,
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

    // TODO(tk-6): decide the script runner's fakeability stance —
    // mixing a real subprocess runner with a fake clock will bite the moment
    // a tk init / tk worktree scenario lands.
    var real_runner = proc.RealRunner.init(std.testing.io);
    // Scenarios don't call HTTP today; an unmatched URL panics, mirroring
    // the strict `fake_runner` stance. Scenarios that need to exercise
    // `tk self-update` will register URL expectations before the call.
    var fake_http_client = fake_http.FakeHttpClient.init(allocator);
    defer fake_http_client.deinit();
    var fake_clock = clock_mod.FakeClock.init(0);
    var prng = std.Random.DefaultPrng.init(0);
    var pending_stdin: ?[]u8 = null;
    defer if (pending_stdin) |bytes| allocator.free(bytes);
    var active_cwd = cwd;
    var active_cwd_owned = false;
    defer if (active_cwd_owned) active_cwd.close(std.testing.io);

    var line_it = std.mem.splitScalar(u8, script_body, '\n');
    while (line_it.next()) |line| {
        const trimmed = std.mem.trimEnd(u8, line, "\r");
        const argv = try tokenizeLine(allocator, trimmed, env);
        defer freeTokens(allocator, argv);

        if (argv.len == 0) continue;

        if (std.mem.eql(u8, argv[0], "mkdir")) {
            if (argv.len != 2) {
                try appendScriptError(allocator, &result, "script: mkdir requires exactly one path", .{});
                return result;
            }
            validateInputPath(argv[1]) catch {
                try appendScriptError(allocator, &result, "script: invalid mkdir path: {s}", .{argv[1]});
                return result;
            };
            active_cwd.createDirPath(std.testing.io, argv[1]) catch |err| {
                try appendScriptError(allocator, &result, "script: mkdir failed: {s}: {s}", .{ argv[1], @errorName(err) });
                return result;
            };
            continue;
        }

        if (std.mem.eql(u8, argv[0], "cd")) {
            if (argv.len != 2) {
                try appendScriptError(allocator, &result, "script: cd requires exactly one path", .{});
                return result;
            }
            validateInputPath(argv[1]) catch {
                try appendScriptError(allocator, &result, "script: invalid cd path: {s}", .{argv[1]});
                return result;
            };
            const next_cwd = active_cwd.openDir(std.testing.io, argv[1], .{}) catch |err| {
                try appendScriptError(allocator, &result, "script: cd failed: {s}: {s}", .{ argv[1], @errorName(err) });
                return result;
            };
            if (active_cwd_owned) active_cwd.close(std.testing.io);
            active_cwd = next_cwd;
            active_cwd_owned = true;
            continue;
        }

        if (std.mem.eql(u8, argv[0], "stdin")) {
            if (pending_stdin != null) {
                try appendScriptError(allocator, &result, "script: stdin already set", .{});
                return result;
            }
            if (argv.len != 2) {
                try appendScriptError(allocator, &result, "script: stdin requires exactly one source", .{});
                return result;
            }
            pending_stdin = (try loadStdinSource(allocator, active_cwd, argv[1], &result)) orelse return result;
            continue;
        }

        if (std.mem.eql(u8, argv[0], "git")) {
            var run_result = real_runner.runner().run(allocator, .{ .argv = argv, .cwd = active_cwd }) catch |err| {
                try appendScriptError(allocator, &result, "script: git failed to run: {s}", .{@errorName(err)});
                return result;
            };
            defer run_result.deinit(allocator);
            result.final_exit = run_result.exit_code;
            try result.stdout.appendSlice(allocator, run_result.stdout);
            try result.stderr.appendSlice(allocator, run_result.stderr);
            continue;
        }

        if (!std.mem.eql(u8, argv[0], "tk")) continue;

        var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stdout_buf.deinit();
        var stderr_buf: std.Io.Writer.Allocating = .init(allocator);
        defer stderr_buf.deinit();
        const stdin_bytes = pending_stdin;
        pending_stdin = null;
        defer if (stdin_bytes) |bytes| allocator.free(bytes);
        var stdin_reader: std.Io.Reader = .fixed(stdin_bytes orelse "");

        const deps = cli.Deps{
            .stdout = &stdout_buf.writer,
            .stderr = &stderr_buf.writer,
            .stdin = &stdin_reader,
            .gpa = allocator,
            .io = std.testing.io,
            .cwd = active_cwd,
            .runner = real_runner.runner(),
            .http = fake_http_client.http(),
            .clock = fake_clock.clock(),
            .random = prng.random(),
            .styler = .{ .stdout = .no_color, .stderr = .no_color },
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

    if (pending_stdin != null) {
        try appendScriptError(allocator, &result, "script: stdin set but no tk command consumed it", .{});
        return result;
    }

    return result;
}

fn rewriteSections(
    allocator: Allocator,
    sections: []const Section,
    result: ScriptResult,
    work_path: []const u8,
) ![]u8 {
    const new_exit = try std.fmt.allocPrint(allocator, "{d}\n", .{result.final_exit});
    defer allocator.free(new_exit);

    // Route the captured streams through the same normaliser the compare
    // path uses so `TK_UPDATE=1` produces fixtures that match the convention
    // checked at read time: the absolute work directory is substituted with
    // `$WORK` and Windows native `\` separators inside `$WORK\…` spans are
    // rewritten to `/`. Without this, a Windows developer running
    // `TK_UPDATE=1` would commit a fixture full of host-specific absolute
    // paths that no other runner could match.
    const stdout_norm = try normalizeWork(allocator, result.stdout.items, work_path);
    defer stdout_norm.deinit(allocator);
    const stderr_norm = try normalizeWork(allocator, result.stderr.items, work_path);
    defer stderr_norm.deinit(allocator);

    var new_sections: std.ArrayList(Section) = .empty;
    defer new_sections.deinit(allocator);

    for (sections) |sec| {
        const body = if (std.mem.eql(u8, sec.name, txtar.section_expected_stdout))
            stdout_norm.text
        else if (std.mem.eql(u8, sec.name, txtar.section_expected_stderr))
            stderr_norm.text
        else if (std.mem.eql(u8, sec.name, txtar.section_expected_exit))
            new_exit
        else
            sec.body;
        try new_sections.append(allocator, .{ .name = sec.name, .body = body });
    }

    return txtar.serialize(allocator, new_sections.items);
}

const NormalizedText = struct {
    text: []const u8,
    owned: bool,

    fn deinit(self: NormalizedText, allocator: Allocator) void {
        if (self.owned) allocator.free(self.text);
    }
};

fn normalizeWork(allocator: Allocator, text: []const u8, work_path: []const u8) !NormalizedText {
    // `std.mem.indexOf` treats an empty needle as matched at index 0, so an
    // empty `work_path` would slip past the guard below and into
    // `replaceOwned`, which asserts on a zero-length needle. Real callers
    // always pass an absolute temp directory, but a defensive early return
    // makes the contract explicit and survives a refactored caller.
    if (work_path.len == 0) return .{ .text = text, .owned = false };
    if (std.mem.indexOf(u8, text, work_path) == null) {
        return .{ .text = text, .owned = false };
    }
    const owned = try std.mem.replaceOwned(u8, allocator, text, work_path, "$WORK");
    // On Windows the substituted path still uses native `\` separators
    // (Git's `/` output is normalized to native at the discovery boundary,
    // and `std.fs.path.join` already emits native). txtar fixtures use `/`
    // as the canonical work-relative separator, so bridge the convention
    // gap here -- but only inside `$WORK`-prefixed spans, to avoid
    // mangling legitimate backslashes elsewhere in the stream (e.g. troff
    // escapes printed by `tk manpage`).
    if (platform.is_windows) normalizeWorkSpans(owned);
    return .{ .text = owned, .owned = true };
}

/// Rewrite `\` to `/` inside each `$WORK`-prefixed path token. Mutates the
/// buffer in place.
///
/// Two safeguards keep this surgical:
///
/// 1. The byte immediately after `$WORK` must be a path separator or a
///    terminator (whitespace / end of buffer). This avoids matching
///    identifiers that merely share the prefix (e.g. `$WORKSPACE`,
///    `$WORKDIR`, or a `$WORK_other` left over when one work-rooted path
///    is a strict prefix of another).
/// 2. The walk over each span stops at the first non-path byte rather
///    than at the first whitespace. Quotes, commas, semicolons, and the
///    like terminate the span so that backslashes appearing later on the
///    same line (e.g. inside a quoted literal printed alongside the
///    path) are not collateral-damaged.
fn normalizeWorkSpans(buf: []u8) void {
    const marker = "$WORK";
    var i: usize = 0;
    while (std.mem.indexOfPos(u8, buf, i, marker)) |hit| {
        const after = hit + marker.len;
        if (!isWorkBoundary(buf, after)) {
            i = after;
            continue;
        }
        var j = after;
        while (j < buf.len and isPathByte(buf[j])) : (j += 1) {
            if (buf[j] == '\\') buf[j] = '/';
        }
        i = j;
    }
}

/// `$WORK` is a complete token when followed by a path separator or by
/// any non-path terminator (whitespace, punctuation, end of buffer).
fn isWorkBoundary(buf: []const u8, after: usize) bool {
    if (after == buf.len) return true;
    const c = buf[after];
    return c == '/' or c == '\\' or !isPathByte(c);
}

/// Bytes that may appear inside a filesystem path the test runner cares
/// about: ASCII alphanumerics plus the few punctuation characters that
/// surface in tk's emitted paths (drive prefixes, separators, dotted file
/// names, dash/underscore-bearing display IDs, tilde-prefixed home
/// references).
fn isPathByte(c: u8) bool {
    return switch (c) {
        'a'...'z', 'A'...'Z', '0'...'9', '/', '\\', '.', '_', '-', ':', '~' => true,
        else => false,
    };
}

fn printMismatch(label: []const u8, expected: []const u8, actual: []const u8) void {
    std.debug.print("\n--- {s} mismatch ---\nexpected:\n{s}\nactual:\n{s}\n", .{ label, expected, actual });
}

fn compareAndReport(
    allocator: Allocator,
    sections: []const Section,
    result: ScriptResult,
    work_path: []const u8,
    quiet: bool,
) !void {
    const expected_stdout = if (txtar.findSection(sections, txtar.section_expected_stdout)) |s| s.body else "";
    const expected_stderr = if (txtar.findSection(sections, txtar.section_expected_stderr)) |s| s.body else "";
    const expected_exit_str = if (txtar.findSection(sections, txtar.section_expected_exit)) |s| s.body else "0\n";
    const actual_stdout = try normalizeWork(allocator, result.stdout.items, work_path);
    defer actual_stdout.deinit(allocator);
    const actual_stderr = try normalizeWork(allocator, result.stderr.items, work_path);
    defer actual_stderr.deinit(allocator);

    const expected_exit = try std.fmt.parseInt(u8, std.mem.trimEnd(u8, expected_exit_str, " \t\r\n"), 10);

    var fail = false;

    if (!std.mem.eql(u8, actual_stdout.text, expected_stdout)) {
        if (!quiet) printMismatch("stdout", expected_stdout, actual_stdout.text);
        fail = true;
    }

    if (!std.mem.eql(u8, actual_stderr.text, expected_stderr)) {
        if (!quiet) printMismatch("stderr", expected_stderr, actual_stderr.text);
        fail = true;
    }

    if (result.final_exit != expected_exit) {
        if (!quiet) std.debug.print("\n--- exit code mismatch: expected {d}, got {d} ---\n", .{ expected_exit, result.final_exit });
        fail = true;
    }

    if (fail) return error.ScenarioFailed;
}

/// Scenario execution knobs used by tests and snapshot-update mode.
pub const RunOptions = struct {
    /// Rewrite expected sections from actual output.
    update: bool = false,
    /// Preserve the temporary `$WORK` directory for debugging.
    keep_work: bool = false,
    /// Suppress mismatch printing, useful for tests expecting failure.
    quiet: bool = false,
};

/// Run a txtar scenario using `TK_UPDATE` and `TK_TESTWORK` environment flags.
pub fn runScenario(
    allocator: Allocator,
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

    fn deinit(self: *Staged, allocator: Allocator, keep_work: bool) void {
        self.result.deinit(allocator);
        allocator.free(self.work_path);
        allocator.free(self.sections);
        if (!keep_work) self.tmp.cleanup();
    }
};

fn stage(allocator: Allocator, txtar_bytes: []const u8) !Staged {
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

/// Run a txtar scenario with explicit options.
///
/// Scenarios execute in-process against `cli.runArgv`, aggregate stdout/stderr
/// from every `tk` command, and compare the final command's exit code.
pub fn runScenarioWith(
    allocator: Allocator,
    fixture_path: ?[]const u8,
    txtar_bytes: []const u8,
    opts: RunOptions,
) !void {
    var staged = try stage(allocator, txtar_bytes);
    defer staged.deinit(allocator, opts.keep_work);

    if (opts.keep_work) std.debug.print("WORK={s}\n", .{staged.work_path});

    if (opts.update) {
        const rewritten = try rewriteSections(allocator, staged.sections, staged.result, staged.work_path);
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

/// Return a rewritten txtar fixture with expected sections updated from an
/// in-process scenario run.
pub fn rewriteScenarioBytes(allocator: Allocator, txtar_bytes: []const u8) ![]u8 {
    var staged = try stage(allocator, txtar_bytes);
    defer staged.deinit(allocator, false);
    return rewriteSections(allocator, staged.sections, staged.result, staged.work_path);
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

    const fixture = try std.fmt.allocPrint(allocator,
        \\-- script --
        \\tk --version
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

    // Expected body is computed from `build_options` so the assertion
    // survives `zig build test -Drelease-version=...` / `-Drelease-triple=...`.
    var expected_buf: [256]u8 = undefined;
    const expected = try std.fmt.bufPrint(&expected_buf, "{s} ({s})\n", .{
        build_options.version,
        build_options.triple,
    });
    var found_stdout = false;
    for (sections_after) |sec| {
        if (std.mem.eql(u8, sec.name, "expected/stdout")) {
            try std.testing.expectEqualStrings(expected, sec.body);
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
    // Build the fixture from `build_options` so the stdout section
    // matches whatever the test binary actually prints, isolating
    // the assertion to the exit-code mismatch the test is verifying.
    const fixture = try std.fmt.allocPrint(allocator,
        \\-- script --
        \\tk --version
        \\-- expected/stdout --
        \\{s} ({s})
        \\-- expected/stderr --
        \\
        \\-- expected/exit --
        \\1
        \\
    , .{ build_options.version, build_options.triple });
    defer allocator.free(fixture);

    try std.testing.expectError(
        error.ScenarioFailed,
        runScenarioWith(allocator, null, fixture, .{ .quiet = true }),
    );
}

test "runScenario: fails when stdin directive is not consumed" {
    const allocator = std.testing.allocator;
    const fixture =
        \\-- script --
        \\stdin message.md
        \\
        \\-- input/message.md --
        \\Title
        \\
        \\-- expected/stdout --
        \\-- expected/stderr --
        \\script: stdin set but no tk command consumed it
        \\-- expected/exit --
        \\3
        \\
    ;

    try runScenarioWith(allocator, null, fixture, .{ .quiet = true });
}

test "runScenario: fails when stdin source is missing" {
    const allocator = std.testing.allocator;
    const fixture =
        \\-- script --
        \\stdin missing.md
        \\
        \\-- expected/stdout --
        \\-- expected/stderr --
        \\script: stdin source not found: missing.md
        \\-- expected/exit --
        \\3
        \\
    ;

    try runScenarioWith(allocator, null, fixture, .{ .quiet = true });
}

test "runScenario: fails when stdin is set twice before a tk command" {
    const allocator = std.testing.allocator;
    const fixture =
        \\-- script --
        \\stdin first.md
        \\stdin second.md
        \\
        \\-- input/first.md --
        \\first
        \\-- input/second.md --
        \\second
        \\
        \\-- expected/stdout --
        \\-- expected/stderr --
        \\script: stdin already set
        \\-- expected/exit --
        \\3
        \\
    ;

    try runScenarioWith(allocator, null, fixture, .{ .quiet = true });
}

test "stdin source can use aggregate stdout and stderr" {
    const allocator = std.testing.allocator;
    var result = ScriptResult{
        .stdout = .empty,
        .stderr = .empty,
        .final_exit = 0,
    };
    defer result.deinit(allocator);

    try result.stdout.appendSlice(allocator, "first stdout\n");
    try result.stderr.appendSlice(allocator, "first stderr\n");

    const from_stdout = (try loadStdinSource(allocator, std.Io.Dir.cwd(), "stdout", &result)).?;
    defer allocator.free(from_stdout);
    const from_stderr = (try loadStdinSource(allocator, std.Io.Dir.cwd(), "stderr", &result)).?;
    defer allocator.free(from_stderr);

    try std.testing.expectEqualStrings("first stdout\n", from_stdout);
    try std.testing.expectEqualStrings("first stderr\n", from_stderr);
}

test "runScenario: stdin stdout is consumed by the next tk command" {
    const allocator = std.testing.allocator;
    // Build the fixture from `build_options` so the expected stdout
    // section matches whatever the test binary actually prints under
    // its build flags.
    const fixture = try std.fmt.allocPrint(allocator,
        \\-- script --
        \\tk --version
        \\stdin stdout
        \\tk --version
        \\
        \\-- expected/stdout --
        \\{s} ({s})
        \\{s} ({s})
        \\-- expected/stderr --
        \\-- expected/exit --
        \\0
        \\
    , .{
        build_options.version, build_options.triple,
        build_options.version, build_options.triple,
    });
    defer allocator.free(fixture);

    try runScenarioWith(allocator, null, fixture, .{ .quiet = true });
}

test "validateInputPath: rejects parent escapes and absolute paths" {
    try std.testing.expectError(error.InvalidInputPath, validateInputPath(""));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("/abs"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("../escape"));
    try std.testing.expectError(error.InvalidInputPath, validateInputPath("a/../b"));
    try validateInputPath("a/b/c");
    try validateInputPath("file.md");
}

test "normalizeWork: empty work_path returns the input untouched" {
    const allocator = std.testing.allocator;
    const text = "no substitution should happen here";
    const result = try normalizeWork(allocator, text, "");
    defer result.deinit(allocator);
    try std.testing.expectEqualStrings(text, result.text);
    try std.testing.expect(!result.owned);
}

test "rewriteSections: substitutes work_path in stdout and stderr bodies" {
    const allocator = std.testing.allocator;

    const sections = [_]Section{
        .{ .name = "expected/stdout", .body = "old\n" },
        .{ .name = "expected/stderr", .body = "old\n" },
        .{ .name = "expected/exit", .body = "1\n" },
    };

    var result: ScriptResult = .{
        .stdout = .empty,
        .stderr = .empty,
        .final_exit = 0,
    };
    defer result.deinit(allocator);
    try result.stdout.appendSlice(allocator, "store at /tmp/abc/project/.git/tk.db\n");
    try result.stderr.appendSlice(allocator, "wrote /tmp/abc/project/note\n");

    const rewritten = try rewriteSections(allocator, &sections, result, "/tmp/abc");
    defer allocator.free(rewritten);

    const parsed = try txtar.parse(allocator, rewritten);
    defer allocator.free(parsed);

    try std.testing.expectEqualStrings("expected/stdout", parsed[0].name);
    try std.testing.expectEqualStrings("store at $WORK/project/.git/tk.db\n", parsed[0].body);
    try std.testing.expectEqualStrings("expected/stderr", parsed[1].name);
    try std.testing.expectEqualStrings("wrote $WORK/project/note\n", parsed[1].body);
    try std.testing.expectEqualStrings("expected/exit", parsed[2].name);
    try std.testing.expectEqualStrings("0\n", parsed[2].body);
}

test "normalizeWorkSpans: rewrites only inside genuine $WORK path tokens" {
    const allocator = std.testing.allocator;

    // Happy path: substituted work path with native separators is converted
    // to the fixture's forward-slash form.
    {
        const buf = try allocator.dupe(u8, "store at $WORK\\project\\.git\\tk.db\n");
        defer allocator.free(buf);
        normalizeWorkSpans(buf);
        try std.testing.expectEqualStrings("store at $WORK/project/.git/tk.db\n", buf);
    }

    // Prefix-collision: `$WORKSPACE` shares the `$WORK` prefix but is a
    // distinct identifier. Its backslashes must not be rewritten.
    {
        const buf = try allocator.dupe(u8, "$WORKSPACE\\foo\\bar\n");
        defer allocator.free(buf);
        normalizeWorkSpans(buf);
        try std.testing.expectEqualStrings("$WORKSPACE\\foo\\bar\n", buf);
    }

    // Adjacent prose on the same line: the span ends at the first non-path
    // byte, so a quoted literal following the path keeps its backslashes.
    {
        const buf = try allocator.dupe(u8, "path $WORK\\foo 'lit\\eral'\n");
        defer allocator.free(buf);
        normalizeWorkSpans(buf);
        try std.testing.expectEqualStrings("path $WORK/foo 'lit\\eral'\n", buf);
    }

    // Multiple `$WORK` occurrences on separate lines are each rewritten.
    {
        const buf = try allocator.dupe(u8, "$WORK\\a\n$WORK\\b\n");
        defer allocator.free(buf);
        normalizeWorkSpans(buf);
        try std.testing.expectEqualStrings("$WORK/a\n$WORK/b\n", buf);
    }
}
