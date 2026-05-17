test {
    _ = @import("cli.zig");
    _ = @import("clock.zig");
    _ = @import("commands/add.zig");
    _ = @import("commands/block.zig");
    _ = @import("commands/unblock.zig");
    _ = @import("commands/init.zig");
    _ = @import("commands/list.zig");
    _ = @import("commands/message.zig");
    _ = @import("commands/next.zig");
    _ = @import("commands/prime.zig");
    _ = @import("proc/runner.zig");
    _ = @import("proc/fake.zig");
    _ = @import("domain/display_prefix.zig");
    _ = @import("domain/item_class.zig");
    _ = @import("domain/origin.zig");
    _ = @import("domain/priority.zig");
    _ = @import("domain/status.zig");
    _ = @import("domain/ticket_kind.zig");
    _ = @import("git/discovery.zig");
    _ = @import("store/diagnostic.zig");
    _ = @import("store/migrations.zig");
    _ = @import("store/repository.zig");
    _ = @import("testing/script.zig");
    _ = @import("testing/scenarios.zig");
    _ = @import("testing/smoke.zig");
    _ = @import("testing/txtar.zig");
}
