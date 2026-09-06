//! Local Plan membership and current-state reads (ADR-0050).
//!
//! Membership edits resolve the entire batch under the write lock and never
//! update Tickets or append Mutations. Stable Item identity survives Binding
//! changes, so Promotion, Detach and Re-Adopt need no Plan-specific path.

use std::collections::{HashMap, HashSet};

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;

use super::{Store, resolve_item_ref};

/// The requested change to the current Plan's membership.
#[derive(Debug, Clone, Copy)]
pub enum MembershipEdit {
    Add,
    Remove,
}

/// One unique Ticket's result, reported only after the batch commits.
#[derive(Debug)]
pub struct MembershipResult {
    pub display_id: String,
    pub changed: bool,
}

/// A batch validation failure leaves all membership unchanged.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("'{0}' is not a known Display ID or Alias")]
    NotFound(String),
    #[error("'{0}' is an Epic; only Tickets can belong to the Plan")]
    NotATicket(String),
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
}

/// Resolve and apply one atomic membership edit, deduplicating Aliases.
pub fn edit_membership(
    store: &mut Store,
    ids: &[String],
    edit: MembershipEdit,
) -> Result<Vec<MembershipResult>, PlanError> {
    let tx = crate::store::write_transaction(&mut store.conn)?;
    let mut seen = HashSet::new();
    let mut tickets = Vec::new();
    for id in ids {
        let item = resolve_item_ref(&tx, id)?.ok_or_else(|| PlanError::NotFound(id.clone()))?;
        if item.item_class != ItemClass::Ticket {
            return Err(PlanError::NotATicket(item.display_id));
        }
        if seen.insert(item.id.clone()) {
            tickets.push(item);
        }
    }
    let sql = match edit {
        MembershipEdit::Add => {
            "insert into plan_members(item_id) values (?1) on conflict do nothing"
        }
        MembershipEdit::Remove => "delete from plan_members where item_id = ?1",
    };
    let mut results = Vec::new();
    for item in tickets {
        results.push(MembershipResult {
            changed: tx.execute(sql, [&item.id])? != 0,
            display_id: item.display_id,
        });
    }
    tx.commit()?;
    Ok(results)
}

/// Clear membership, including unfinished Tickets, without changing Items.
pub fn clear(store: &mut Store) -> Result<usize, PlanError> {
    let tx = crate::store::write_transaction(&mut store.conn)?;
    let count = tx.execute("delete from plan_members", [])?;
    tx.commit()?;
    Ok(count)
}

/// A Plan member's current Ticket state, in creation order.
#[derive(Debug)]
pub struct PlanTicket {
    pub display_id: String,
    pub title: String,
    pub priority: Option<Priority>,
    pub status: ItemStatus,
    pub selection: SelectionState,
    pub blockers: Vec<PlanBlocker>,
}

/// Unresolved readiness evidence; Dependency membership is relative to the whole Plan.
#[derive(Debug)]
pub enum PlanBlocker {
    Dependency {
        display_id: String,
        outside_plan: bool,
    },
    External {
        reason: String,
    },
}

/// Mutually exclusive progress sections derived from current Ticket state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSection {
    Ready,
    InProgress,
    Waiting,
    Done,
}

impl PlanTicket {
    /// Done and active Items keep their section even if blockers remain.
    #[must_use]
    pub fn section(&self) -> PlanSection {
        match self.status {
            ItemStatus::Done => PlanSection::Done,
            ItemStatus::Active => PlanSection::InProgress,
            ItemStatus::Open
                if self.selection == SelectionState::Accepted && self.blockers.is_empty() =>
            {
                PlanSection::Ready
            }
            ItemStatus::Open => PlanSection::Waiting,
        }
    }
}

