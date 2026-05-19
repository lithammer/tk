//! `tk remote` — inspect and configure the singleton Remote.
//!
//! Subcommands:
//!   tk remote                     Show current Remote (or "None").
//!   tk remote set github --repo OWNER/NAME
//!   tk remote set jira --site URL --project KEY
//!   tk remote clear               Remove the Remote when no pending/failed
//!                                 Mutations would be orphaned.

const std = @import("std");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const store_sync = @import("../store/sync.zig");
const Diagnostic = @import("../domain/diagnostic.zig").Diagnostic;

/// Dispatcher metadata for `tk remote`.
pub const meta: cli.CommandMeta = .{
    .name = "remote",
    .description = "Inspect or configure the Remote",
};

const Subcommand = enum { set, clear };

pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    if (args_iter.next()) |s| {
        if (std.mem.eql(u8, s, "-h") or std.mem.eql(u8, s, "--help")) {
            writeHelp(deps) catch {};
            return 0;
        }
        if (std.meta.stringToEnum(Subcommand, s)) |sub| return switch (sub) {
            .set => try runSet(deps, args_iter),
            .clear => try runClear(deps),
        };
        deps.stderr.print("tk remote: unknown subcommand '{s}'\n", .{s}) catch {};
        return 2;
    }
    return try runStatus(deps);
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk remote - inspect or configure the Remote
        \\
        \\Usage:
        \\  tk remote                                   Show the current Remote.
        \\  tk remote set github --repo OWNER/NAME      Configure GitHub Remote.
        \\  tk remote set jira --site URL --project K   Configure Jira Remote.
        \\  tk remote clear                             Remove the configured Remote.
        \\
        \\Options:
        \\  -h, --help    Display this help and exit.
        \\
    );
}

const status_storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.remote_status_store_busy_retry,
    .out_of_memory = messages.remote_status_out_of_memory,
    .fallback = messages.remote_status_read_failed,
};

const status_open_msgs: repository.OpenMessages = .{
    .command_name = "remote",
    .missing_store = messages.remote_status_missing_store,
    .storage = status_storage_msgs,
};

const set_storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.remote_set_store_busy_retry,
    .out_of_memory = messages.remote_set_out_of_memory,
    .fallback = messages.remote_set_write_failed,
};

const set_open_msgs: repository.OpenMessages = .{
    .command_name = "remote set",
    .missing_store = messages.remote_set_missing_store,
    .storage = set_storage_msgs,
};

const clear_storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.remote_clear_store_busy_retry,
    .out_of_memory = messages.remote_clear_out_of_memory,
    .fallback = messages.remote_clear_write_failed,
};

const clear_open_msgs: repository.OpenMessages = .{
    .command_name = "remote clear",
    .missing_store = messages.remote_clear_missing_store,
    .storage = clear_storage_msgs,
};

fn runStatus(deps: cli.Deps) !u8 {
    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, status_open_msgs) orelse return 1;
    defer store.close();

    const row_opt = store_sync.getRemote(store.conn, deps.gpa) catch |err| {
        repository.renderStorageError(deps.stderr, err, status_storage_msgs);
        return 1;
    };
    if (row_opt) |row| {
        defer row.deinit(deps.gpa);
        try deps.stdout.print("Remote: {s} ({s})\n", .{ row.backend_kind, row.config_json });
        return 0;
    }
    try deps.stdout.print("{s}\n", .{messages.remote_status_none});
    return 0;
}

const BackendKind = enum { github, jira };

fn runSet(deps: cli.Deps, args_iter: anytype) !u8 {
    const kind_arg = args_iter.next() orelse {
        try writeHelp(deps);
        return 2;
    };
    const kind = std.meta.stringToEnum(BackendKind, kind_arg) orelse {
        deps.stderr.print(
            "{s}{s}{s}\n",
            .{ messages.remote_set_unknown_kind_prefix, kind_arg, messages.remote_set_unknown_kind_suffix },
        ) catch {};
        return 2;
    };

    return switch (kind) {
        .github => try runSetGithub(deps, args_iter),
        .jira => try runSetJira(deps, args_iter),
    };
}

