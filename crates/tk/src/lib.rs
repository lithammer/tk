//! `tk` Rust port — slice 0 (ADR-0018 / ADR-0019).
//!
//! Layout mirrors the Zig oracle under `src/`:
//!
//! - [`cli`]: top-level dispatch and shared `Deps`.
//! - [`commands`]: one module per subcommand.
//! - [`messages`]: verbatim user-visible substrings (ADR-0017).
//! - [`platform`]: comptime-style OS predicates and cross-platform helpers.
//! - [`proc`]: subprocess runner trait + real/fake implementations.
//! - [`clock`]: injectable wall clock with `TK_NOW` override.
//! - [`rng`]: injectable RNG with `TK_RAND_SEED` override (designed in slice 0,
//!   only `RealRng` is wired — `tk init` has no RNG call sites).
//! - [`git`]: git subprocess discovery façade.
//! - [`store`]: Repository Store + migrations.
//! - [`domain`]: pure domain helpers.

pub mod cli;
pub mod clock;
pub mod commands;
pub mod domain;
pub mod git;
pub mod messages;
pub mod platform;
pub mod proc;
pub mod rng;
pub mod store;
