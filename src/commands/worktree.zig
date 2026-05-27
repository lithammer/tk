//! `tk worktree` — inspect and configure Workspace Scope.

const std = @import("std");
const Allocator = std.mem.Allocator;

const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");
const resolver = @import("resolver.zig");
const worktree_scope = @import("../worktree/scope.zig");
const git_discovery = @import("../git/discovery.zig");
const ItemClass = @import("../domain/item_class.zig").ItemClass;
const ItemStatus = @import("../domain/status.zig").ItemStatus;

/// Dispatcher metadata for `tk worktree`.
pub const meta: cli.CommandMeta = .{
    .name = "worktree",
    .description = "Inspect or configure the current Workspace Scope",
};

const Subcommand = enum { clear, set, start };

/// Parse `tk worktree` args, dispatch to subcommand or report status.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    if (args_iter.next()) |s| {
        if (std.mem.eql(u8, s, "-h") or std.mem.eql(u8, s, "--help")) {
            writeHelp(deps) catch {};
            return 0;
        }
        if (std.meta.stringToEnum(Subcommand, s)) |sub| return switch (sub) {
            .clear => try runClear(deps),
            .set => try runSet(deps, args_iter),
            .start => try runStart(deps, args_iter),
        };
    }
    return try runStatus(deps);
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk worktree - inspect or configure Workspace Scope
        \\
        \\Usage:
        \\  tk worktree                     Report configured/inferred scope.
        \\  tk worktree set <id>            Configure Workspace Scope for this worktree.
        \\  tk worktree clear               Remove configured Workspace Scope.
        \\  tk worktree start <id> [path]   Create a Ticket Branch and scoped git worktree.
        \\
        \\Options for `start`:
        \\  --no-status   Skip marking the scoped item active.
        \\
        \\Options:
        \\  -h, --help    Display this help and exit.
        \\
    );
}

const status_storage_msgs: resolver.StorageErrorMessages = .{
    .busy_retry = messages.worktree_status_store_busy_retry,
    .out_of_memory = messages.worktree_status_out_of_memory,
    .fallback = messages.worktree_status_read_failed,
};

const set_storage_msgs: resolver.StorageErrorMessages = .{
    .busy_retry = messages.worktree_set_store_busy_retry,
    .out_of_memory = messages.worktree_set_out_of_memory,
    .fallback = messages.worktree_set_read_failed,
};

const start_storage_msgs: resolver.StorageErrorMessages = .{
    .busy_retry = messages.worktree_start_store_busy_retry,
    .out_of_memory = messages.worktree_start_out_of_memory,
    .fallback = messages.worktree_start_write_failed,
};

const status_open_msgs: resolver.OpenMessages = .{
    .command_name = "worktree",
    .missing_store = messages.worktree_status_missing_store,
    .storage = status_storage_msgs,
};

const set_open_msgs: resolver.OpenMessages = .{
    .command_name = "worktree set",
    .missing_store = messages.worktree_set_missing_store,
    .storage = set_storage_msgs,
};

const start_open_msgs: resolver.OpenMessages = .{
    .command_name = "worktree start",
    .missing_store = messages.worktree_start_missing_store,
    .storage = start_storage_msgs,
};

fn runStatus(deps: cli.Deps) !u8 {
    const store = (resolver.open(deps.gpa, deps.runner, deps.cwd, deps.stderr, status_open_msgs) orelse return 1).store;
    defer store.close();

    const raw = worktree_scope.readGitSide(deps.gpa, deps.runner, deps.cwd) catch |err| {
        resolver.renderStorageError(deps.stderr, err, status_storage_msgs);
        return 1;
    };
    defer worktree_scope.freeRaw(deps.gpa, raw);

    const scope_outcome = worktree_scope.resolveAgainstStore(store, deps.gpa, raw) catch |err| {
        resolver.renderStorageError(deps.stderr, err, status_storage_msgs);
        return 1;
    };
    switch (scope_outcome) {
        .none => {
            try deps.stdout.print("{s}\n", .{messages.worktree_no_scope});
            return 0;
        },
        .scope => |s| {
            defer s.deinit(deps.gpa);
            try deps.stdout.print("Scope:  {s} - {s}\n", .{ s.display_id, s.title });
            switch (s.source) {
                .configured => try deps.stdout.writeAll("Source: configured\n"),
                .inferred => try deps.stdout.print(
                    "Source: inferred from branch '{s}'\n",
                    .{raw.branch_name.?},
                ),
            }
            return 0;
        },
        .configured_unresolved => |stored| {
            defer deps.gpa.free(stored);
            deps.stderr.print(
                "{s}{s}{s}\n",
                .{ messages.worktree_status_unresolved_prefix, stored, messages.worktree_status_unresolved_suffix },
            ) catch {};
            return 1;
        },
    }
}

