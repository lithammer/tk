//! Per-command parsing and handlers. One module per subcommand. Slice 0
//! ships `init` only; downstream slices add `add`, `list`, `show`, … as
//! their tickets land.

pub mod init;
