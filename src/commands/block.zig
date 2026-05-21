//! `tk block` — create an item-backed Dependency edge.

const std = @import("std");
const clap = @import("clap");
const cli = @import("../cli.zig");
const parse_diagnostic = @import("parse_diagnostic.zig");
const messages = @import("../messages.zig");
const repository = @import("../store/repository.zig");

/// Dispatcher metadata for `tk block`.
pub const meta: cli.CommandMeta = .{
    .name = "block",
    .description = "Record that one item blocks another",
};

const params = clap.parseParamsComptime(
    \\-h, --help  Display this help and exit.
    \\<str>
    \\<str>
    \\
);

/// Parse `tk block` args, resolve both items, and create a Dependency.
pub fn run(deps: cli.Deps, args_iter: anytype) !u8 {
    var res = (try parse_diagnostic.parseOrReportUsage(clap.Help, &params, clap.parsers.default, args_iter, .{
        .stderr = deps.stderr,
        .allocator = deps.gpa,
        .command = .{ .subcommand = meta.name },
    })) orelse return 2;
    defer res.deinit();

    if (res.args.help != 0) {
        writeHelp(deps) catch {};
        return 0;
    }

    const blocked_arg = res.positionals[0] orelse {
        deps.stderr.writeAll(messages.block_args_required ++ "\n") catch {};
        return 2;
    };
    const blocking_arg = res.positionals[1] orelse {
        deps.stderr.writeAll(messages.block_args_required ++ "\n") catch {};
        return 2;
    };

    const store = repository.openStoreCatching(deps.gpa, deps.runner, deps.cwd, deps.stderr, open_msgs) orelse return 1;
    defer store.close();

    const blocked = (repository.resolveItemRef(store, deps.gpa, blocked_arg) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.block_blocked_not_found_prefix ++ "{s}" ++ messages.block_item_not_found_suffix ++ "\n", .{blocked_arg}) catch {};
        return 1;
    };
    defer blocked.deinit(deps.gpa);

    const blocking = (repository.resolveItemRef(store, deps.gpa, blocking_arg) catch |err| {
        renderStorageError(deps, err);
        return 1;
    }) orelse {
        deps.stderr.print(messages.block_blocking_not_found_prefix ++ "{s}" ++ messages.block_item_not_found_suffix ++ "\n", .{blocking_arg}) catch {};
        return 1;
    };
    defer blocking.deinit(deps.gpa);

    if (std.mem.eql(u8, blocked.id, blocking.id)) {
        deps.stderr.writeAll(messages.block_self_dependency ++ "\n") catch {};
        return 1;
    }

    const outcome = repository.addDependency(store, deps.gpa, deps.clock, .{
        .blocked_id = blocked.id,
        .blocking_id = blocking.id,
    }) catch |err| {
        renderStorageError(deps, err);
        return 1;
    };
    switch (outcome) {
        .ok => {},
        .blocked_done => {
            deps.stderr.print(messages.block_blocked_done_prefix ++ "{s}' is done\n", .{blocked_arg}) catch {};
            return 1;
        },
        .blocking_done => {
            deps.stderr.print(messages.block_blocking_done_prefix ++ "{s}' is done\n", .{blocking_arg}) catch {};
            return 1;
        },
        .cycle => {
            deps.stderr.writeAll(messages.block_dependency_cycle ++ "\n") catch {};
            return 1;
        },
        .backend_blocked_local_blocking => {
            deps.stderr.print(
                messages.block_backend_blocked_local_blocking_prefix ++ "{s}' cannot depend on Local blocking item '{s}'\n",
                .{ blocked_arg, blocking_arg },
            ) catch {};
            return 1;
        },
        .backend_kind_mismatch => {
            deps.stderr.print(
                messages.block_backend_kind_mismatch_prefix ++ "{s}' cannot depend on blocking item '{s}' from another Backend kind\n",
                .{ blocked_arg, blocking_arg },
            ) catch {};
            return 1;
        },
    }

    deps.stdout.print(messages.block_success_prefix ++ "{s} blocked by {s}\n", .{ blocked_arg, blocking_arg }) catch {};
    return 0;
}

fn writeHelp(deps: cli.Deps) !void {
    try deps.stdout.writeAll(
        \\tk block - create a Dependency
        \\
        \\Usage:
        \\  tk block <blocked-id> <blocking-id> [options]
        \\
        \\Options:
        \\  -h, --help  Display this help and exit.
        \\
    );
}

const storage_msgs: repository.StorageErrorMessages = .{
    .busy_retry = messages.block_store_busy_retry,
    .out_of_memory = messages.block_out_of_memory,
    .fallback = messages.block_write_failed,
};

const open_msgs: repository.OpenMessages = .{
    .command_name = "block",
    .missing_store = messages.block_missing_store,
    .storage = storage_msgs,
};

fn renderStorageError(deps: cli.Deps, err: anyerror) void {
    repository.renderStorageError(deps.stderr, err, storage_msgs);
}