/// Resolved item fields needed by `tk worktree start`. One query against
/// `item_ids` joined to `items` replaces the prior `resolveItemRef` +
/// `showItem` pair, dropping two superfluous reads and the over-fetch of
/// container/dependency rows.
const StartTarget = struct {
    id: []u8,
    display_id: []u8,
    title: []u8,
    item_class: ItemClass,
    status: ItemStatus,

    fn deinit(self: StartTarget, gpa: Allocator) void {
        gpa.free(self.id);
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

fn lookupStartTarget(
    store: repository.Store,
    gpa: Allocator,
    display_arg: []const u8,
) repository.ResolveError!?StartTarget {
    const row = (try store.conn.row(
        \\select i.id, i.display_value, i.item_class, i.title, i.status
        \\  from item_ids ids
        \\  join items i on i.id = ids.item_id
        \\ where ids.value = ?1
    , .{display_arg})) orelse return null;
    defer row.deinit();
    const id = try gpa.dupe(u8, row.text(0));
    errdefer gpa.free(id);
    const display_id = try gpa.dupe(u8, row.text(1));
    errdefer gpa.free(display_id);
    const title = try gpa.dupe(u8, row.text(3));
    return .{
        .id = id,
        .display_id = display_id,
        .title = title,
        .item_class = std.meta.stringToEnum(ItemClass, row.text(2)) orelse unreachable,
        .status = std.meta.stringToEnum(ItemStatus, row.text(4)) orelse unreachable,
    };
}

fn runStart(deps: cli.Deps, args_iter: anytype) !u8 {
    var id_arg: ?[]const u8 = null;
    var path_arg: ?[]const u8 = null;
    var no_status = false;
    while (args_iter.next()) |arg| {
        if (std.mem.eql(u8, arg, "--no-status")) {
            no_status = true;
        } else if (id_arg == null) {
            id_arg = arg;
        } else if (path_arg == null) {
            path_arg = arg;
        }
    }
    const id = id_arg orelse {
        deps.stderr.print("{s}\n", .{messages.worktree_start_id_required}) catch {};
        return 2;
    };

    const store = (resolver.open(deps.gpa, deps.runner, deps.cwd, deps.stderr, start_open_msgs) orelse return 1).store;
    defer store.close();

    const target = (lookupStartTarget(store, deps.gpa, id) catch |err| {
        resolver.renderStorageError(deps.stderr, err, start_storage_msgs);
        return 1;
    }) orelse {
        deps.stderr.print(
            "{s}{s}{s}\n",
            .{ messages.worktree_start_id_not_found_prefix, id, messages.worktree_start_id_not_found_suffix },
        ) catch {};
        return 1;
    };
    defer target.deinit(deps.gpa);

    if (target.status == .done) {
        deps.stderr.print(
            "{s}{s}\n",
            .{ messages.worktree_start_locked_done_prefix, target.item_class.label() },
        ) catch {};
        return 1;
    }

    const branch_slug = try worktree_scope.sanitize(deps.gpa, target.title, 40);
    defer deps.gpa.free(branch_slug);
    const path_slug = try worktree_scope.sanitize(deps.gpa, target.title, 30);
    defer deps.gpa.free(path_slug);

    const branch = try buildBranch(deps.gpa, target.display_id, branch_slug);
    defer deps.gpa.free(branch);

    const worktree_path = if (path_arg) |p|
        try deps.gpa.dupe(u8, p)
    else
        (try buildDefaultPath(deps, target.display_id, path_slug)) orelse return 1;
    defer deps.gpa.free(worktree_path);

    if (!(try runGitOrFail(deps, &.{ "git", "config", "extensions.worktreeConfig", "true" }, messages.worktree_start_git_failed))) return 1;
    if (!(try runGitOrFail(deps, &.{ "git", "worktree", "add", "-b", branch, worktree_path }, messages.worktree_start_git_failed))) return 1;
    if (!(try runGitOrFail(deps, &.{ "git", "-C", worktree_path, "config", "--worktree", "tk.scope", id }, messages.worktree_start_git_failed))) return 1;

    if (!no_status) {
        const set_outcome = repository.setItemStatus(store, deps.gpa, deps.clock, .{
            .id = target.id,
            .status = .active,
        }) catch |err| {
            resolver.renderStorageError(deps.stderr, err, start_storage_msgs);
            return 1;
        };
        switch (set_outcome) {
            .ok => |snap| snap.deinit(deps.gpa),
            // pre-checked target.status above; the schema trigger from ADR
            // 0006 backstops any future writer that bypasses this path.
            .not_found, .locked_done => unreachable,
        }
    }

    try deps.stdout.print("{s}{s}: {s} - {s}\n", .{
        messages.worktree_start_success_prefix,
        target.item_class.label(),
        target.display_id,
        target.title,
    });
    if (!no_status) try deps.stdout.writeAll("Status: active\n");
    try deps.stdout.print("Branch: {s}\n", .{branch});
    try deps.stdout.print("Path:   {s}\n", .{worktree_path});
    return 0;
}

fn buildBranch(gpa: Allocator, display_id: []const u8, slug: []const u8) ![]u8 {
    if (slug.len == 0) return try std.fmt.allocPrint(gpa, "tk/{s}", .{display_id});
    return try std.fmt.allocPrint(gpa, "tk/{s}-{s}", .{ display_id, slug });
}

/// Compute the default worktree path: `<parent-of-main-toplevel>/<repo>.<id-sanitized>-<slug>`.
///
/// "Parent of main toplevel" is derived from `git rev-parse --show-toplevel`
/// of the current worktree; the linked-worktree edge case is an accepted
/// surviving risk for v1 per the design contract. Returns `null` so callers
/// can `orelse return 1` after the discovery diagnostic is rendered.
fn buildDefaultPath(
    deps: cli.Deps,
    display_id: []const u8,
    slug: []const u8,
) error{OutOfMemory}!?[]u8 {
    var outcome = try git_discovery.discoverPaths(deps.gpa, deps.runner, deps.cwd);
    switch (outcome) {
        .ok => |*paths| {
            defer paths.deinit(deps.gpa);
            const parent = std.fs.path.dirname(paths.toplevel) orelse "/";
            const repo = std.fs.path.basename(paths.toplevel);
            const id_sanitized = try worktree_scope.sanitize(deps.gpa, display_id, std.math.maxInt(usize));
            defer deps.gpa.free(id_sanitized);
            const leaf = if (slug.len == 0)
                try std.fmt.allocPrint(deps.gpa, "{s}.{s}", .{ repo, id_sanitized })
            else
                try std.fmt.allocPrint(deps.gpa, "{s}.{s}-{s}", .{ repo, id_sanitized, slug });
            defer deps.gpa.free(leaf);
            return try std.fs.path.join(deps.gpa, &.{ parent, leaf });
        },
        else => {
            git_discovery.renderFailure(deps.stderr, deps.gpa, "worktree start", outcome);
            return null;
        },
    }
}

fn runSet(deps: cli.Deps, args_iter: anytype) !u8 {
    const id = args_iter.next() orelse {
        deps.stderr.print("{s}\n", .{messages.worktree_set_id_required}) catch {};
        return 2;
    };

    const r = resolver.open(deps.gpa, deps.runner, deps.cwd, deps.stderr, set_open_msgs) orelse return 1;
    defer r.close();

    const resolved = r.resolve(id, .{
        .prefix = messages.worktree_set_id_not_found_prefix,
        .suffix = messages.worktree_set_id_not_found_suffix,
    }) orelse return 1;
    resolved.deinit(deps.gpa);

    if (!(try runGitOrFail(deps, &.{ "git", "config", "extensions.worktreeConfig", "true" }, messages.worktree_set_failed))) return 1;
    if (!(try runGitOrFail(deps, &.{ "git", "config", "--worktree", "tk.scope", id }, messages.worktree_set_failed))) return 1;

    try deps.stdout.print("{s}{s}\n", .{ messages.worktree_set_prefix, id });
    return 0;
}

/// Run a git subprocess. Returns `true` on exit 0, `false` on spawn failure
/// or non-zero exit (with the diagnostic already written to stderr).
///
/// On a non-zero exit, `failure_msg` is the framing line naming which tk
/// operation failed; git's own captured stderr is then forwarded on a second
/// line so the caller sees git's precise reason (e.g. a worktree path or
/// branch collision) rather than only the generic frame. Empty git stderr is
/// omitted. tk treats git as the authority on these collisions and does not
/// preflight-reimplement git's existence checks — see ARCHITECTURE.md and
/// tk-15. Stderr trimming mirrors `git.discovery`.
fn runGitOrFail(deps: cli.Deps, argv: []const []const u8, failure_msg: []const u8) error{OutOfMemory}!bool {
    var result = deps.runner.run(deps.gpa, .{ .argv = argv, .cwd = deps.cwd }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => {
            deps.stderr.print("{s}\n{s}\n", .{ failure_msg, @errorName(err) }) catch {};
            return false;
        },
    };
    defer result.deinit(deps.gpa);
    if (result.exit.code() != 0) {
        deps.stderr.print("{s}\n", .{failure_msg}) catch {};
        const git_stderr = std.mem.trim(u8, result.stderr, " \t\r\n");
        if (git_stderr.len > 0) deps.stderr.print("{s}\n", .{git_stderr}) catch {};
        return false;
    }
    return true;
}

fn renderSetStorageError(deps: cli.Deps, err: anyerror) void {
    resolver.renderStorageError(deps.stderr, err, .{
        .busy_retry = messages.worktree_set_store_busy_retry,
        .out_of_memory = messages.worktree_set_out_of_memory,
        .fallback = messages.worktree_set_read_failed,
    });
}

fn runClear(deps: cli.Deps) !u8 {
    var result = deps.runner.run(deps.gpa, .{
        .argv = &.{ "git", "config", "--worktree", "--unset", "tk.scope" },
        .cwd = deps.cwd,
    }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => {
            deps.stderr.print("{s}\n{s}\n", .{ messages.worktree_clear_failed, @errorName(err) }) catch {};
            return 1;
        },
    };
    defer result.deinit(deps.gpa);

    // Exit 5 is git's "key already absent" code; the design contract treats
    // that as the idempotent no-op success path.
    if (result.exit.code() != 0 and result.exit.code() != 5) {
        deps.stderr.print("{s}\n", .{messages.worktree_clear_failed}) catch {};
        // Same forwarding contract as `runGitOrFail`: surface git's own
        // captured stderr beneath the frame. This call site is inline rather
        // than routed through the helper because exit 5 is a success path.
        const git_stderr = std.mem.trim(u8, result.stderr, " \t\r\n");
        if (git_stderr.len > 0) deps.stderr.print("{s}\n", .{git_stderr}) catch {};
        return 1;
    }
    try deps.stdout.print("{s}\n", .{messages.worktree_cleared});
    return 0;
}

const Harness = @import("../testing/test_cli.zig").Harness;
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const init_command = @import("init.zig");
const zqlite = @import("zqlite");

const StoreFixture = struct {
    tmp_store: TmpStore,
    cwd: std.Io.Dir,
    rev_parse: []u8,

    fn init(gpa: Allocator) !StoreFixture {
        var tmp_store = try TmpStore.init(gpa, "project");
        errdefer tmp_store.deinit(gpa);
        var cwd = try std.Io.Dir.cwd().openDir(std.testing.io, tmp_store.toplevel_path, .{});
        errdefer cwd.close(std.testing.io);
        const rev_parse = try tmp_store.gitRevParseStdout(gpa);
        errdefer gpa.free(rev_parse);

        {
            var h = Harness.init(gpa, &.{}, .{ .cwd = cwd });
            defer h.deinit();
            try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = rev_parse });
            try std.testing.expectEqual(@as(u8, 0), try init_command.run(h.deps(), &h.iter));
        }

        return .{ .tmp_store = tmp_store, .cwd = cwd, .rev_parse = rev_parse };
    }

    fn deinit(self: *StoreFixture, gpa: Allocator) void {
        gpa.free(self.rev_parse);
        self.cwd.close(std.testing.io);
        self.tmp_store.deinit(gpa);
    }
};

