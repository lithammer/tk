pub const SliceArgIter = struct {
    items: []const []const u8,
    idx: usize = 0,

    pub fn next(self: *SliceArgIter) ?[]const u8 {
        if (self.idx >= self.items.len) return null;
        defer self.idx += 1;
        return self.items[self.idx];
    }
};
