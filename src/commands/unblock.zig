//! `tk unblock` — remove an item-backed Dependency edge.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");

/// Dispatcher metadata for `tk unblock`.
pub const meta: cli.CommandMeta = .{
    .name = "unblock",
    .description = "Remove a blocking relationship",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\<str>
    \\<str>
    \\
);

/// Parse `tk unblock` args, resolve both items, and remove a Dependency.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var diag: clap.Diagnostic = .{};
    var res = clap.parseEx(clap.Help, &params, clap.parsers.default, args_iter, .{
        .diagnostic = &diag,
        .allocator = deps.gpa,
    }) catch |err| {
        diag.report(deps.stderr, err) catch {};
        return 2;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    const blocked_arg = res.positionals[0] orelse {
        deps.stderr.writeAll(messages.unblock_args_required ++ "\n") catch {};
        return 2;
    };
    const blocking_arg = res.positionals[1] orelse {
        deps.stderr.writeAll(messages.unblock_args_required ++ "\n") catch {};
        return 2;
    };

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    const blocked = (repository.resolveItemRef(store, deps.gpa, blocked_arg) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.unblock_blocked_not_found_prefix ++ "{s}" ++ messages.unblock_item_not_found_suffix ++ "\n", .{blocked_arg}) catch {};
        return 1;
    };
    defer blocked.deinit(deps.gpa);

    const blocking = (repository.resolveItemRef(store, deps.gpa, blocking_arg) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.unblock_blocking_not_found_prefix ++ "{s}" ++ messages.unblock_item_not_found_suffix ++ "\n", .{blocking_arg}) catch {};
        return 1;
    };
    defer blocking.deinit(deps.gpa);

    if (std.mem.eql(u8, blocked.id, blocking.id)) {
        deps.stderr.writeAll(messages.unblock_self_dependency ++ "\n") catch {};
        return 1;
    }

    repository.removeDependency(store, deps.gpa, deps.clock, .{
        .blocked_id = blocked.id,
        .blocking_id = blocking.id,
    }) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };

    deps.stdout.print(messages.unblock_success_prefix ++ "{s} no longer blocked by {s}\n", .{ blocked_arg, blocking_arg }) catch {};
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk unblock - remove a Dependency
        \\
        \\Usage:
        \\  tk unblock <blocked-id> <blocking-id> [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
    );
}

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.unblock_store_busy_retry,
    .out_of_memory = messages.unblock_out_of_memory,
    .fallback = messages.unblock_write_failed,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "unblock",
    .missing_store = messages.unblock_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
}
