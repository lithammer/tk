const std = @import("std");
const embed = @import("../embed.zig");
const platform = @import("platform.zig");
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

// `@embedFile`'s path is resolved relative to the file containing the call,
// so this helper must live in the same source file as the scenario embeds.
// Hoisting it into `embed.zig` would resolve every path against that file's
// directory instead.
fn embedScenario(comptime path: []const u8) []const u8 {
    const bytes = @embedFile(path);
    comptime embed.assertNoCR(bytes);
    return bytes;
}

const prime_basic = embedScenario("scenarios/prime/basic.txtar");
const add_create_epic_child = embedScenario("scenarios/add/create_epic_child.txtar");
const block_help = embedScenario("scenarios/block/help.txtar");
const done_help = embedScenario("scenarios/done/help.txtar");
const list_help = embedScenario("scenarios/list/help.txtar");
const manpage_basic = embedScenario("scenarios/manpage/basic.txtar");
const next_help = embedScenario("scenarios/next/help.txtar");
const show_help = embedScenario("scenarios/show/help.txtar");
const unblock_help = embedScenario("scenarios/unblock/help.txtar");
const update_help = embedScenario("scenarios/update/help.txtar");
const worktree_help = embedScenario("scenarios/worktree/help.txtar");
const harness_preserve = embedScenario("scenarios/_harness/preserve_sections.txtar");

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
    // The fixture's expected stdout contains troff escapes (`\-`, `\fB`,
    // `\fR`, `.\"`) that the Windows branch of `script.normalizeWork` would
    // rewrite to forward slashes, producing a spurious mismatch. Windows has
    // no `man` pager and the embedded manpage bytes are identical across
    // platforms, so POSIX coverage is sufficient.
    try platform.skipOnWindows();
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
