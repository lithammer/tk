test {
    _ = @import("cli.zig");
    _ = @import("clock.zig");
    _ = @import("commands/prime.zig");
    _ = @import("proc/runner.zig");
    _ = @import("proc/fake.zig");
    _ = @import("domain/display_prefix.zig");
    _ = @import("store/sqlite.zig");
    _ = @import("store/migrations.zig");
    _ = @import("testing/script.zig");
    _ = @import("testing/scenarios.zig");
    _ = @import("testing/txtar.zig");
}
