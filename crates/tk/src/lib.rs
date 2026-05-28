//! `tk` Rust port — slice 0 (ADR-0018 / ADR-0019).
//!
//! Modules:
//!
//! - [`cli`]: top-level dispatch and shared `Deps`.
//! - [`commands`]: one module per subcommand.
//! - [`messages`]: verbatim user-visible substrings (ADR-0017).
//! - [`platform`]: compile-time OS predicates.
//! - [`proc`]: subprocess runner trait + real/fake implementations.
//! - [`clock`]: injectable wall clock with `TK_NOW` override.
//! - [`git`]: git subprocess discovery façade.
//! - [`store`]: Repository Store + migrations.
//! - [`domain`]: pure domain helpers.
//!
//! RNG lives in the `rand` crate; `Deps::rng` is a borrowed
//! `&mut dyn rand::Rng` (the dyn-compatible low-level trait; `RngCore` is
//! still defined as a marker alias). `TK_RAND_SEED` is consumed in
//! `main.rs` to pick between `StdRng::seed_from_u64` and
//! `StdRng::try_from_rng(&mut SysRng)`.

pub mod cli;
pub mod clock;
pub mod commands;
pub mod domain;
pub mod git;
pub mod messages;
pub mod platform;
pub mod proc;
pub mod render;
pub mod store;
pub mod worktree;
