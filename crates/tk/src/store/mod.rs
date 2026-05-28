//! Repository Store module: SQLite-backed current-state store + Mutation Log.
//!
//! The store layer owns: schema migrations, the `display_prefix` seed,
//! the monotonic [`sequences`] counters, the [`mutations`] outbox, the
//! [`repository`] facade exposing typed item operations
//! (open / resolve / list / next / show / create / update / status / dependency),
//! the [`sync`] helpers (Pull merge, Mutation Log decode + state transitions,
//! Remote read), and the `sql_value` SQLite value mapping for the domain
//! enums.

pub mod display_prefix;
pub mod migrations;
pub mod mutations;
pub mod repository;
pub mod sequences;
mod sql_value;
pub mod sync;

#[cfg(test)]
pub(crate) mod testing;