/// Read every member regardless of Scope, Origin or Ticket state.
pub fn read(store: &Store) -> Result<Vec<PlanTicket>, PlanError> {
    // One read snapshot keeps membership, waiting reasons and footer counts consistent.
    let tx = store.conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "select i.display_value, i.title, i.priority, i.status, i.work_state, i.selection_state, i.id \
         from plan_members p join items i on i.id = p.item_id order by i.created_seq",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(6)?,
                PlanTicket {
                    display_id: row.get(0)?,
                    title: row.get(1)?,
                    priority: row.get(2)?,
                    status: ItemStatus::of(row.get(3)?, row.get(4)?),
                    selection: row.get(5)?,
                    blockers: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut indices = HashMap::new();
    let mut tickets = Vec::new();
    for (id, ticket) in rows {
        indices.insert(id, tickets.len());
        tickets.push(ticket);
    }
    let mut dependencies = tx.prepare(
        "select d.blocked_id, b.display_value, \
                not exists (select 1 from plan_members where item_id = b.id) \
         from dependencies d join plan_members p on p.item_id = d.blocked_id \
         join items b on b.id = d.blocking_id where b.status <> 'done' \
         order by b.created_seq",
    )?;
    for row in dependencies.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            PlanBlocker::Dependency {
                display_id: row.get(1)?,
                outside_plan: row.get(2)?,
            },
        ))
    })? {
        let (id, blocker) = row?;
        tickets[indices[&id]].blockers.push(blocker);
    }
    let mut external = tx.prepare(
        "select eb.item_id, eb.reason from external_blockers eb \
         join plan_members p on p.item_id = eb.item_id \
         where eb.resolved_at is null order by eb.created_at, eb.id",
    )?;
    for row in external.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, reason) = row?;
        tickets[indices[&id]]
            .blockers
            .push(PlanBlocker::External { reason });
    }
    Ok(tickets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::backend_operation::{AdoptedItem, BackendItemIdentity};
    use crate::domain::lifecycle::Lifecycle;
    use crate::domain::promotion_capability::PromotionCapabilities;
    use crate::domain::ticket_kind::TicketKind;
    use crate::store::repository::{
        detach,
        next::{self, NextOptions, NextScope},
    };
    use crate::store::testing::{
        FixtureItem, FixtureRemote, TmpStore, apply_promotion_receipt, insert_alias,
        insert_dependency, insert_external_blocker, insert_fixture_item, insert_fixture_remote,
        seed_store,
    };
    use rand::SeedableRng;

    #[test]
    fn membership_follows_identity_through_promotion_detach_and_readopt() {
        let tmp = TmpStore::new("tk");
        let conn = seed_store(&tmp);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "stable",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_alias(&conn, "old-1", "stable").unwrap();
        let mut store = Store::for_test(conn);
        let results = edit_membership(
            &mut store,
            &["old-1".into(), "TK-1".into()],
            MembershipEdit::Add,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display_id, "tk-1");
        let identity = BackendItemIdentity {
            backend_key: "https://github.com/o/r/issues/42".into(),
            display_id: "gh-42".into(),
        };
        apply_promotion_receipt(
            &mut store.conn,
            "stable",
            "github",
            &identity,
            "2026-05-10T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(read(&store).unwrap()[0].display_id, "gh-42");
        detach::detach(&mut store, "gh-42", "2026-05-11T00:00:00.000Z").unwrap();
        assert_eq!(read(&store).unwrap()[0].display_id, "tk-1");
        let mut rng = rand::rngs::StdRng::seed_from_u64(0);
        crate::store::sync::adopt_backend_ticket(
            &mut store.conn,
            BackendKind::Github,
            &mut rng,
            &AdoptedItem {
                backend_key: identity.backend_key,
                display_id: identity.display_id,
                ticket_kind: TicketKind::Task,
                title: "Updated remotely".into(),
                body: String::new(),
                status: Lifecycle::Done,
            },
            PromotionCapabilities::all(),
            "2026-05-12T00:00:00.000Z",
        )
        .unwrap();
        let members = read(&store).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].display_id, "gh-42");
        assert_eq!(members[0].status, ItemStatus::Done);
        assert_eq!(members[0].title, "Updated remotely");
        assert!(crate::store::sync::mutation_log_is_empty(&store.conn).unwrap());
    }

    #[test]
    fn plan_priority_crosses_an_epic_only_to_included_children() {
        let tmp = TmpStore::new("tk");
        let conn = seed_store(&tmp);
        for mut item in [
            FixtureItem {
                id: "epic",
                display: "tk-1",
                item_class: "epic",
                priority: None,
                ticket_kind: None,
                created_seq: 1,
                ..FixtureItem::default()
            },
            FixtureItem {
                id: "helper",
                display: "tk-2",
                priority: Some("P3"),
                created_seq: 2,
                ..FixtureItem::default()
            },
            FixtureItem {
                id: "other",
                display: "tk-3",
                priority: Some("P2"),
                created_seq: 3,
                ..FixtureItem::default()
            },
            FixtureItem {
                id: "inside",
                display: "tk-4",
                priority: Some("P1"),
                container_id: Some("epic"),
                created_seq: 4,
                ..FixtureItem::default()
            },
            FixtureItem {
                id: "outside",
                display: "tk-5",
                priority: Some("P0"),
                container_id: Some("epic"),
                created_seq: 5,
                ..FixtureItem::default()
            },
        ] {
            item.title = item.id;
            insert_fixture_item(&conn, item).unwrap();
        }
        insert_dependency(&conn, "helper", "epic").unwrap();
        insert_external_blocker(&conn, "external", "inside", None).unwrap();
        let mut store = Store::for_test(conn);
        edit_membership(
            &mut store,
            &["tk-2".into(), "tk-3".into(), "tk-4".into()],
            MembershipEdit::Add,
        )
        .unwrap();
        let selected = next::next_ready_ticket(
            &store,
            NextOptions {
                scope: NextScope::Plan(None),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.display_id, "tk-2");
        assert_eq!(selected.rationale.unwrap().blocked_display_id, "tk-4");
        edit_membership(&mut store, &["tk-4".into()], MembershipEdit::Remove).unwrap();
        let selected = next::next_ready_ticket(
            &store,
            NextOptions {
                scope: NextScope::Plan(None),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.display_id, "tk-3");
    }
}
