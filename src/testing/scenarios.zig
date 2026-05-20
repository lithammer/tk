const std = @import("std");
const cli = @import("../cli.zig");
const script = @import("script.zig");

const prime_basic_path = "src/testing/scenarios/prime/basic.txtar";
const add_create_epic_child_path = "src/testing/scenarios/add/create_epic_child.txtar";
const block_help_path = "src/testing/scenarios/block/help.txtar";
const done_help_path = "src/testing/scenarios/done/help.txtar";
const list_help_path = "src/testing/scenarios/list/help.txtar";
const manpage_basic_path = "src/testing/scenarios/manpage/basic.txtar";
const next_help_path = "src/testing/scenarios/next/help.txtar";
const show_help_path = "src/testing/scenarios/show/help.txtar";
const unblock_help_path = "src/testing/scenarios/unblock/help.txtar";
const update_help_path = "src/testing/scenarios/update/help.txtar";
const worktree_help_path = "src/testing/scenarios/worktree/help.txtar";
const harness_preserve_path = "src/testing/scenarios/_harness/preserve_sections.txtar";

const prime_basic = @embedFile("scenarios/prime/basic.txtar");
comptime {
    cli.assertNoCR(prime_basic);
}
const add_create_epic_child = @embedFile("scenarios/add/create_epic_child.txtar");
comptime {
    cli.assertNoCR(add_create_epic_child);
}
const block_help = @embedFile("scenarios/block/help.txtar");
comptime {
    cli.assertNoCR(block_help);
}
const done_help = @embedFile("scenarios/done/help.txtar");
comptime {
    cli.assertNoCR(done_help);
}
const list_help = @embedFile("scenarios/list/help.txtar");
comptime {
    cli.assertNoCR(list_help);
}
const manpage_basic = @embedFile("scenarios/manpage/basic.txtar");
comptime {
    cli.assertNoCR(manpage_basic);
}
const next_help = @embedFile("scenarios/next/help.txtar");
comptime {
    cli.assertNoCR(next_help);
}
const show_help = @embedFile("scenarios/show/help.txtar");
comptime {
    cli.assertNoCR(show_help);
}
const unblock_help = @embedFile("scenarios/unblock/help.txtar");
comptime {
    cli.assertNoCR(unblock_help);
}
const update_help = @embedFile("scenarios/update/help.txtar");
comptime {
    cli.assertNoCR(update_help);
}
const worktree_help = @embedFile("scenarios/worktree/help.txtar");
comptime {
    cli.assertNoCR(worktree_help);
}
const harness_preserve = @embedFile("scenarios/_harness/preserve_sections.txtar");
comptime {
    cli.assertNoCR(harness_preserve);
}

test "prime/basic" {
    try script.runScenario(std.testing.allocator, prime_basic_path, prime_basic);
}

test "add/create_epic_child" {
    try script.runScenario(std.testing.allocator, add_create_epic_child_path, add_create_epic_child);
}

test "block/help" {
    try script.runScenario(std.testing.allocator, block_help_path, block_help);
}

test "done/help" {
    try script.runScenario(std.testing.allocator, done_help_path, done_help);
}

test "list/help" {
    try script.runScenario(std.testing.allocator, list_help_path, list_help);
}

test "manpage/basic" {
    try script.runScenario(std.testing.allocator, manpage_basic_path, manpage_basic);
}

test "next/help" {
    try script.runScenario(std.testing.allocator, next_help_path, next_help);
}

test "show/help" {
    try script.runScenario(std.testing.allocator, show_help_path, show_help);
}

test "unblock/help" {
    try script.runScenario(std.testing.allocator, unblock_help_path, unblock_help);
}

test "update/help" {
    try script.runScenario(std.testing.allocator, update_help_path, update_help);
}

test "worktree/help" {
    try script.runScenario(std.testing.allocator, worktree_help_path, worktree_help);
}

test "_harness/preserve_sections" {
    try script.runScenario(std.testing.allocator, harness_preserve_path, harness_preserve);
}
