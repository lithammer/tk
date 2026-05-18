//! Workspace Scope discovery and helpers.
//!
//! Per `docs/implementation.md` "Worktrees" section, this module owns
//! Workspace Scope storage, discovery (configured + inferred), branch-name
//! inference, slug derivation, and the git config helpers used by
//! `tk worktree set`, `clear`, and `start`.

const std = @import("std");
const zqlite = @import("zqlite");
const proc = @import("../proc/runner.zig");
const repository = @import("../store/repository.zig");
const migrations = @import("../store/migrations.zig");
const TmpStore = @import("../testing/tmp_store.zig").TmpStore;
const fake_proc = @import("../proc/fake.zig");

/// Workspace Scope and its provenance.
///
/// `display_id` is the *current* Display ID after Alias resolution, so
/// configured scope rendered by `tk worktree` matches the user's working
/// vocabulary even when the stored value is now an Alias.
pub const Scope = struct {
    source: enum { configured, inferred },
    display_id: []u8,
    title: []u8,

    /// Free the resolved Display ID and title. Held by the caller in the
    /// switch arm that receives a `.scope` outcome.
    pub fn deinit(self: Scope, gpa: std.mem.Allocator) void {
        gpa.free(self.display_id);
        gpa.free(self.title);
    }
};

/// Raw discovery inputs handed off from the git-side reader.
///
/// `configured_value` is whatever `git config --worktree --get tk.scope`
/// returned (null when the key is unset or the extension is disabled).
/// `branch_name` is the current branch's short ref (null on detached HEAD).
/// Fields are `[]const u8` so tests can construct `Raw` from string literals
/// without an allocator; `readGitSide` returns a `Raw` whose populated slices
/// are gpa-owned and freed through `freeRaw`.
pub const Raw = struct {
    configured_value: ?[]const u8 = null,
    branch_name: ?[]const u8 = null,
};

/// Free populated slices on a `Raw` returned by `readGitSide`. Safe on an
/// all-null `Raw`. Do not call on a literal-built `Raw` from tests.
pub fn freeRaw(gpa: std.mem.Allocator, raw: Raw) void {
    if (raw.configured_value) |s| gpa.free(s);
    if (raw.branch_name) |s| gpa.free(s);
}

/// Outcome of resolving raw discovery against the Repository Store.
///
/// Per AGENTS.md "Error Handling": each switch arm frees its own payload.
/// There is intentionally no `Outcome.deinit` because cleanup is asymmetric
/// across variants.
pub const ResolveOutcome = union(enum) {
    none,
    scope: Scope,
    /// `tk.scope` held a value the resolver could not find. Carried as a
    /// gpa-owned slice so callers can name it in a diagnostic before freeing.
    configured_unresolved: []u8,
};

pub const ResolveError = repository.ResolveError;

/// Read the two git-side inputs needed to resolve Workspace Scope.
///
/// Runs `git config --worktree --get tk.scope` and `git symbolic-ref --short
/// HEAD` through the supplied runner. Any non-zero git exit (key absent,
/// `extensions.worktreeConfig` disabled, detached HEAD, git binary missing,
/// spawn failure) collapses the corresponding slot to `null`. Only
/// `error.OutOfMemory` escapes; everything else folds into the "no info"
/// shape so callers above this seam render scope as `.none` instead of
/// distinguishing many failure modes that all mean the same thing for
/// downstream consumers.
///
/// Populated slices in the returned `Raw` are gpa-owned; free them with
/// `freeRaw`.
pub fn readGitSide(
    gpa: std.mem.Allocator,
    runner: proc.Runner,
    cwd: std.Io.Dir,
) error{OutOfMemory}!Raw {
    const configured = try readSingleLine(
        gpa,
        runner,
        cwd,
        &.{ "git", "config", "--worktree", "--get", "tk.scope" },
    );
    errdefer if (configured) |s| gpa.free(s);
    const branch = try readSingleLine(
        gpa,
        runner,
        cwd,
        &.{ "git", "symbolic-ref", "--short", "HEAD" },
    );
    return .{ .configured_value = configured, .branch_name = branch };
}