test "worktree: with configured scope prints two-line block" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement scope", .created_seq = 1 });

    var h = Harness.init(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 0, .stdout = "project-1\n" });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\Scope:  project-1 - Implement scope
        \\Source: configured
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree: with inferred branch scope prints provenance hint" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement scope", .created_seq = 1 });

    var h = Harness.init(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 0, .stdout = "tk/project-1-implement-scope\n" });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        \\Scope:  project-1 - Implement scope
        \\Source: inferred from branch 'tk/project-1-implement-scope'
        \\
    , h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree: with unresolved configured scope exits 1 with diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.init(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 0, .stdout = "ghost-42\n" });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_status_unresolved_prefix ++ "ghost-42" ++ messages.worktree_status_unresolved_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree start: success block lists Ticket, status, branch, and absolute path" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Implement worktree start",
        .created_seq = 1,
    });

    // Expected derived names.
    const branch = "tk/project-1-implement-worktree-start";
    const path_leaf = "project.project-1-implement-worktree-start";
    const parent = std.fs.path.dirname(fixture.tmp_store.toplevel_path).?;
    const path = try std.fs.path.join(gpa, &.{ parent, path_leaf });
    defer gpa.free(path);

    var h = Harness.init(gpa, &.{ "start", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "worktree", "add", "-b", branch, path }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "-C", path, "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    const expected = try std.fmt.allocPrint(gpa,
        \\Created worktree for Ticket: project-1 - Implement worktree start
        \\Status: active
        \\Branch: {s}
        \\Path:   {s}
        \\
    , .{ branch, path });
    defer gpa.free(expected);
    try std.testing.expectEqualStrings(expected, h.stdout());

    // Ticket should be active.
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("active", row.text(0));
}

