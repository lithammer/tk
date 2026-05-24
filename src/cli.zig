const std = @import("std");
const Allocator = std.mem.Allocator;

const build_options = @import("build_options");
const clap = @import("clap");
const http_mod = @import("http/client.zig");
const proc = @import("proc/runner.zig");
const clock_mod = @import("clock.zig");
const render = @import("render/styler.zig");
const parse_diagnostic = @import("commands/parse_diagnostic.zig");

/// Runtime services available to command handlers.
///
/// Commands receive these explicitly instead of reaching for process globals so
/// unit tests and txtar scenarios can substitute writers, clocks, working
/// directories, and subprocess runners.
pub const Deps = struct {
    /// Primary command output. Commands write user data here.
    stdout: *std.Io.Writer,
    /// Diagnostics and usage errors. Commands avoid writing normal output here.
    stderr: *std.Io.Writer,
    /// Primary command input. Commands read this only when their contract
    /// explicitly accepts stdin, such as `tk add -F -`.
    stdin: *std.Io.Reader,
    /// Allocator used for parsing and short-lived command work.
    gpa: Allocator,
    /// I/O implementation handle, threaded through filesystem and subprocess
    /// calls. Tests use `std.testing.io`; main uses the runtime's init.io.
    io: std.Io,
    /// User's working directory at invocation time. Passed as the cwd for
    /// subprocess discovery (e.g. `git rev-parse`); filesystem operations on
    /// absolute paths (the Repository Store location) bypass it.
    cwd: std.Io.Dir,
    /// Captured-output subprocess runner. Real impl wraps `std.process.run`;
    /// tests inject a `FakeRunner`.
    runner: proc.Runner,
    /// Type-erased HTTP client. Real impl wraps `std.http.Client.fetch`;
    /// tests inject a `FakeHttpClient` from `http/fake.zig`. Used by
    /// `tk self-update`; see ADR 0013 for the trust-root rationale.
    http: http_mod.Http,
    /// UTC millisecond clock. Tests inject a `FakeClock` so timestamps stay
    /// deterministic.
    clock: clock_mod.Clock,
    /// Random source used for opaque internal IDs.
    random: std.Random,
    /// Resolved per-stream color mode. Commands that emit styled output
    /// reach for `deps.styler.forStdout()` / `forStderr()` and let the
    /// returned sub-styler gate emission. See ADR 0014.
    styler: render.Styler,
};

/// Metadata every command module exports for dispatcher registration and help.
pub const CommandMeta = struct {
    /// CLI spelling, also used as the generated `SubCommand` tag name.
    name: [:0]const u8,
    /// One-line description shown in `tk --help`.
    description: []const u8,
};

/// Metadata for a planned subcommand whose surface is documented in `tk prime`
/// and `docs/cli.md` but whose implementation has not yet shipped.
///
/// Registered in `unimplemented_commands` so an agent that follows the
/// workflow vision from `tk prime` and reaches for a future command receives a
/// deliberate "not yet implemented" diagnostic instead of an unknown-
/// subcommand error from the top-level parser. The slot is removed when the
/// real command module lands in `all_commands`.
pub const UnimplementedMeta = struct {
    /// CLI spelling, also used as the generated `SubCommand` tag name.
    name: [:0]const u8,
    /// One-line description shown under the "Planned" section of `tk --help`.
    description: []const u8,
    /// One-line tracking note pointing at the slice that will ship the
    /// command. Rendered after the "not yet implemented" line on stderr so
    /// agents see where to look.
    tracking: []const u8,
};

const all_commands = .{
    @import("commands/init.zig"),
    @import("commands/add.zig"),
    @import("commands/block.zig"),
    @import("commands/done.zig"),
    @import("commands/list.zig"),
    @import("commands/manpage.zig"),
    @import("commands/next.zig"),
    @import("commands/prime.zig"),
    @import("commands/show.zig"),
    @import("commands/start.zig"),
    @import("commands/stop.zig"),
    @import("commands/unblock.zig"),
    @import("commands/update.zig"),
    @import("commands/worktree.zig"),
    @import("commands/remote.zig"),
    @import("commands/self_update.zig"),
    @import("commands/sync.zig"),
};

const remote_sync_slice = "Planned: remote and sync skeleton slice.";
const post_sync_slice = "Planned: later slice once Remote and sync are in place.";

