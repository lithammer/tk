//! Repository Store module: SQLite-backed current-state store + Mutation Log.
//!
//! Slice 0 wires migrations and the `display_prefix` derivation. Later slices
//! grow this module to mirror `src/store/` from the Zig oracle (item views,
//! mutation enqueue, sync helpers).

pub mod display_prefix;
pub mod migrations;
