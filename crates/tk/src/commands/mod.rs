//! Per-command parsing and handlers. One module per subcommand, mirroring
//! `src/commands/` in the Zig oracle. Slice 0 ships `init` only; downstream
//! slices add `add`, `list`, `show`, … as their tickets land.

pub mod init;