/// Run a git subprocess and return the trimmed stdout when it exits 0.
///
/// Any failure to spawn, non-zero exit, or empty trimmed stdout returns
/// `null` so caller can collapse it into "no info" without distinguishing
/// failure shapes.
fn readSingleLine(
    gpa: std.mem.Allocator,
    runner: proc.Runner,
    cwd: std.Io.Dir,
    argv: []const []const u8,
) error{OutOfMemory}!?[]const u8 {
    var result = runner.run(gpa, .{ .argv = argv, .cwd = cwd }) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.ExecutableNotFound, error.SpawnFailed => return null,
    };
    defer result.deinit(gpa);

    if (result.exit_code != 0) return null;
    const trimmed = std.mem.trim(u8, result.stdout, " \t\r\n");
    if (trimmed.len == 0) return null;
    return try gpa.dupe(u8, trimmed);
}

/// Resolve raw discovery output against the Repository Store.
///
/// Configured scope is checked first per CONTEXT.md ("Worktree Config scope
/// takes precedence over Inferred Workspace Scope"). When the stored value
/// resolves through `item_ids`, the returned `display_id` is the *current*
/// Display ID, not the stored string. A stored value that no longer
/// resolves surfaces as `.configured_unresolved` so callers can render
/// `tk worktree: Workspace Scope '<stored>' is not a known Display ID or
/// Alias` (per docs/implementation.md "Worktrees").
pub fn resolveAgainstStore(
    store: repository.Store,
    gpa: std.mem.Allocator,
    raw: Raw,
) ResolveError!ResolveOutcome {
    if (raw.configured_value) |stored| {
        if (try repository.resolveItemRef(store, gpa, stored)) |resolved| {
            defer resolved.deinit(gpa);
            return .{ .scope = try loadScope(store, gpa, resolved.id, .configured) };
        }
        return .{ .configured_unresolved = try gpa.dupe(u8, stored) };
    }
    if (raw.branch_name) |branch| {
        if (std.mem.startsWith(u8, branch, ticket_branch_prefix)) {
            const tail = branch[ticket_branch_prefix.len..];
            if (try longestPrefixMatch(store, gpa, tail)) |item_id| {
                defer gpa.free(item_id);
                return .{ .scope = try loadScope(store, gpa, item_id, .inferred) };
            }
        }
    }
    return .none;
}

fn loadScope(
    store: repository.Store,
    gpa: std.mem.Allocator,
    item_id: []const u8,
    source: @FieldType(Scope, "source"),
) ResolveError!Scope {
    const row = (try store.conn.row(
        "select display_value, title from items where id = ?1",
        .{item_id},
    )) orelse unreachable;
    defer row.deinit();
    const display_id = try gpa.dupe(u8, row.text(0));
    errdefer gpa.free(display_id);
    const title = try gpa.dupe(u8, row.text(1));
    return .{ .source = source, .display_id = display_id, .title = title };
}

const ticket_branch_prefix = "tk/";

/// Longest stored Display ID or Alias that is a `-`-bounded prefix of the
/// branch tail. The `-` boundary keeps `proj` from silently shadowing
/// `tk/project-1`. `collate nocase` matches the `item_ids` PK collation.
const longest_prefix_match_sql =
    \\select item_id
    \\  from item_ids
    \\ where (?1 = value collate nocase
    \\        or ?1 like value || '-%' collate nocase)
    \\ order by length(value) desc
    \\ limit 1
;

fn longestPrefixMatch(
    store: repository.Store,
    gpa: std.mem.Allocator,
    tail: []const u8,
) ResolveError!?[]u8 {
    if (tail.len == 0) return null;
    if (try store.conn.row(longest_prefix_match_sql, .{tail})) |r| {
        defer r.deinit();
        return try gpa.dupe(u8, r.text(0));
    }
    return null;
}