const unimplemented_commands = [_]UnimplementedMeta{
    .{ .name = "promote", .description = "Promote a Local Ticket or Epic through the configured Remote", .tracking = post_sync_slice },
};

/// Top-level subcommand enum generated from `all_commands` and
/// `unimplemented_commands`.
///
/// The two compile-time tables are the only dispatcher touchpoints: the
/// enum, zig-clap parser, help listing, and dispatch switch all derive from
/// them. A new command starts as an `UnimplementedMeta` row (auto-registered
/// for "not yet implemented" diagnostics) and graduates to `all_commands`
/// when its module ships.
pub const SubCommand = blk: {
    const total = all_commands.len + unimplemented_commands.len;
    const Tag = std.math.IntFittingRange(0, total -| 1);
    var names: [total][]const u8 = undefined;
    var values: [total]Tag = undefined;
    for (all_commands, 0..) |cmd, i| {
        names[i] = cmd.meta.name;
        values[i] = @intCast(i);
    }
    for (unimplemented_commands, 0..) |stub, i| {
        names[all_commands.len + i] = stub.name;
        values[all_commands.len + i] = @intCast(all_commands.len + i);
    }
    break :blk @Enum(Tag, .exhaustive, &names, &values);
};

/// Parsed `--color` flag value. `null` means the flag was omitted, which
/// resolves to whatever `Mode.detect` returned (i.e. the env/TTY decision).
pub const ColorFlag = enum { auto, always, never };

/// Resolve the active Mode given the env/TTY-derived `env_mode` (from
/// `Mode.detect` in main.zig) and the parsed `--color` flag value. Explicit
/// `always`/`never` win over env; `auto` and omitted flag both pass through.
pub fn applyColorFlag(env_mode: render.Mode, flag: ?ColorFlag) render.Mode {
    return switch (flag orelse .auto) {
        .auto => env_mode,
        .always => .escape_codes,
        .never => .no_color,
    };
}

/// Comptime-computed width for the `tk --help` command listing column.
/// Walks both the implemented and planned command tables so adding a
/// longer command name doesn't break the column alignment.
const help_col_width: usize = blk: {
    var max: usize = 0;
    for (all_commands) |cmd| {
        if (cmd.meta.name.len > max) max = cmd.meta.name.len;
    }
    for (unimplemented_commands) |stub| {
        if (stub.name.len > max) max = stub.name.len;
    }
    break :blk max;
};

const top_flag_text =
    \\-h, --help           Display this help and exit.
    \\-v, --version        Print version and exit.
    \\    --color <color>  Color policy: auto, always, or never.
    \\
;
const top_flags = clap.parseParamsComptime(top_flag_text);
const top_params = clap.parseParamsComptime(top_flag_text ++
    \\<command>
    \\
);
const top_parsers = .{
    .command = clap.parsers.enumeration(SubCommand),
    .color = clap.parsers.enumeration(ColorFlag),
};

const help_options: clap.HelpOptions = .{
    .description_on_new_line = false,
    .description_indent = 2,
    .indent = 2,
    .spacing_between_parameters = 0,
};

/// Parse top-level args and dispatch to a command handler.
///
/// Returns tk's process exit code contract: 0 success, 1 logical failure,
/// 2 usage error, and propagated unexpected errors for `main.zig` to translate
/// to exit 3.
pub fn runArgv(deps: Deps, args_iter: anytype) !u8 {
    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &top_params, top_parsers, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .top_level,
        .terminating_positional = 0,
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeTopLevelHelp(deps) catch {};
        return 0;
    }
    if (res.args.version != 0) {
        deps.stdout.print("{s} ({s})\n", .{ build_options.version, build_options.triple }) catch {};
        return 0;
    }

    const subcmd = res.positionals[0] orelse {
        deps.stderr.writeAll("tk: missing subcommand; run 'tk --help' for usage\n") catch {};
        return 2;
    };

    var resolved = deps;
    resolved.styler.stdout = applyColorFlag(deps.styler.stdout, res.args.color);
    resolved.styler.stderr = applyColorFlag(deps.styler.stderr, res.args.color);

    return switch (subcmd) {
        inline else => |tag| blk: {
            inline for (all_commands) |cmd| {
                if (comptime std.mem.eql(u8, cmd.meta.name, @tagName(tag))) {
                    break :blk cmd.run(resolved, args_iter);
                }
            }
            inline for (unimplemented_commands) |stub| {
                if (comptime std.mem.eql(u8, stub.name, @tagName(tag))) {
                    break :blk runUnimplemented(resolved, stub);
                }
            }
            unreachable;
        },
    };
}