fn runSetGithub(deps: cli.Deps, args_iter: anytype) !u8 {
    var repo: ?[]const u8 = null;
    while (args_iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--repo")) {
            repo = args_iter.next();
        }
    }
    const repo_val = repo orelse {
        deps.stderr.print("{s}\n", .{messages.remote_set_github_repo_required}) catch {};
        return 2;
    };
    if (std.mem.indexOfScalar(u8, repo_val, '/') == null) {
        deps.stderr.print("{s}\n", .{messages.remote_set_github_repo_malformed}) catch {};
        return 2;
    }

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, set_open_msgs) orelse return 1;
    defer store.close();

    // Validate prefix collision before writing.
    if (try checkPrefixCollision(store.conn, deps.gpa, "gh", deps.stderr)) return 1;

    const config_json = try std.json.Stringify.valueAlloc(deps.gpa, .{ .repo = repo_val }, .{ .escape_unicode = true });
    defer deps.gpa.free(config_json);

    var now_buf: [24]u8 = undefined;
    const now = deps.clock.nowIso(&now_buf);

    var diag: Diagnostic = .{};
    store_sync.setRemote(store.conn, @tagName(BackendKind.github), config_json, now, &diag) catch |err| {
        renderStorageDiag(deps.stderr, err, set_storage_msgs, &diag);
        return 1;
    };

    try deps.stdout.print("{s}github ({s})\n", .{ messages.remote_set_success_prefix, repo_val });
    return 0;
}

fn runSetJira(deps: cli.Deps, args_iter: anytype) !u8 {
    var site: ?[]const u8 = null;
    var project: ?[]const u8 = null;
    while (args_iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--site")) {
            site = args_iter.next();
        } else if (std.mem.eql(u8, arg, "--project")) {
            project = args_iter.next();
        }
    }
    const site_val = site orelse {
        deps.stderr.print("{s}\n", .{messages.remote_set_jira_required}) catch {};
        return 2;
    };
    const project_val = project orelse {
        deps.stderr.print("{s}\n", .{messages.remote_set_jira_required}) catch {};
        return 2;
    };

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, set_open_msgs) orelse return 1;
    defer store.close();

    // Validate prefix collision: jira's adapter namespace is the project key.
    const project_lower = try std.ascii.allocLowerString(deps.gpa, project_val);
    defer deps.gpa.free(project_lower);
    if (try checkPrefixCollision(store.conn, deps.gpa, project_lower, deps.stderr)) return 1;

    const config_json = try std.json.Stringify.valueAlloc(
        deps.gpa,
        .{ .site = site_val, .project = project_val },
        .{ .escape_unicode = true },
    );
    defer deps.gpa.free(config_json);

    var now_buf: [24]u8 = undefined;
    const now = deps.clock.nowIso(&now_buf);

    var diag: Diagnostic = .{};
    store_sync.setRemote(store.conn, @tagName(BackendKind.jira), config_json, now, &diag) catch |err| {
        renderStorageDiag(deps.stderr, err, set_storage_msgs, &diag);
        return 1;
    };

    try deps.stdout.print("{s}jira ({s} / {s})\n", .{ messages.remote_set_success_prefix, site_val, project_val });
    return 0;
}

/// Returns true and renders a diagnostic if `local_prefix` collides with the
/// adapter's namespace prefix.
fn checkPrefixCollision(
    conn: anytype,
    gpa: std.mem.Allocator,
    adapter_prefix: []const u8,
    stderr: *std.Io.Writer,
) !bool {
    const local_prefix = repository.queryTextAlloc(
        conn,
        gpa,
        "select value from store_config where key = 'display_prefix'",
    ) catch |err| switch (err) {
        error.Notfound => return false, // No prefix configured; nothing to collide with.
        else => return err,
    };
    defer gpa.free(local_prefix);

    if (std.ascii.eqlIgnoreCase(local_prefix, adapter_prefix)) {
        stderr.print(
            "{s}{s}{s}\n",
            .{ messages.remote_set_prefix_collision_prefix, local_prefix, messages.remote_set_prefix_collision_suffix },
        ) catch {};
        return true;
    }
    return false;
}

fn runClear(deps: cli.Deps) !u8 {
    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, clear_open_msgs) orelse return 1;
    defer store.close();

    const count = store_sync.pendingOrFailedMutationCount(store.conn) catch |err| {
        repository.renderStorageError(deps.stderr, err, clear_storage_msgs);
        return 1;
    };
    if (count > 0) {
        deps.stderr.print(
            "{s}{d}{s}\n",
            .{ messages.remote_clear_refused_prefix, count, messages.remote_clear_refused_suffix },
        ) catch {};
        return 1;
    }

    var diag: Diagnostic = .{};
    store_sync.clearRemote(store.conn, &diag) catch |err| {
        renderStorageDiag(deps.stderr, err, clear_storage_msgs, &diag);
        return 1;
    };

    try deps.stdout.print("{s}\n", .{messages.remote_clear_success});
    return 0;
}