test "worktree start: forwards git's own message when worktree add collides" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Implement worktree start",
        .created_seq = 1,
    });

    const branch = "tk/project-1-implement-worktree-start";
    const path_leaf = "project.project-1-implement-worktree-start";
    const parent = std.fs.path.dirname(fixture.tmp_store.toplevel_path).?;
    const path = try std.fs.path.join(gpa, &.{ parent, path_leaf });
    defer gpa.free(path);

    const git_msg = "fatal: a branch named '" ++ branch ++ "' already exists";

    var h = Harness.init(gpa, &.{ "start", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    // Re-running start collides on the would-be branch. tk forwards git's own
    // message (trailing newline trimmed) instead of preflighting the
    // collision itself; the generic frame names the operation, git names why.
    try h.fake_runner.expect(&.{ "git", "worktree", "add", "-b", branch, path }, .{
        .exit_code = 128,
        .stderr = git_msg ++ "\n",
    });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_start_git_failed ++ "\n" ++ git_msg ++ "\n",
        h.stderr(),
    );

    // The failed git step short-circuits before the status write, so the
    // Ticket stays at its pre-start status rather than flipping to active.
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
}

test "worktree start: rejects a done Ticket uniformly per ADR 0006" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Done already",
        .status = "done",
        .created_seq = 1,
    });

    var h = Harness.init(gpa, &.{ "start", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_start_locked_done_prefix ++ "Ticket\n",
        h.stderr(),
    );
}

