//! Subprocess smoke tests against the linked `tk` executable.
//!
//! These tests build the actual binary (via build-options wiring in
//! `build.zig`) and exec it with stdout/stderr captured. They are the only
//! end-to-end check that argv plumbing, embed wiring, the SQLite link, and
//! Git subprocess discovery all line up. In-process scenarios cannot detect
//! a regression in any of those because the test binary embeds the same
//! bytes via the same module imports.

const std = @import("std");
const build_options = @import("build_options");
const messages = @import("../messages.zig");

const tk_exe_path: []const u8 = build_options.tk_exe_path;

/// `build_options.tk_exe_path` is the cache-relative path emitted by the
/// build system. The smoke tests `cd` into a temp directory before spawning
/// the binary, so the relative path becomes meaningless. Resolve it to an
/// absolute path once at test start. Caller frees with the same allocator.
fn absoluteTkPath(gpa: std.mem.Allocator) ![]u8 {
    if (std.fs.path.isAbsolute(tk_exe_path)) return gpa.dupe(u8, tk_exe_path);
    var buf: [std.Io.Dir.max_path_bytes]u8 = undefined;
    const n = try std.Io.Dir.cwd().realPathFile(std.testing.io, tk_exe_path, &buf);
    return gpa.dupe(u8, buf[0..n]);
}

fn runProcess(gpa: std.mem.Allocator, argv: []const []const u8, cwd: std.Io.Dir) !std.process.RunResult {
    return std.process.run(gpa, std.testing.io, .{
        .argv = argv,
        .cwd = .{ .dir = cwd },
    });
}

fn freeRunResult(gpa: std.mem.Allocator, r: std.process.RunResult) void {
    gpa.free(r.stdout);
    gpa.free(r.stderr);
}

test "smoke: tk init in a real git repo creates the store" {
    const gpa = std.testing.allocator;

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", gpa);
    defer gpa.free(root);

    const tk_abs = try absoluteTkPath(gpa);
    defer gpa.free(tk_abs);

    // git init
    const git_init = try runProcess(gpa, &.{ "git", "init", "-q" }, tmp.dir);
    defer freeRunResult(gpa, git_init);
    try std.testing.expectEqual(@as(std.process.Child.Term, .{ .exited = 0 }), git_init.term);

    // tk init
    const tk_init = try runProcess(gpa, &.{ tk_abs, "init" }, tmp.dir);
    defer freeRunResult(gpa, tk_init);
    if (!std.meta.eql(tk_init.term, std.process.Child.Term{ .exited = 0 })) {
        std.debug.print("\ntk init unexpected exit {any}\nstdout:\n{s}\nstderr:\n{s}\n", .{ tk_init.term, tk_init.stdout, tk_init.stderr });
        return error.SmokeInitFailed;
    }
    try std.testing.expect(std.mem.indexOf(u8, tk_init.stdout, messages.init_success_fresh) != null);

    // Verify the file is on disk and looks like a SQLite database.
    const db_path = try std.fs.path.joinZ(gpa, &.{ root, ".git", "tk", "ticket.db" });
    defer gpa.free(db_path);

    var file = try std.Io.Dir.cwd().openFile(std.testing.io, db_path, .{});
    defer file.close(std.testing.io);
    var header: [16]u8 = undefined;
    var read_buf: [16]u8 = undefined;
    var reader = file.reader(std.testing.io, &read_buf);
    try reader.interface.readSliceAll(header[0..]);
    // SQLite database header magic is "SQLite format 3\x00" (16 bytes).
    try std.testing.expectEqualStrings("SQLite format 3\x00", &header);
}

test "smoke: tk add creates a local Ticket from a message file" {
    const gpa = std.testing.allocator;

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "project");
    var project = try tmp.dir.openDir(std.testing.io, "project", .{});
    defer project.close(std.testing.io);

    const tk_abs = try absoluteTkPath(gpa);
    defer gpa.free(tk_abs);

    const git_init = try runProcess(gpa, &.{ "git", "init", "-q" }, project);
    defer freeRunResult(gpa, git_init);
    try std.testing.expectEqual(@as(std.process.Child.Term, .{ .exited = 0 }), git_init.term);

    const tk_init = try runProcess(gpa, &.{ tk_abs, "init" }, project);
    defer freeRunResult(gpa, tk_init);
    try std.testing.expectEqual(@as(std.process.Child.Term, .{ .exited = 0 }), tk_init.term);

    try project.writeFile(std.testing.io, .{
        .sub_path = "followup.md",
        .data =
        \\Investigate flaky login retry
        \\
        \\Repro is intermittent on the staging cluster.
        \\First seen 2026-04-30.
        \\
        ,
    });

    const tk_add = try runProcess(gpa, &.{ tk_abs, "add", "-F", "followup.md" }, project);
    defer freeRunResult(gpa, tk_add);
    if (!std.meta.eql(tk_add.term, std.process.Child.Term{ .exited = 0 })) {
        std.debug.print("\ntk add unexpected exit {any}\nstdout:\n{s}\nstderr:\n{s}\n", .{ tk_add.term, tk_add.stdout, tk_add.stderr });
        return error.SmokeAddFailed;
    }
    try std.testing.expect(std.mem.indexOf(u8, tk_add.stdout, "Created Ticket: project-1 - Investigate flaky login retry\n") != null);
    try std.testing.expect(std.mem.indexOf(u8, tk_add.stdout, "Priority: P2\n") != null);
    try std.testing.expect(std.mem.indexOf(u8, tk_add.stdout, "Status: open\n") != null);
    try std.testing.expectEqualStrings("", tk_add.stderr);

    const tk_list = try runProcess(gpa, &.{ tk_abs, "list" }, project);
    defer freeRunResult(gpa, tk_list);
    if (!std.meta.eql(tk_list.term, std.process.Child.Term{ .exited = 0 })) {
        std.debug.print("\ntk list unexpected exit {any}\nstdout:\n{s}\nstderr:\n{s}\n", .{ tk_list.term, tk_list.stdout, tk_list.stderr });
        return error.SmokeListFailed;
    }
    try std.testing.expect(std.mem.indexOf(u8, tk_list.stdout, "○ project-1 ● P2 Investigate flaky login retry\n") != null);
    try std.testing.expect(std.mem.indexOf(u8, tk_list.stdout, "Total: 1 item (1 open)\n") != null);
    try std.testing.expectEqualStrings("", tk_list.stderr);

    const tk_next = try runProcess(gpa, &.{ tk_abs, "next" }, project);
    defer freeRunResult(gpa, tk_next);
    if (!std.meta.eql(tk_next.term, std.process.Child.Term{ .exited = 0 })) {
        std.debug.print("\ntk next unexpected exit {any}\nstdout:\n{s}\nstderr:\n{s}\n", .{ tk_next.term, tk_next.stdout, tk_next.stderr });
        return error.SmokeNextFailed;
    }
    try std.testing.expectEqualStrings("project-1\n", tk_next.stdout);
    try std.testing.expectEqualStrings("", tk_next.stderr);
}

// Note: we intentionally do not have a subprocess smoke test for the
// outside-git path. It is exercised by the unit test in commands/init.zig
// (which uses the fake runner), and engineering a reliably git-free temp
// directory inside a checkout is awkward. The real-git success path here
// is the only check that exercises argv plumbing, Git subprocess discovery,
// filesystem writes, and SQLite linkage end-to-end.