/// Render the "not yet implemented" diagnostic for a planned subcommand.
///
/// Returns exit 1 to keep the contract honest: an agent that invoked the
/// command did request work, the work did not happen, and exit 0 would mask
/// the gap. The body names the tracking slice so the agent knows where to
/// look without grepping the codebase.
fn runUnimplemented(deps: Deps, stub: UnimplementedMeta) !u8 {
    deps.stderr.print(
        "tk {s}: not yet implemented\n{s}\n",
        .{ stub.name, stub.tracking },
    ) catch {};
    return 1;
}

fn writeTopLevelHelp(deps: Deps) !void {
    try deps.stdout.writeAll(
        \\tk - an agent-first CLI for tracking work items
        \\
        \\Usage:
        \\  tk <command> [options]
        \\  tk [-h | --help]
        \\  tk [-v | --version]
        \\
        \\Commands:
        \\
    );
    inline for (all_commands) |cmd| {
        // Padding string is comptime-known per iteration because each
        // command's name length is comptime-known inside `inline for`.
        // This keeps the description column aligned even when a new
        // command (e.g. `self-update`, 11 chars) is longer than the
        // previous longest one.
        const padding = " " ** (help_col_width - cmd.meta.name.len);
        try deps.stdout.print("  {s}{s} {s}\n", .{ cmd.meta.name, padding, cmd.meta.description });
    }
    try deps.stdout.writeAll(
        \\
        \\Planned (not yet implemented):
        \\
    );
    inline for (unimplemented_commands) |stub| {
        const padding = " " ** (help_col_width - stub.name.len);
        try deps.stdout.print("  {s}{s} {s}\n", .{ stub.name, padding, stub.description });
    }
    try deps.stdout.writeAll(
        \\
        \\Options:
        \\
    );
    try clap.help(deps.stdout, clap.Help, &top_flags, help_options);
    try deps.stdout.writeAll(
        \\
        \\Run 'tk <command> --help' for command-specific help.
        \\
    );
}

const Harness = @import("testing/test_cli.zig").Harness;
const TmpStore = @import("testing/tmp_store.zig").TmpStore;
const zqlite = @import("zqlite");

fn expectStandardParseDiagnostic(stderr: []const u8, command_prefix: []const u8, hint_line: []const u8) !void {
    try std.testing.expect(std.mem.startsWith(u8, stderr, command_prefix));
    try std.testing.expect(std.mem.endsWith(u8, stderr, hint_line));
    try std.testing.expectEqual(@as(usize, 2), std.mem.count(u8, stderr, "\n"));
}

