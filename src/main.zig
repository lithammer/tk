const std = @import("std");
const build_options = @import("build_options");
const cli = @import("cli.zig");
const http_mod = @import("http/client.zig");
const proc = @import("proc/runner.zig");
const clock_mod = @import("clock.zig");
const render = @import("render/styler.zig");

/// User-Agent string sent on every HTTP request the real client makes.
/// Encodes the binary's embedded version and triple so GitHub server logs
/// identify the client and which release is fetching.
const user_agent = "tk/" ++ build_options.version ++ " (" ++ build_options.triple ++ ")";

/// Process entrypoint for `tk`.
///
/// The executable owns real stdout/stderr buffers, the real subprocess runner,
/// and the real clock. Everything after argv setup is delegated to `cli.runArgv`
/// so command behavior stays testable without spawning a process.
pub fn main(init: std.process.Init) !void {
    const io = init.io;
    var stdout_buf: [4096]u8 = undefined;
    var stderr_buf: [1024]u8 = undefined;
    var stdin_buf: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(io, &stdout_buf);
    var stderr = std.Io.File.stderr().writer(io, &stderr_buf);
    var stdin = std.Io.File.stdin().reader(io, &stdin_buf);

    var real_runner = proc.RealRunner.init(io);
    var real_http = http_mod.RealHttp.init(init.gpa, io, user_agent);
    defer real_http.deinit();
    var real_clock = clock_mod.RealClock.init(io);
    var random_source = std.Random.IoSource{ .io = io };

    const no_color = init.minimal.environ.containsUnemptyConstant("NO_COLOR");
    const clicolor_force = init.minimal.environ.containsUnemptyConstant("CLICOLOR_FORCE");
    const stdout_mode = try render.Mode.detect(io, std.Io.File.stdout(), no_color, clicolor_force);
    const stderr_mode = try render.Mode.detect(io, std.Io.File.stderr(), no_color, clicolor_force);

    const deps = cli.Deps{
        .stdout = &stdout.interface,
        .stderr = &stderr.interface,
        .stdin = &stdin.interface,
        .gpa = init.gpa,
        .io = io,
        .cwd = std.Io.Dir.cwd(),
        .runner = real_runner.runner(),
        .http = real_http.http(),
        .clock = real_clock.clock(),
        .random = random_source.interface(),
        .styler = .{ .stdout = stdout_mode, .stderr = stderr_mode },
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
