//! Captured-output subprocess runner.
//!
//! Commands invoke external CLIs (e.g. `git rev-parse`) through this
//! abstraction so tests can substitute a `FakeRunner` that returns scripted
//! results without spawning real processes.

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Captured result of an external CLI invocation.
pub const Result = struct {
    /// 0..255 for processes that exited normally; 255 for signal/unknown.
    exit_code: u8,
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
