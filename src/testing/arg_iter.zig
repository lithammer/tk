/// Minimal slice-backed argv iterator with the `next()` shape expected by
/// zig-clap and `cli.runArgv`.
pub const SliceArgIter = struct {
    /// Remaining argument slice, not including argv[0].
    items: []const []const u8,
    idx: usize = 0,

    /// Return the next argument or null at end of input.
    pub fn next(self: *SliceArgIter) ?[]const u8 {
        if (self.idx >= self.items.len) return null;
        defer self.idx += 1;
        return self.items[self.idx];
    }
};
