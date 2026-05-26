//! Fake `Runner` for tests. Matches argv prefixes against scripted responses.

const std = @import("std");
const runner_mod = @import("runner.zig");
const Allocator = std.mem.Allocator;

const Runner = runner_mod.Runner;
const Options = runner_mod.Options;
const Result = runner_mod.Result;

/// Scripted subprocess response returned by `FakeRunner`.
pub const Response = struct {
    /// Process exit code reported to the command under test.
    exit_code: u8 = 0,
    /// Captured stdout payload.
    stdout: []const u8 = "",
    /// Captured stderr payload.
    stderr: []const u8 = "",
};

const Expectation = struct {
    argv_prefix: []const []const u8,
    response: Response,
};

/// Strict fake subprocess runner for command-handler tests.
///
/// Expectations match argv prefixes in insertion order. An unmatched call
/// panics because it means the test forgot to declare an expected subprocess
/// interaction.
pub const FakeRunner = struct {
    gpa: Allocator,
    expectations: std.ArrayList(Expectation),

    /// Create an empty fake runner.
    pub fn init(gpa: Allocator) FakeRunner {
        return .{ .gpa = gpa, .expectations = .empty };
    }

    /// Free copied expectation argv prefixes.
    pub fn deinit(self: *FakeRunner) void {
        for (self.expectations.items) |exp| {
            for (exp.argv_prefix) |s| self.gpa.free(s);
            self.gpa.free(exp.argv_prefix);
        }
        self.expectations.deinit(self.gpa);
    }

    /// Add an argv-prefix expectation and its response.
    pub fn expect(self: *FakeRunner, argv_prefix: []const []const u8, response: Response) !void {
        const owned_prefix = try self.gpa.alloc([]const u8, argv_prefix.len);
        errdefer self.gpa.free(owned_prefix);
        var i: usize = 0;
        errdefer for (owned_prefix[0..i]) |s| self.gpa.free(s);
        while (i < argv_prefix.len) : (i += 1) {
            owned_prefix[i] = try self.gpa.dupe(u8, argv_prefix[i]);
        }
        try self.expectations.append(self.gpa, .{ .argv_prefix = owned_prefix, .response = response });
    }

    /// Return the type-erased runner view passed through `cli.Deps`.
    pub fn runner(self: *FakeRunner) Runner {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Runner.VTable = .{ .run = runImpl };

    fn runImpl(context: *anyopaque, gpa: Allocator, options: Options) runner_mod.Error!Result {
        const self: *FakeRunner = @ptrCast(@alignCast(context));

        for (self.expectations.items) |exp| {
            if (matchesPrefix(options.argv, exp.argv_prefix)) {
                const stdout = gpa.dupe(u8, exp.response.stdout) catch return error.OutOfMemory;
                errdefer gpa.free(stdout);
                const stderr = gpa.dupe(u8, exp.response.stderr) catch return error.OutOfMemory;
                return .{ .exit = .{ .exited = exp.response.exit_code }, .stdout = stdout, .stderr = stderr };
            }
        }

        // Strict mode: an unmatched call is a test bug, not a runtime
        // condition. Panic loudly so the offending test fails immediately
        // instead of silently receiving exit 127.
        std.debug.print("FakeRunner: no expectation matched argv:", .{});
        for (options.argv) |arg| std.debug.print(" {s}", .{arg});
        std.debug.print("\n", .{});
        @panic("FakeRunner: unexpected subprocess call");
    }

    fn matchesPrefix(argv: []const []const u8, prefix: []const []const u8) bool {
        if (argv.len < prefix.len) return false;
        for (prefix, 0..) |want, i| {
            if (!std.mem.eql(u8, want, argv[i])) return false;
        }
        return true;
    }
};

/// Hand-rolled subprocess runner that returns a fixed `runner.Error` on every
/// call. Companion to `FakeRunner`, which can only return `Result`-shaped
/// responses; this helper covers tests that need to exercise the
/// runner-error mapping (`ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`).
/// Argv and cwd are ignored.
pub const ErrorInjectingRunner = struct {
    /// Error returned by every `run` invocation.
    err: runner_mod.Error,

    /// Return the type-erased runner view passed through `cli.Deps` or to
    /// callees that take a `proc.Runner` directly.
    pub fn runner(self: *ErrorInjectingRunner) Runner {
        return .{ .context = self, .vtable = &error_vtable };
    }

    const error_vtable: Runner.VTable = .{ .run = errorRunImpl };

    fn errorRunImpl(context: *anyopaque, gpa: Allocator, options: Options) runner_mod.Error!Result {
        _ = gpa;
        _ = options;
        const self: *ErrorInjectingRunner = @ptrCast(@alignCast(context));
        return self.err;
    }
};
