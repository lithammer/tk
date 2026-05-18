const std = @import("std");
const script = @import("script.zig");

const prime_basic_path = "src/testing/scenarios/prime/basic.txtar";
const block_help_path = "src/testing/scenarios/block/help.txtar";
const done_help_path = "src/testing/scenarios/done/help.txtar";
const list_help_path = "src/testing/scenarios/list/help.txtar";
const next_help_path = "src/testing/scenarios/next/help.txtar";
const show_help_path = "src/testing/scenarios/show/help.txtar";
const unblock_help_path = "src/testing/scenarios/unblock/help.txtar";
const update_help_path = "src/testing/scenarios/update/help.txtar";
const worktree_help_path = "src/testing/scenarios/worktree/help.txtar";
const harness_preserve_path = "src/testing/scenarios/_harness/preserve_sections.txtar";

const prime_basic = @embedFile("scenarios/prime/basic.txtar");
const block_help = @embedFile("scenarios/block/help.txtar");
const done_help = @embedFile("scenarios/done/help.txtar");
const list_help = @embedFile("scenarios/list/help.txtar");
const next_help = @embedFile("scenarios/next/help.txtar");
const show_help = @embedFile("scenarios/show/help.txtar");
const unblock_help = @embedFile("scenarios/unblock/help.txtar");
const update_help = @embedFile("scenarios/update/help.txtar");
const worktree_help = @embedFile("scenarios/worktree/help.txtar");
const harness_preserve = @embedFile("scenarios/_harness/preserve_sections.txtar");

test "prime/basic" {
    try script.runScenario(std.testing.allocator, prime_basic_path, prime_basic);
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