test "runArgv routes prime" {
    var h = Harness.init(std.testing.allocator, &.{"prime"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(h.stdout().len > 0);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "Deps.styler defaults to no_color on both streams under Harness" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();
    const d = h.deps();
    try std.testing.expect(d.styler.stdout == .no_color);
    try std.testing.expect(d.styler.stderr == .no_color);
}

test "runArgv accepts --color=always before subcommand" {
    var h = Harness.init(std.testing.allocator, &.{ "--color=always", "prime" });
    defer h.deinit();
    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(h.stdout().len > 0);
}

test "runArgv accepts --color=never before subcommand" {
    var h = Harness.init(std.testing.allocator, &.{ "--color=never", "prime" });
    defer h.deinit();
    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
}

test "runArgv rejects invalid --color value with standardized parse diagnostic" {
    var h = Harness.init(std.testing.allocator, &.{ "--color=zebra", "prime" });
    defer h.deinit();
    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try expectStandardParseDiagnostic(h.stderr(), "tk: ", "tk: run 'tk --help' for usage\n");
}

test "applyColorFlag: auto and null both pass env_mode through" {
    try std.testing.expect(applyColorFlag(.no_color, .auto) == .no_color);
    try std.testing.expect(applyColorFlag(.escape_codes, .auto) == .escape_codes);
    try std.testing.expect(applyColorFlag(.no_color, null) == .no_color);
    try std.testing.expect(applyColorFlag(.escape_codes, null) == .escape_codes);
}

test "applyColorFlag: always forces escape_codes; never forces no_color" {
    try std.testing.expect(applyColorFlag(.no_color, .always) == .escape_codes);
    try std.testing.expect(applyColorFlag(.escape_codes, .always) == .escape_codes);
    try std.testing.expect(applyColorFlag(.escape_codes, .never) == .no_color);
    try std.testing.expect(applyColorFlag(.no_color, .never) == .no_color);
}

test "Harness.Options overrides the per-stream Mode on Deps.styler" {
    var h = Harness.initWith(std.testing.allocator, &.{}, .{
        .stdout_mode = .escape_codes,
        .stderr_mode = .escape_codes,
    });
    defer h.deinit();
    try std.testing.expect(h.deps().styler.stdout == .escape_codes);
    try std.testing.expect(h.deps().styler.stderr == .escape_codes);
}

test "runArgv returns 2 on unknown subcommand" {
    var h = Harness.init(std.testing.allocator, &.{"bogus"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv returns 2 on extra positional after a subcommand" {
    // Trailing positionals after a known subcommand surface through the
    // subcommand parser; this representative case proves dispatch uses the
    // shared clap diagnostic wrapper without retesting every command.
    var h = Harness.init(std.testing.allocator, &.{ "prime", "unexpected" });
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try expectStandardParseDiagnostic(h.stderr(), "tk prime: ", "tk prime: run 'tk prime --help' for usage\n");
}

test "runArgv returns 2 on missing subcommand" {
    var h = Harness.init(std.testing.allocator, &.{});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 2), code);
    try std.testing.expect(h.stderr().len > 0);
}

test "runArgv prints version with embedded triple" {
    var h = Harness.init(std.testing.allocator, &.{"--version"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    // Compute the expectation from `build_options` so the assertion
    // stays accurate when `zig build test -Drelease-version=...` or
    // `-Drelease-triple=...` overrides the test binary's options. The
    // format string here mirrors the one in `runArgv`; verifying both
    // shape AND values from build_options keeps a swapped-args typo
    // visible even when version == triple.
    var expected_buf: [256]u8 = undefined;
    const expected = try std.fmt.bufPrint(&expected_buf, "{s} ({s})\n", .{
        build_options.version,
        build_options.triple,
    });
    try std.testing.expectEqualStrings(expected, h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv prints help" {
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Usage:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Commands:") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "init") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "add") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "done") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "list") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "manpage") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "prime") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "self-update") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--version") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "--color") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Planned (not yet implemented):") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "worktree") != null);
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "sync") != null);
    try std.testing.expectEqualStrings("", h.stderr());
}

test "runArgv help aligns command columns regardless of name length" {
    // Defends the help_col_width invariant: every implemented and
    // planned command name fits the comptime-computed column, so
    // adding a longer name (e.g. an 11-char `self-update`) keeps
    // every row visually aligned.
    var h = Harness.init(std.testing.allocator, &.{"--help"});
    defer h.deinit();
    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 0), code);

    // Every command name in the output is followed by `help_col_width
    // - name.len` padding spaces and then exactly one separator space
    // before its description. Pick a couple of long and short names
    // and confirm the column-after-name is at the same offset.
    const stdout = h.stdout();
    inline for (all_commands) |cmd| {
        const expected_prefix = "  " ++ cmd.meta.name ++ " " ** (help_col_width - cmd.meta.name.len) ++ " " ++ cmd.meta.description;
        try std.testing.expect(std.mem.indexOf(u8, stdout, expected_prefix) != null);
    }
}

test "runArgv routes a planned subcommand to a not-yet-implemented stub" {
    var h = Harness.init(std.testing.allocator, &.{"promote"});
    defer h.deinit();

    const code = try runArgv(h.deps(), &h.iter);
    try std.testing.expectEqual(@as(u8, 1), code);
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk promote: not yet implemented\n" ++ post_sync_slice ++ "\n",
        h.stderr(),
    );
}

