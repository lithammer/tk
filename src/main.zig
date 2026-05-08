const std = @import("std");
const cli = @import("cli.zig");
const proc = @import("proc/runner.zig");
const clock_mod = @import("clock.zig");

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    var stdout_buf: [4096]u8 = undefined;
    var stderr_buf: [1024]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(io, &stdout_buf);
    var stderr = std.Io.File.stderr().writer(io, &stderr_buf);

    var real_runner = proc.RealRunner.init(io);
    var real_clock = clock_mod.RealClock.init(io);

    const deps = cli.Deps{
        .stdout = &stdout.interface,
        .stderr = &stderr.interface,
        .gpa = init.gpa,
        .cwd = std.Io.Dir.cwd(),
        .runner = real_runner.runner(),
        .clock = real_clock.clock(),
    };

    var code = run(deps, init) catch |err| blk: {
        deps.stderr.print("internal error: {s}\n", .{@errorName(err)}) catch {};
        break :blk @as(u8, 3);
    };
    stdout.interface.flush() catch {
        if (code == 0) code = 3;
    };
    stderr.interface.flush() catch {};
    std.process.exit(code);
}

fn run(deps: cli.Deps, init: std.process.Init) !u8 {
    var args_iter = try init.minimal.args.iterateAllocator(init.gpa);
    defer args_iter.deinit();
    _ = args_iter.next();
    return try cli.runArgv(deps, &args_iter);
}
