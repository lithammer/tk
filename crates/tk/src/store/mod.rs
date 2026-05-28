//! Repository Store module: SQLite-backed current-state store + Mutation Log.
//!
//! The store layer owns: schema migrations, the `display_prefix` seed,
//! the monotonic [`sequences`] counters, the [`mutations`] outbox, and the
//! [`repository`] facade exposing typed item operations
//! (open / resolve / list / next / show / create / update / status / dependency).

pub mod display_prefix;
pub mod migrations;
pub mod mutations;
pub mod repository;
pub mod sequences;

#[cfg(test)]
pub(crate) mod testing;