test "worktree start: --no-status omits the Status line and leaves item open" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{
        .id = "t1",
        .display = "project-1",
        .title = "Implement worktree start",
        .created_seq = 1,
    });

    const branch = "tk/project-1-implement-worktree-start";
    const path_leaf = "project.project-1-implement-worktree-start";
    const parent = std.fs.path.dirname(fixture.tmp_store.toplevel_path).?;
    const path = try std.fs.path.join(gpa, &.{ parent, path_leaf });
    defer gpa.free(path);

    var h = Harness.init(gpa, &.{ "start", "project-1", "--no-status" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "worktree", "add", "-b", branch, path }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "-C", path, "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    // No Status line; item should remain `open`.
    try std.testing.expect(std.mem.indexOf(u8, h.stdout(), "Status:") == null);
    const row = (try conn.row("select status from items where id = 't1'", .{})) orelse return error.ExpectedRow;
    defer row.deinit();
    try std.testing.expectEqualStrings("open", row.text(0));
}

test "worktree start: unknown id exits 1" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.init(gpa, &.{ "start", "no-such-id" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(
        messages.worktree_start_id_not_found_prefix ++ "no-such-id" ++ messages.worktree_start_id_not_found_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree start: missing positional id exits 2" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"start"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings(messages.worktree_start_id_required ++ "\n", h.stderr());
}

test "worktree: with no scope prints No Workspace Scope and exits 0" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.init(gpa, &.{}, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try h.fake_runner.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("No Workspace Scope.\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: runs git config --unset and prints success" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"}, .{});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: exit code 5 from git config --unset is the idempotent no-op" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"}, .{});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 5 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Workspace Scope cleared\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree set: missing positional id exits 2 with usage" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"set"}, .{});
    defer h.deinit();

    try std.testing.expectEqual(@as(u8, 2), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_set_id_required ++ "\n", h.stderr());
}

test "worktree set: unknown id exits 1 with role-specific diagnostic" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    var h = Harness.init(gpa, &.{ "set", "no-such-id" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_set_id_not_found_prefix ++ "no-such-id" ++ messages.worktree_set_id_not_found_suffix ++ "\n",
        h.stderr(),
    );
}

test "worktree set: git config failure surfaces as exit 1" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);
    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "T", .created_seq = 1 });

    var h = Harness.init(gpa, &.{ "set", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 128 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_set_failed ++ "\n", h.stderr());
}

