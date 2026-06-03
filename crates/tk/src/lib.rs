//! `tk` — agent-first, repository-local work tracker.
//!
//! Modules:
//!
//! - [`cli`]: top-level dispatch and shared `Deps`.
//! - [`commands`]: one module per subcommand.
//! - [`platform`]: compile-time OS predicates.
//! - [`proc`]: subprocess runner trait + real/fake implementations.
//! - [`clock`]: injectable wall clock.
//! - [`git`]: git subprocess discovery façade.
//! - [`store`]: Repository Store + migrations.
//! - [`domain`]: pure domain helpers.
//! - [`remote`]: Backend Adapter trait + test fake (real adapters in tk-40).
//! - [`sync`]: backend-blind sync engine (Pull merge + Mutation outbox replay).
//!
//! RNG lives in the `rand` crate; `Deps::rng` is a borrowed
//! `&mut dyn rand::Rng` (the dyn-compatible low-level trait; `RngCore` is
//! still defined as a marker alias). `main.rs` seeds `StdRng` from OS entropy
//! (`SysRng`); tests inject a seeded `StdRng` through `Deps`.

pub mod cli;
pub mod clock;
pub mod commands;
pub mod domain;
pub mod git;
pub mod platform;
pub mod proc;
pub mod remote;
pub mod render;
pub mod store;
pub mod sync;
