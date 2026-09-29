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

use crate::domain::backend_kind::BackendKind;
use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::ticket_kind::TicketKind;
use crate::domain::work_state::WorkState;

impl FromSql for BackendKind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::from_str(text).map_err(|_| corrupt("backend_kind", text))
    }
}

impl ToSql for BackendKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.text().to_sql()
    }
}

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

    fn assert_decodes<T>(spelling: &'static [u8], expected: T)
    where
        T: FromSql + Copy + PartialEq + std::fmt::Debug,
    {
        assert_eq!(
            T::column_result(ValueRef::Text(spelling)).unwrap(),
            expected,
            "stored spelling {:?}",
            std::str::from_utf8(spelling).unwrap()
        );
    }

    #[test]
    fn from_sql_accepts_the_check_constrained_spellings() {
        // Keep literals independent from `text()` so decoder and encoder
        // drift cannot agree with each other and hide a broken SQL mapping.
        assert_decodes(b"github", BackendKind::Github);
        assert_decodes(b"jira", BackendKind::Jira);

        assert_decodes(b"ticket", ItemClass::Ticket);
        assert_decodes(b"epic", ItemClass::Epic);

        assert_decodes(b"task", TicketKind::Task);
        assert_decodes(b"bug", TicketKind::Bug);

        assert_decodes(b"P0", Priority::P0);
        assert_decodes(b"P1", Priority::P1);
        assert_decodes(b"P2", Priority::P2);
        assert_decodes(b"P3", Priority::P3);
        assert_decodes(b"P4", Priority::P4);

        assert_decodes(b"triage", SelectionState::Triage);
        assert_decodes(b"accepted", SelectionState::Accepted);
        assert_decodes(b"parked", SelectionState::Parked);

        assert_decodes(b"local", Origin::Local);
        assert_decodes(b"backend", Origin::Backend);

        assert_decodes(b"pending", MutationState::Pending);
        assert_decodes(b"failed", MutationState::Failed);
        assert_decodes(b"applying", MutationState::Applying);
        assert_decodes(b"skipped", MutationState::Skipped);
        assert_decodes(b"cancelled", MutationState::Cancelled);
        assert_decodes(b"abandoned", MutationState::Abandoned);
        assert_decodes(b"applied", MutationState::Applied);

        assert_decodes(b"update_ticket", MutationType::UpdateTicket);
        assert_decodes(b"update_epic", MutationType::UpdateEpic);
        assert_decodes(b"set_item_status", MutationType::SetItemStatus);
        assert_decodes(b"add_ticket_to_epic", MutationType::AddTicketToEpic);
        assert_decodes(
            b"remove_ticket_from_epic",
            MutationType::RemoveTicketFromEpic,
        );
        assert_decodes(b"add_dependency", MutationType::AddDependency);
        assert_decodes(b"remove_dependency", MutationType::RemoveDependency);
        assert_decodes(b"add_external_blocker", MutationType::AddExternalBlocker);
        assert_decodes(
            b"resolve_external_blocker",
            MutationType::ResolveExternalBlocker,
        );
        assert_decodes(b"promote_ticket", MutationType::PromoteTicket);
        assert_decodes(b"promote_epic", MutationType::PromoteEpic);

        assert_decodes(b"open", Lifecycle::Open);
        assert_decodes(b"done", Lifecycle::Done);

        assert_decodes(b"idle", WorkState::Idle);
        assert_decodes(b"active", WorkState::Active);
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
