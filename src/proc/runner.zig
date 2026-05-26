//! Captured-output subprocess runner.
//!
//! Commands invoke external CLIs (e.g. `git rev-parse`) through this
//! abstraction so tests can substitute a `FakeRunner` that returns scripted
//! results without spawning real processes.

const std = @import("std");
const Allocator = std.mem.Allocator;

const platform = @import("../platform.zig");

/// How an external CLI invocation ended.
pub const Exit = union(enum) {
    /// The child process exited normally with this status code.
    exited: u8,
    /// The child process was terminated by a signal.
    signal: u32,
    /// The child process was stopped by a signal.
    stopped: u32,
    /// The platform reported an unclassified termination status.
    unknown: u32,

    /// Return the status code for a normal process exit.
    pub fn code(self: Exit) ?u8 {
        return switch (self) {
            .exited => |status| status,
            .signal, .stopped, .unknown => null,
        };
    }

    /// Write the stable user-facing representation of the exit mode.
    pub fn format(self: Exit, writer: *std.Io.Writer) !void {
        switch (self) {
            .exited => |status| try writer.print("exit {d}", .{status}),
            .signal => |sig| try writer.print("signal {d}", .{sig}),
            .stopped => |sig| try writer.print("stopped {d}", .{sig}),
            .unknown => |status| try writer.print("unknown {d}", .{status}),
        }
    }
};

/// Captured result of an external CLI invocation.
pub const Result = struct {
    /// Typed process exit outcome.
    exit: Exit,
    /// Captured stdout. Owned by `gpa` from the call to `run`.
    stdout: []u8,
    /// Captured stderr. Owned by `gpa` from the call to `run`.
    stderr: []u8,

    /// Free captured stdout and stderr with the allocator passed to `run`.
    pub fn deinit(self: *Result, gpa: Allocator) void {
        gpa.free(self.stdout);
        gpa.free(self.stderr);
    }
};

test "RealRunner reports normal process exit as typed exit" {
    try platform.skipOnWindows();

    const gpa = std.testing.allocator;

    var runner = RealRunner.init(std.testing.io);
    var result = try runner.runner().run(gpa, .{
        .argv = &.{ "/bin/sh", "-c", "printf stdout; printf stderr >&2; exit 7" },
    });
    defer result.deinit(gpa);

    try std.testing.expectEqual(@as(Exit, .{ .exited = 7 }), result.exit);
    try std.testing.expectEqualStrings("stdout", result.stdout);
    try std.testing.expectEqualStrings("stderr", result.stderr);
}

test "RealRunner reports signal termination as typed exit" {
    try platform.skipOnWindows();

    const gpa = std.testing.allocator;

    var runner = RealRunner.init(std.testing.io);
    var result = try runner.runner().run(gpa, .{
        .argv = &.{ "/bin/sh", "-c", "kill -TERM $$" },
    });
    defer result.deinit(gpa);

    try std.testing.expectEqual(@as(Exit, .{ .signal = @intFromEnum(std.posix.SIG.TERM) }), result.exit);
}

/// Subprocess request issued by a command handler.
pub const Options = struct {
    /// Full argv, including the executable name at index 0.
    argv: []const []const u8,
    /// Optional working directory for the child process.
    cwd: ?std.Io.Dir = null,
};

/// Error set exposed to command handlers by any runner implementation.
pub const Error = error{
    /// argv[0] could not be located on PATH or at the absolute path given.
    /// Distinguished from generic spawn failures so commands can render an
    /// install-this-tool diagnostic instead of a retry hint.
    ExecutableNotFound,
    /// Catch-all for any other failure to spawn or wait for the child.
    SpawnFailed,
    OutOfMemory,
};

/// Type-erased subprocess runner.
///
/// Commands use this through `cli.Deps` so tests can script subprocess output
/// while production code still uses `RealRunner`.
pub const Runner = struct {
    /// Implementation-owned pointer passed back to the vtable.
    context: *anyopaque,
    /// Runner operations for the concrete implementation behind `context`.
    vtable: *const VTable,

    /// Runner implementation hooks.
    pub const VTable = struct {
        run: *const fn (context: *anyopaque, gpa: Allocator, options: Options) Error!Result,
    };

    /// Run a subprocess and capture stdout/stderr.
    pub fn run(self: Runner, gpa: Allocator, options: Options) Error!Result {
        return self.vtable.run(self.context, gpa, options);
    }
};

/// Real runner backed by `std.process.run`.
///
/// Zig 0.16's POSIX spawn path performs argv/env sentinel allocation before
/// `fork` and reports child-side setup/exec failures through a pipe, so tk's
/// runner keeps the Repository Store and Backend Adapter subprocess boundary
/// fork-safe without adding a second custom process launcher.
pub const RealRunner = struct {
    io: std.Io,

    /// Bind the runner to the process I/O handle supplied by Zig's main.
    pub fn init(io: std.Io) RealRunner {
        return .{ .io = io };
    }

    /// Return the type-erased runner view passed through `cli.Deps`.
    pub fn runner(self: *RealRunner) Runner {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Runner.VTable = .{ .run = runImpl };

    fn runImpl(context: *anyopaque, gpa: Allocator, options: Options) Error!Result {
        const self: *RealRunner = @ptrCast(@alignCast(context));
        const cwd: std.process.Child.Cwd = if (options.cwd) |dir| .{ .dir = dir } else .inherit;

        const result = std.process.run(gpa, self.io, .{
            .argv = options.argv,
            .cwd = cwd,
        }) catch |err| switch (err) {
            error.OutOfMemory => return error.OutOfMemory,
            error.FileNotFound => return error.ExecutableNotFound,
            else => return error.SpawnFailed,
        };

        return .{
            .exit = exitFromTerm(result.term),
            .stdout = result.stdout,
            .stderr = result.stderr,
        };
    }
};

fn exitFromTerm(term: std.process.Child.Term) Exit {
    return switch (term) {
        .exited => |code| .{ .exited = code },
        .signal => |sig| .{ .signal = @intFromEnum(sig) },
        .stopped => |sig| .{ .stopped = @intFromEnum(sig) },
        .unknown => |status| .{ .unknown = status },
    };
}
