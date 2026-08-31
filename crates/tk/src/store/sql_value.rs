//! SQLite value mapping for the schema-determined domain enums.
//!
//! A CHECK constraint, or the ADR that introduces one, pins each column's
//! legal spellings, so [`FromSql`] accepts only those; an unrecognized value is
//! Repository Store corruption, surfaced as a [`FromSqlError`] rather than a
//! panic so it rides the store's `rusqlite::Error` path and renders through the
//! storage-error frame. [`ToSql`] single-sources each spelling on the enum's
//! `text()` method, which is the storage contract.
//!
//! These impls live in the store layer, not under [`crate::domain`], so the
//! domain value types stay free of any SQLite coupling.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

use std::str::FromStr;

use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::ticket_kind::TicketKind;
use crate::domain::work_state::WorkState;

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

impl FromSql for SelectionState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        // Tickets always carry a value; Epics store NULL, which rusqlite maps
        // to `None` for an `Option<SelectionState>` column before this is
        // reached — so a NULL never lands here.
        match value.as_str()? {
            "triage" => Ok(Self::Triage),
            "accepted" => Ok(Self::Accepted),
            "parked" => Ok(Self::Parked),
            other => Err(corrupt("selection_state", other)),
        }
    }
}

impl ToSql for SelectionState {
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

impl FromSql for MutationType {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        // Reuse the single-sourced `mutations.mutation_type` spelling table on
        // `FromStr`; an unrecognized value is store corruption, not the
        // `UnknownMutationType` domain error the Apply path raises.
        let text = value.as_str()?;
        Self::from_str(text).map_err(|_| corrupt("mutation_type", text))
    }
}

impl ToSql for MutationType {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for MutationState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "pending" => Ok(Self::Pending),
            "failed" => Ok(Self::Failed),
            "applying" => Ok(Self::Applying),
            "skipped" => Ok(Self::Skipped),
            "cancelled" => Ok(Self::Cancelled),
            "abandoned" => Ok(Self::Abandoned),
            "applied" => Ok(Self::Applied),
            other => Err(corrupt("state", other)),
        }
    }
}

impl ToSql for MutationState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for Lifecycle {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "open" => Ok(Self::Open),
            "done" => Ok(Self::Done),
            other => Err(corrupt("status", other)),
        }
    }
}

impl ToSql for Lifecycle {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

impl FromSql for WorkState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "idle" => Ok(Self::Idle),
            "active" => Ok(Self::Active),
            other => Err(corrupt("work_state", other)),
        }
    }
}

impl ToSql for WorkState {
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
            SelectionState::column_result(ValueRef::Text(b"parked")).unwrap(),
            SelectionState::Parked
        );
        assert_eq!(
            Origin::column_result(ValueRef::Text(b"backend")).unwrap(),
            Origin::Backend
        );
        assert_eq!(
            MutationState::column_result(ValueRef::Text(b"skipped")).unwrap(),
            MutationState::Skipped
        );
        assert_eq!(
            MutationType::column_result(ValueRef::Text(b"set_item_status")).unwrap(),
            MutationType::SetItemStatus
        );
    }

    #[test]
    fn round_trips_through_text_and_from_sql() {
        // Going through `text()` rather than a literal per variant catches
        // one-sided drift a paired literal assertion would miss. A failure
        // means `text()` and the `FromSql` arm disagree for some variant;
        // fix whichever drifted.
        for v in [Lifecycle::Open, Lifecycle::Done] {
            assert_eq!(
                Lifecycle::column_result(ValueRef::Text(v.text().as_bytes())).unwrap(),
                v
            );
        }
        for v in [WorkState::Idle, WorkState::Active] {
            assert_eq!(
                WorkState::column_result(ValueRef::Text(v.text().as_bytes())).unwrap(),
                v
            );
        }
    }

    #[test]
    fn from_sql_rejects_the_other_axis_spelling() {
        // Neither axis may quietly accept the other's spelling. `active` is
        // what leaves `items.status` at migration 011, and an ADR-0028 rebuild
        // that copied the old column verbatim is how `open` would reach
        // `work_state`. Both must decode as corruption, not as a valid value.
        let err = Lifecycle::column_result(ValueRef::Text(b"active")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "repository store corruption: unknown status `active`"
        );
        let err = WorkState::column_result(ValueRef::Text(b"open")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "repository store corruption: unknown work_state `open`"
        );
    }
}
