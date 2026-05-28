//! SQLite value mapping for the schema-determined domain enums.
//!
//! The `items.*` columns carry CHECK constraints that pin the legal spellings,
//! so [`FromSql`] accepts only those spellings; an unrecognized value is
//! Repository Store corruption, surfaced as a [`FromSqlError`] rather than a
//! panic so it rides the store's `rusqlite::Error` path and renders through the
//! storage-error frame. [`ToSql`] single-sources each spelling on the enum's
//! `text()` method, which is the storage contract.
//!
//! These impls live in the store layer, not under [`crate::domain`], so the
//! domain value types stay free of any SQLite coupling.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

use crate::domain::item_class::ItemClass;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;

impl FromSql for ItemClass {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "ticket" => Ok(Self::Ticket),
            "epic" => Ok(Self::Epic),
            other => Err(corrupt("item_class", other)),
        }
    }
}

impl ToSql for ItemClass {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for TicketKind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "task" => Ok(Self::Task),
            "bug" => Ok(Self::Bug),
            other => Err(corrupt("ticket_kind", other)),
        }
    }
}

impl ToSql for TicketKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for Priority {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "P0" => Ok(Self::P0),
            "P1" => Ok(Self::P1),
            "P2" => Ok(Self::P2),
            "P3" => Ok(Self::P3),
            "P4" => Ok(Self::P4),
            other => Err(corrupt("priority", other)),
        }
    }
}

impl ToSql for Priority {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for ItemStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "open" => Ok(Self::Open),
            "active" => Ok(Self::Active),
            "done" => Ok(Self::Done),
            other => Err(corrupt("status", other)),
        }
    }
}

impl ToSql for ItemStatus {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for Origin {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "local" => Ok(Self::Local),
            "backend" => Ok(Self::Backend),
            other => Err(corrupt("origin", other)),
        }
    }
}

impl ToSql for Origin {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

/// Build the corruption error for a CHECK-violating column value. The message
/// names the column and the offending spelling so a corrupt Repository Store is
/// diagnosable from the rendered storage error.
fn corrupt(column: &str, value: &str) -> FromSqlError {
    FromSqlError::Other(format!("repository store corruption: unknown {column} `{value}`").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_sql_accepts_the_check_constrained_spellings() {
        // Pins the legal spelling set at the decode boundary; a drift between
        // these and the V1 CHECK constraints is store corruption.
        assert_eq!(
            ItemClass::column_result(ValueRef::Text(b"epic")).unwrap(),
            ItemClass::Epic
        );
        assert_eq!(
            TicketKind::column_result(ValueRef::Text(b"bug")).unwrap(),
            TicketKind::Bug
        );
        assert_eq!(
            Priority::column_result(ValueRef::Text(b"P3")).unwrap(),
            Priority::P3
        );
        assert_eq!(
            ItemStatus::column_result(ValueRef::Text(b"active")).unwrap(),
            ItemStatus::Active
        );
        assert_eq!(
            Origin::column_result(ValueRef::Text(b"backend")).unwrap(),
            Origin::Backend
        );
    }

    #[test]
    fn from_sql_rejects_corrupt_value_instead_of_panicking() {
        // A value the CHECK constraint should have rejected must surface as a
        // recoverable FromSqlError the store renders as corruption — never a
        // thread abort.
        let err = ItemStatus::column_result(ValueRef::Text(b"archived")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "repository store corruption: unknown status `archived`"
        );
    }
}
