//! Captured-output subprocess runner.
//!
//! Commands invoke external CLIs (e.g. `git rev-parse`) through this
//! abstraction so tests can substitute a `FakeRunner` that returns scripted
//! results without spawning real processes.

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const Result = struct {
    /// 0..255 for processes that exited normally; 255 for signal/unknown.
    exit_code: u8,
    /// Captured stdout. Owned by `gpa` from the call to `run`.
    stdout: []u8,
    /// Captured stderr. Owned by `gpa` from the call to `run`.
    stderr: []u8,

    pub fn deinit(self: *Result, gpa: Allocator) void {
        gpa.free(self.stdout);
        gpa.free(self.stderr);
    }
};

pub const Options = struct {
    argv: []const []const u8,
    cwd: ?std.Io.Dir = null,
};

pub const Error = error{
    SpawnFailed,
    OutOfMemory,
};

pub const Runner = struct {
    context: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        run: *const fn (context: *anyopaque, gpa: Allocator, options: Options) Error!Result,
    };

    pub fn run(self: Runner, gpa: Allocator, options: Options) Error!Result {
        return self.vtable.run(self.context, gpa, options);
    }
};

/// Real runner backed by `std.process.run`.
pub const RealRunner = struct {
    io: std.Io,

    pub fn init(io: std.Io) RealRunner {
        return .{ .io = io };
    }

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
            else => return error.SpawnFailed,
        };

        const exit_code: u8 = switch (result.term) {
            .exited => |c| c,
            else => 255,
        };

        return .{
            .exit_code = exit_code,
            .stdout = result.stdout,
            .stderr = result.stderr,
        };
    }
};