test "runArgv block creates a Dependency that affects next ready Ticket" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Blocked work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });

    {
        var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("Added Dependency: project-1 blocked by project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    {
        var h = Harness.initWith(gpa, &.{"next"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
        try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "runArgv unblock removes a Dependency so the blocked Ticket is ready again" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Blocked work", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    {
        var h = Harness.initWith(gpa, &.{ "unblock", "project-1", "project-2" }, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("Removed Dependency: project-1 no longer blocked by project-2\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }

    {
        var h = Harness.initWith(gpa, &.{"next"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
        try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
        try std.testing.expectEqualStrings("project-1\n", h.stdout());
        try std.testing.expectEqualStrings("", h.stderr());
    }
}

test "runArgv block rejects a self Dependency with a role-specific diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "item", .display = "project-1", .title = "One item", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: an item cannot depend on itself\n", h.stderr());
}

test "runArgv unblock rejects a self Dependency argument pair" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "item", .display = "project-1", .title = "One item", .created_seq = 1 });

    var h = Harness.initWith(gpa, &.{ "unblock", "project-1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk unblock: an item cannot depend on itself\n", h.stderr());
}

test "runArgv block rejects a done Blocked Item" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocked", .display = "project-1", .title = "Finished work", .status = "done", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-2", .title = "Blocking work", .created_seq = 2 });

    var h = Harness.initWith(gpa, &.{ "block", "project-1", "project-2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: blocked item 'project-1' is done\n", h.stderr());
}

test "runArgv block rejects a Dependency cycle with a domain diagnostic" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "a", .display = "project-1", .title = "First item", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "b", .display = "project-2", .title = "Second item", .created_seq = 2 });
    try TmpStore.insertDependency(conn, "b", "a");

    var h = Harness.initWith(gpa, &.{ "block", "project-2", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings("tk block: Dependency would create a cycle\n", h.stderr());
}

test "runArgv block emits add_dependency Mutation for same-backend items" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Added Dependency: GH#1 blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());

    const row = (try conn.row(
        \\select mutation_type, item_id, item_class, payload_json
        \\  from mutations
        \\ where sequence = 1
    , .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("add_dependency", row.text(0));
    try std.testing.expectEqualStrings("blocked", row.text(1));
    try std.testing.expectEqualStrings("ticket", row.text(2));
    try std.testing.expectEqualStrings("{\"blocking_id\":\"blocking\"}", row.text(3));
}

test "runArgv block rejects Backend Blocked Item depending on Local Blocking Item" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{ .id = "blocking", .display = "project-1", .title = "Local blocking", .created_seq = 2 });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "project-1" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk block: Backend blocked item 'GH#1' cannot depend on Local blocking item 'project-1'\n",
        h.stderr(),
    );
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "runArgv block rejects Backend Dependency across Backend kinds" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "GitHub blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "JIRA-2",
        .title = "Jira blocking",
        .origin = "backend",
        .backend_kind = "jira",
        .backend_key = "2",
        .created_seq = 2,
    });

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "JIRA-2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        "tk block: Backend blocked item 'GH#1' cannot depend on blocking item 'JIRA-2' from another Backend kind\n",
        h.stderr(),
    );
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}

test "runArgv unblock emits remove_dependency Mutation for same-backend items" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    var h = Harness.initWith(gpa, &.{ "unblock", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Removed Dependency: GH#1 no longer blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());

    const row = (try conn.row(
        \\select mutation_type, item_id, item_class, payload_json
        \\  from mutations
        \\ where sequence = 1
    , .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("remove_dependency", row.text(0));
    try std.testing.expectEqualStrings("blocked", row.text(1));
    try std.testing.expectEqualStrings("ticket", row.text(2));
    try std.testing.expectEqualStrings("{\"blocking_id\":\"blocking\"}", row.text(3));
}

test "runArgv block existing Dependency is idempotent and emits no Mutation" {
    const gpa = std.testing.allocator;
    var store = try TmpStore.init(gpa, "project");
    defer store.deinit(gpa);

    var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, store.toplevel_path, .{});
    defer cwd.close(std.testing.io);

    const rev_parse = try store.gitRevParseStdout(gpa);
    defer gpa.free(rev_parse);

    {
        var h = Harness.initWith(gpa, &.{"init"}, .{ .cwd = cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    }

    const conn = try zqlite.open(store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocked",
        .display = "GH#1",
        .title = "Backend blocked",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "1",
        .created_seq = 1,
    });
    try TmpStore.insertFixtureItem(conn, .{
        .id = "blocking",
        .display = "GH#2",
        .title = "Backend blocking",
        .origin = "backend",
        .backend_kind = "github",
        .backend_key = "2",
        .created_seq = 2,
    });
    try TmpStore.insertDependency(conn, "blocking", "blocked");

    var h = Harness.initWith(gpa, &.{ "block", "GH#1", "GH#2" }, .{ .cwd = cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
    try std.testing.expectEqual(@as(u8, 0), try runArgv(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Added Dependency: GH#1 blocked by GH#2\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
    try std.testing.expectEqual(@as(i64, 0), try TmpStore.mutationCount(conn));
}
