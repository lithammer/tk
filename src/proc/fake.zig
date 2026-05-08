//! Fake `Runner` for tests. Matches argv prefixes against scripted responses.

const std = @import("std");
const runner_mod = @import("runner.zig");
const Allocator = std.mem.Allocator;

const Runner = runner_mod.Runner;
const Options = runner_mod.Options;
const Result = runner_mod.Result;

pub const Response = struct {
    exit_code: u8 = 0,
    stdout: []const u8 = "",
    stderr: []const u8 = "",
};

const Expectation = struct {
    argv_prefix: []const []const u8,
    response: Response,
};

pub const FakeRunner = struct {
    gpa: Allocator,
    expectations: std.ArrayList(Expectation),
    /// If true, an unmatched call returns a synthesized failure (exit 127,
    /// stderr noting the missing expectation). If false, the call returns the
    /// `default` response.
    strict: bool = true,
    default: Response = .{ .exit_code = 127, .stderr = "FakeRunner: no expectation matched\n" },
    /// Recorded calls, in order. Useful for asserting the runner saw the
    /// argv we expected.
    calls: std.ArrayList([]const []const u8),

    pub fn init(gpa: Allocator) FakeRunner {
        return .{
            .gpa = gpa,
            .expectations = .empty,
            .calls = .empty,
        };
    }

    pub fn deinit(self: *FakeRunner) void {
        for (self.expectations.items) |exp| {
            for (exp.argv_prefix) |s| self.gpa.free(s);
            self.gpa.free(exp.argv_prefix);
        }
        self.expectations.deinit(self.gpa);

        for (self.calls.items) |call| {
            for (call) |s| self.gpa.free(s);
            self.gpa.free(call);
        }
        self.calls.deinit(self.gpa);
    }

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

    pub fn runner(self: *FakeRunner) Runner {
        return .{ .context = self, .vtable = &vtable };
    }

    const vtable: Runner.VTable = .{ .run = runImpl };

    fn runImpl(context: *anyopaque, gpa: Allocator, options: Options) runner_mod.Error!Result {
        const self: *FakeRunner = @ptrCast(@alignCast(context));

        const recorded = gpa.alloc([]const u8, options.argv.len) catch return error.OutOfMemory;
        var rec_i: usize = 0;
        errdefer {
            for (recorded[0..rec_i]) |s| gpa.free(s);
            gpa.free(recorded);
        }
        while (rec_i < options.argv.len) : (rec_i += 1) {
            recorded[rec_i] = gpa.dupe(u8, options.argv[rec_i]) catch return error.OutOfMemory;
        }
        self.calls.append(self.gpa, recorded) catch return error.OutOfMemory;

        const response: Response = matchPrefix: {
            for (self.expectations.items) |exp| {
                if (matchesPrefix(options.argv, exp.argv_prefix)) break :matchPrefix exp.response;
            }
            if (self.strict) {
                break :matchPrefix self.default;
            }
            break :matchPrefix self.default;
        };

        const stdout = gpa.dupe(u8, response.stdout) catch return error.OutOfMemory;
        errdefer gpa.free(stdout);
        const stderr = gpa.dupe(u8, response.stderr) catch return error.OutOfMemory;
        return .{
            .exit_code = response.exit_code,
            .stdout = stdout,
            .stderr = stderr,
        };
    }

    fn matchesPrefix(argv: []const []const u8, prefix: []const []const u8) bool {
        if (argv.len < prefix.len) return false;
        for (prefix, 0..) |want, i| {
            if (!std.mem.eql(u8, want, argv[i])) return false;
        }
        return true;
    }
};
