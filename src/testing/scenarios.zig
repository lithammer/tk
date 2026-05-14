const std = @import("std");
const script = @import("script.zig");

const prime_basic_path = "src/testing/scenarios/prime/basic.txtar";
const list_help_path = "src/testing/scenarios/list/help.txtar";
const harness_preserve_path = "src/testing/scenarios/_harness/preserve_sections.txtar";

const prime_basic = @embedFile("scenarios/prime/basic.txtar");
const list_help = @embedFile("scenarios/list/help.txtar");
const harness_preserve = @embedFile("scenarios/_harness/preserve_sections.txtar");

test "prime/basic" {
    try script.runScenario(std.testing.allocator, prime_basic_path, prime_basic);
}

test "list/help" {
    try script.runScenario(std.testing.allocator, list_help_path, list_help);
}

test "_harness/preserve_sections" {
    try script.runScenario(std.testing.allocator, harness_preserve_path, harness_preserve);
}