/// Sanitize a Ticket/Epic title into a slug for a git ref or filesystem path
/// component.
///
/// Replaces every maximal run of characters outside `[a-z0-9]` with a single
/// `-`, trims leading and trailing `-`, and truncates the result to `max_len`
/// characters at the last `-` boundary that fits. Returns the empty slice
/// when the input contains no `[a-z0-9]` characters after lowercasing.
///
/// Caller owns the returned slice and frees it through `gpa`.
pub fn sanitize(gpa: std.mem.Allocator, title: []const u8, max_len: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(gpa);

    var prev_dash = false;
    for (title) |c| {
        const lower = std.ascii.toLower(c);
        if (isSlugByte(lower)) {
            try out.append(gpa, lower);
            prev_dash = false;
        } else if (!prev_dash and out.items.len > 0) {
            try out.append(gpa, '-');
            prev_dash = true;
        }
    }
    if (out.items.len > 0 and out.items[out.items.len - 1] == '-') {
        _ = out.pop();
    }

    if (out.items.len > max_len) {
        // Truncate at the last `-` boundary at or before `max_len`. If none,
        // hard truncate to `max_len`. Then trim any trailing `-` so the
        // result never ends in a dash.
        var cut: usize = max_len;
        while (cut > 0 and out.items[cut] != '-') : (cut -= 1) {}
        if (cut == 0) cut = max_len;
        out.shrinkRetainingCapacity(cut);
        if (out.items.len > 0 and out.items[out.items.len - 1] == '-') {
            _ = out.pop();
        }
    }
    return try out.toOwnedSlice(gpa);
}

fn isSlugByte(c: u8) bool {
    return (c >= 'a' and c <= 'z') or (c >= '0' and c <= '9');
}

test "readGitSide: returns trimmed configured value when git config exits 0" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{
        .exit_code = 0,
        .stdout = "proj-1\n",
    });
    try fake.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 1 });

    const raw = try readGitSide(gpa, fake.runner(), std.Io.Dir.cwd());
    defer freeRaw(gpa, raw);
    try std.testing.expectEqualStrings("proj-1", raw.configured_value.?);
    try std.testing.expect(raw.branch_name == null);
}

test "readGitSide: returns trimmed branch name when symbolic-ref exits 0" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try fake.expect(&.{ "git", "symbolic-ref" }, .{
        .exit_code = 0,
        .stdout = "tk/proj-1-fix-login\n",
    });

    const raw = try readGitSide(gpa, fake.runner(), std.Io.Dir.cwd());
    defer freeRaw(gpa, raw);
    try std.testing.expect(raw.configured_value == null);
    try std.testing.expectEqualStrings("tk/proj-1-fix-login", raw.branch_name.?);
}

test "readGitSide: detached HEAD and unset key collapse to all-null Raw" {
    const gpa = std.testing.allocator;
    var fake = fake_proc.FakeRunner.init(gpa);
    defer fake.deinit();
    try fake.expect(&.{ "git", "config", "--worktree", "--get", "tk.scope" }, .{ .exit_code = 1 });
    try fake.expect(&.{ "git", "symbolic-ref" }, .{ .exit_code = 128 });

    const raw = try readGitSide(gpa, fake.runner(), std.Io.Dir.cwd());
    defer freeRaw(gpa, raw);
    try std.testing.expect(raw.configured_value == null);
    try std.testing.expect(raw.branch_name == null);
}

test "readGitSide: git missing collapses to all-null Raw" {
    const gpa = std.testing.allocator;
    var injector = fake_proc.ErrorInjectingRunner{ .err = error.ExecutableNotFound };

    const raw = try readGitSide(gpa, injector.runner(), std.Io.Dir.cwd());
    defer freeRaw(gpa, raw);
    try std.testing.expect(raw.configured_value == null);
    try std.testing.expect(raw.branch_name == null);
}

