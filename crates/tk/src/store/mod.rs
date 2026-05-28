//! Repository Store module: SQLite-backed current-state store + Mutation Log.
//!
//! Slice 0 wires migrations and the `display_prefix` derivation. Later slices
//! grow this module with item views, mutation enqueue, and sync helpers.

pub mod display_prefix;
pub mod migrations;