test "worktree set: validates id, enables per-worktree config, writes tk.scope" {
    const gpa = std.testing.allocator;
    var fixture = try StoreFixture.init(gpa);
    defer fixture.deinit(gpa);

    const conn = try zqlite.open(fixture.tmp_store.db_path.ptr, zqlite.OpenFlags.ReadWrite | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try TmpStore.insertFixtureItem(conn, .{ .id = "t1", .display = "project-1", .title = "Implement", .created_seq = 1 });

    var h = Harness.init(gpa, &.{ "set", "project-1" }, .{ .cwd = fixture.cwd });
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "rev-parse" }, .{ .exit_code = 0, .stdout = fixture.rev_parse });
    try h.fake_runner.expect(&.{ "git", "config", "extensions.worktreeConfig", "true" }, .{ .exit_code = 0 });
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "tk.scope", "project-1" }, .{ .exit_code = 0 });

    try std.testing.expectEqual(@as(u8, 0), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("Set Workspace Scope to project-1\n", h.stdout());
    try std.testing.expectEqualStrings("", h.stderr());
}

test "worktree clear: other non-zero git exit surfaces as exit 1" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"}, .{});
    defer h.deinit();
    try h.fake_runner.expect(&.{ "git", "config", "--worktree", "--unset", "tk.scope" }, .{ .exit_code = 128 });

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(messages.worktree_clear_failed ++ "\n", h.stderr());
}

test "worktree clear: forwards git's own message on non-zero exit" {
    const gpa = std.testing.allocator;
    var h = Harness.init(gpa, &.{"clear"}, .{});
    defer h.deinit();
    const git_msg = "error: could not lock config file .git/config: Permission denied";
    try h.fake_runner.expect(
        &.{ "git", "config", "--worktree", "--unset", "tk.scope" },
        .{ .exit_code = 128, .stderr = git_msg ++ "\n" },
    );

    try std.testing.expectEqual(@as(u8, 1), try run(h.deps(), &h.iter));
    try std.testing.expectEqualStrings("", h.stdout());
    try std.testing.expectEqualStrings(
        messages.worktree_clear_failed ++ "\n" ++ git_msg ++ "\n",
        h.stderr(),
    );
}