/// Render a Repository Store error plus the captured Diagnostic message
/// (when non-empty). The Diagnostic carries SQLite errmsg captured by
/// migrations.captureError before the rollback fired.
fn renderStorageDiag(
    stderr: *std.Io.Writer,
    err: anyerror,
    msgs: repository.StorageErrorMessages,
    diag: *const Diagnostic,
) void {
    repository.renderStorageError(stderr, err, msgs);
    if (diag.message().len > 0) stderr.print("{s}\n", .{diag.message()}) catch {};
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const init_command = @import("init.zig");
const zqlite = @import("zqlite");

const StoreFixture = struct {
    tmp_store: TmpStore,
    cwd: std.Io.Dir,
    rev_parse: []u8,

    fn init(gpa: std.mem.Allocator, basename: []const u8) !StoreFixture {
        var tmp_store = try TmpStore.init(gpa, basename);
        errdefer tmp_store.deinit(gpa);
        var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
        errdefer cwd.close(std.testing.io);
        const rev_parse = try tmp_store.gitRevParseStdout(gpa);
        errdefer gpa.free(rev_parse);

        {
            var h = Harness.initWith(gpa, &.{}, .{ .cwd = cwd });
            defer h.deinit();
            try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
            try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
        }

        return .{ .tmp_store = tmp_store, .cwd = cwd, .rev_parse = rev_parse };
    }

    fn deinit(self: *StoreFixture, gpa: std.mem.Allocator) void {
        gpa.free(self.rev_parse);
        self.cwd.close(std.testing.io);
        self.tmp_store.deinit(gpa);
    }
};

test "tk remote: status with no Remote prints None" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.remote_status_none) != null);
}

test "tk remote set github: round-trips to status" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    {
        var h = Harness.initWith(gpa, &.{ "set", "github", "--repo", "owner/name" }, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "github") != null);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "owner/name") != null);
    }

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "github") != null);
    }
}

test "tk remote set github: refuses missing --repo" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "set", "github" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), messages.remote_set_github_repo_required) != null);
}

test "tk remote set: prefix collision refuses with diagnostic" {
    const gpa = std.testing.allocator;
    // Repository basename "gh" derives display_prefix = "gh", which collides
    // with the github adapter's namespace.
    var fixture = try StoreFixture.init(gpa, "gh");
    defer fixture.deinit(gpa);

    var h = Harness.initWith(gpa, &.{ "set", "github", "--repo", "o/r" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "collides") != null);
}

test "tk remote clear: removes configured Remote" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    {
        var h = Harness.initWith(gpa, &.{ "set", "github", "--repo", "o/r" }, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    }

    {
        var h = Harness.initWith(gpa, &.{"clear"}, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), messages.remote_clear_success) != null);
    }
}

test "tk remote clear: refuses when pending or failed mutations exist" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    // Configure a Remote first.
    {
        var h = Harness.initWith(gpa, &.{ "set", "github", "--repo", "o/r" }, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    }

    // Seed a pending Mutation directly via SQL.
    {
        const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
        defer conn.close();
        try TmpStore.insertFixtureItem(conn, .{
            .id = "t1",
            .display = "gh-1",
            .title = "T",
            .origin = "backend",
            .backend_kind = "github",
            .backend_key = "1",
            .created_seq = 1,
        });
        try TmpStore.insertFixtureMutation(conn, .{
            .sequence = 1,
            .mutation_type = "update_ticket",
            .item_id = "t1",
            .payload_json = "{\"title\":\"A\",\"body\":\"\"}",
            .state = "pending",
        });
    }

    var h = Harness.initWith(gpa, &.{"clear"}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expect(std.mem.indexOf(u8, h.stderr(), "pending or failed") != null);
}

test "tk remote set jira: round-trips to status" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa, "project");
    defer fixture.deinit(gpa);

    {
        var h = Harness.initWith(gpa, &.{ "set", "jira", "--site", "https://example.atlassian.net", "--project", "PROJ" }, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "jira") != null);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "PROJ") != null);
    }

    {
        var h = Harness.initWith(gpa, &.{}, .{ .cwd = fixture.cwd });
        defer h.deinit();
        try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
        try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "jira") != null);
        try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "example.atlassian.net") != null);
    }
}
