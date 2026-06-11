//! Monotonic sequence-counter helpers shared by the Repository Store.
//!
//! The `sequences` table holds three named counters
//! (`item_created_seq`, `display_seq`, `mutation_seq`) seeded at zero by
//! migration 1. Allocation is a single `UPDATE … RETURNING` inside an
//! already-open write transaction. A missing counter row is Repository Store
//! corruption rather than a recoverable condition, so [`SequenceError`]
//! makes that case explicit; healthy stores never see it.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Errors returned by [`next`].
///
/// `Missing` flags Repository Store corruption (the counter row vanished
/// from the `sequences` table); `Sqlite` is a pass-through of the
/// underlying driver error so command-side stderr can render the SQLite
/// `errmsg` verbatim per ADR-0017.
#[derive(Debug, Error)]
pub enum SequenceError {
    /// The named counter row is missing from the `sequences` table. The
    /// schema migration seeds all three counters at zero, so reaching this
    /// arm means the store has been tampered with or partially restored.
    #[error("sequence counter `{0}` is missing from the store")]
    Missing(&'static str),
    /// Underlying SQLite error from the `UPDATE … RETURNING` step.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Increment the named counter inside the caller's open write transaction
/// and return the new value.
///
/// The caller MUST have an active `begin immediate` transaction on `conn`
/// (start one with [`crate::store::write_transaction`]); the helper does not
/// start, commit, or roll one back. Passing a [`rusqlite::Transaction`]
/// (which derefs to [`Connection`]) is the idiomatic way to satisfy that
/// contract.
///
/// `name` is a `&'static str` so the [`SequenceError::Missing`] arm can
/// carry the offending counter name without an allocation; the schema's
/// CHECK constraint limits the universe to three values, all of which sit
/// in this crate as string literals.
pub fn next(conn: &Connection, name: &'static str) -> Result<i64, SequenceError> {
    let value: Option<i64> = conn
        .query_row(
            "update sequences set value = value + 1 where name = ?1 returning value",
            params![name],
            |row| row.get(0),
        )
        .optional()?;
    value.ok_or(SequenceError::Missing(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use rusqlite::Connection;

    fn open_seeded_memory() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    #[test]
    fn first_allocation_returns_one() {
        let conn = open_seeded_memory();
        let tx = conn.unchecked_transaction().unwrap();
        assert_eq!(next(&tx, "item_created_seq").unwrap(), 1);
        tx.commit().unwrap();
    }

    #[test]
    fn allocations_increment_monotonically_within_a_transaction() {
        let conn = open_seeded_memory();
        let tx = conn.unchecked_transaction().unwrap();
        let a = next(&tx, "display_seq").unwrap();
        let b = next(&tx, "display_seq").unwrap();
        let c = next(&tx, "display_seq").unwrap();
        tx.commit().unwrap();
        assert_eq!((a, b, c), (1, 2, 3));
    }

    #[test]
    fn counters_are_independent() {
        let conn = open_seeded_memory();
        let tx = conn.unchecked_transaction().unwrap();
        assert_eq!(next(&tx, "item_created_seq").unwrap(), 1);
        assert_eq!(next(&tx, "display_seq").unwrap(), 1);
        assert_eq!(next(&tx, "mutation_seq").unwrap(), 1);
        tx.commit().unwrap();
    }

    #[test]
    fn missing_counter_row_reports_corruption() {
        let conn = open_seeded_memory();
        conn.execute("delete from sequences where name = 'mutation_seq'", [])
            .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        let err = next(&tx, "mutation_seq").unwrap_err();
        assert!(matches!(err, SequenceError::Missing("mutation_seq")));
    }
}