test "resolveAgainstStore: configured value resolves to its current Display ID" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "abc123",
        .display = "proj-1",
        .title = "Implement worktree scope",
        .created_seq = 1,
    });

    const outcome = try resolveAgainstStore(store, gpa, .{ .configured_value = "proj-1" });
    switch (outcome) {
        .scope => |s| {
            defer s.deinit(gpa);
            try std.testing.expectEqual(@as(@TypeOf(s.source), .configured), s.source);
            try std.testing.expectEqualStrings("proj-1", s.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "resolveAgainstStore: configured value that no longer resolves surfaces as .configured_unresolved" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    const outcome = try resolveAgainstStore(store, gpa, .{ .configured_value = "ghost-42" });
    switch (outcome) {
        .configured_unresolved => |stored| {
            defer gpa.free(stored);
            try std.testing.expectEqualStrings("ghost-42", stored);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "resolveAgainstStore: empty raw returns .none" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    const outcome = try resolveAgainstStore(store, gpa, .{});
    try std.testing.expect(outcome == .none);
}

test "resolveAgainstStore: branch matching tk/<id>-<slug> resolves as inferred scope" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "abc123",
        .display = "proj-1",
        .title = "Fix login",
        .created_seq = 1,
    });

    const outcome = try resolveAgainstStore(store, gpa, .{ .branch_name = "tk/proj-1-fix-login" });
    switch (outcome) {
        .scope => |s| {
            defer s.deinit(gpa);
            try std.testing.expectEqual(@as(@TypeOf(s.source), .inferred), s.source);
            try std.testing.expectEqualStrings("proj-1", s.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "resolveAgainstStore: branch without tk/ prefix does not infer scope" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{
        .id = "abc123",
        .display = "proj-1",
        .title = "Fix login",
        .created_seq = 1,
    });

    // Branch literally named `proj-1` (no `tk/` prefix) must not infer scope.
    const outcome = try resolveAgainstStore(store, gpa, .{ .branch_name = "proj-1" });
    try std.testing.expect(outcome == .none);
}

test "resolveAgainstStore: longest matching item_ids.value wins when slug shadows a shorter ID" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    // Both `proj-1` and `proj-1-fix` exist as Display IDs. Branch
    // `tk/proj-1-fix-login` must resolve to the longer match.
    try TmpStore.insertFixtureItem(conn, .{ .id = "short", .display = "proj-1", .title = "Short", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "long", .display = "proj-1-fix", .title = "Long", .created_seq = 2 });

    const outcome = try resolveAgainstStore(store, gpa, .{ .branch_name = "tk/proj-1-fix-login" });
    switch (outcome) {
        .scope => |s| {
            defer s.deinit(gpa);
            try std.testing.expectEqualStrings("proj-1-fix", s.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "resolveAgainstStore: configured scope takes precedence over branch inference" {
    const gpa = std.testing.allocator;
    const conn = try zqlite.open(":memory:", zqlite.OpenFlags.Create | zqlite.OpenFlags.EXResCode);
    defer conn.close();
    try conn.execNoArgs("pragma foreign_keys = on");
    try migrations.applyAll(conn, "2026-05-09T00:00:00.000Z", null);
    const store: repository.Store = .{ .conn = conn };

    try TmpStore.insertFixtureItem(conn, .{ .id = "configured", .display = "proj-1", .title = "Configured", .created_seq = 1 });
    try TmpStore.insertFixtureItem(conn, .{ .id = "inferred", .display = "proj-2", .title = "Inferred", .created_seq = 2 });

    const outcome = try resolveAgainstStore(store, gpa, .{
        .configured_value = "proj-1",
        .branch_name = "tk/proj-2-anything",
    });
    switch (outcome) {
        .scope => |s| {
            defer s.deinit(gpa);
            try std.testing.expectEqual(@as(@TypeOf(s.source), .configured), s.source);
            try std.testing.expectEqualStrings("proj-1", s.display_id);
        },
        else => return error.UnexpectedOutcome,
    }
}

test "sanitize: lowercase ASCII alphanumeric and hyphens pass through unchanged" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "fix-login-bug", 40);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix-login-bug", result);
}

test "sanitize: uppercase letters lowercase and spaces become single hyphen" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "Fix Login", 40);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix-login", result);
}

test "sanitize: truncates at the last hyphen boundary that fits within max_len" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "fix login bug", 8);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("fix", result);
}

test "sanitize: hard truncates a single long word when no hyphen boundary fits" {
    const gpa = std.testing.allocator;
    const result = try sanitize(gpa, "antidisestablishmentarianism", 8);
    defer gpa.free(result);
    try std.testing.expectEqualStrings("antidise", result);
}
