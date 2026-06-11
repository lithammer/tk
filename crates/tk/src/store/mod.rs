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

/// Begin a Repository Store write transaction (`BEGIN IMMEDIATE`).
///
/// Every store write path must start its transaction here. `BEGIN IMMEDIATE`
/// takes the write lock up front, so a concurrent writer queues on the
/// connection's `busy_timeout`. A deferred transaction that reads before
/// writing instead pins a snapshot and gets `SQLITE_BUSY` *immediately* on
/// the write-lock upgrade once any other writer has committed — the busy
/// timeout never applies to that upgrade, because retrying a stale snapshot
/// cannot succeed. Under parallel agents (tk-10) that turned the intended
/// 5-second queue into instant "Repository Store is busy" failures.
///
/// Read-only paths keep using plain statements or deferred transactions;
/// taking the write lock for a read would serialize readers for nothing.
pub fn write_transaction(
    conn: &mut rusqlite::Connection,
) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
}
