//! `tk show` full-item read with parent / children / blocker sub-sections.
//!
//! Composed from one item row plus four narrow sub-queries — parent
//! summary, children (when the item is an Epic), `BLOCKED BY`,
//! `BLOCKING`, and unresolved External Blockers. The Repository Store
//! owns the SQL; the command-side renderer owns the tree-block layout
//! and styled output. Each sub-section returns a typed [`ItemSummary`]
//! or [`ExternalBlockerSummary`] so the renderer never deserializes
//! text columns of its own.

use rusqlite::params;

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;

use super::{Store, resolve_item_ref};

/// Compact summary of a related item shown in the `tk show` sub-sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSummary {
    pub display_id: String,
    pub title: String,
    pub item_class: ItemClass,
    pub status: ItemStatus,
    pub priority: Option<Priority>,
}

/// One unresolved External Blocker rendered in `tk show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalBlockerSummary {
    pub reason: String,
}

/// Full current-state view of one Ticket or Epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetail {
    pub id: String,
    pub display_id: String,
    pub item_class: ItemClass,
    pub ticket_kind: Option<TicketKind>,
    pub priority: Option<Priority>,
    /// Selection State (ADR-0027). `Some` for Tickets, `None` for Epics — the
    /// column CHECK keeps Selection State Ticket-only.
    pub selection_state: Option<SelectionState>,
    pub title: String,
    pub body: String,
    /// Optional Closing Reason (ADR-0023). Present only on `done` items; the
    /// column CHECK keeps it non-null unless `status = 'done'`.
    pub closing_reason: Option<String>,
    pub status: ItemStatus,
    pub created_at: String,
    pub updated_at: String,
    pub parent: Option<ItemSummary>,
    pub children: Vec<ItemSummary>,
    pub blocked_by: Vec<ItemSummary>,
    pub blocking: Vec<ItemSummary>,
    pub external_blockers: Vec<ExternalBlockerSummary>,
}

/// Read one item's full current state. Returns `Ok(None)` when the
/// supplied Display ID or Alias does not resolve.
pub fn show_item(store: &Store, display_arg: &str) -> Result<Option<ItemDetail>, rusqlite::Error> {
    let Some(reference) = resolve_item_ref(&store.conn, display_arg)? else {
        return Ok(None);
    };

    let row = store.conn.query_row(
        "select id, display_value, item_class, ticket_kind, priority, selection_state, title, body, \
                closing_reason, status, created_at, updated_at, container_id \
           from items where id = ?1",
        params![&reference.id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, ItemClass>(2)?,
                r.get::<_, Option<TicketKind>>(3)?,
                r.get::<_, Option<Priority>>(4)?,
                r.get::<_, Option<SelectionState>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, ItemStatus>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, Option<String>>(12)?,
            ))
        },
    );
    let Ok((
        id,
        display_id,
        item_class,
        ticket_kind,
        priority,
        selection_state,
        title,
        body,
        closing_reason,
        status,
        created_at,
        updated_at,
        container_id,
    )) = row
    else {
        return match row {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other),
            Ok(_) => unreachable!(),
        };
    };

    let parent = container_id
        .as_deref()
        .map(|cid| read_item_summary_by_id(&store.conn, cid))
        .transpose()?
        .flatten();

    let children = if item_class == ItemClass::Epic {
        read_children(&store.conn, &id)?
    } else {
        Vec::new()
    };
    let blocked_by = read_blocked_by(&store.conn, &id)?;
    let blocking = read_blocking(&store.conn, &id)?;
    let external_blockers = read_external_blockers(&store.conn, &id)?;

    Ok(Some(ItemDetail {
        id,
        display_id,
        item_class,
        ticket_kind,
        priority,
        selection_state,
        title,
        body,
        closing_reason,
        status,
        created_at,
        updated_at,
        parent,
        children,
        blocked_by,
        blocking,
        external_blockers,
    }))
}

fn read_item_summary_by_id(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<ItemSummary>, rusqlite::Error> {
    conn.query_row(
        "select display_value, title, item_class, status, priority \
           from items where id = ?1",
        params![id],
        item_summary_from_row,
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

fn read_children(
    conn: &rusqlite::Connection,
    parent_id: &str,
) -> Result<Vec<ItemSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "select display_value, title, item_class, status, priority \
           from items \
          where container_id = ?1 \
          order by created_seq asc",
    )?;
    stmt.query_map(params![parent_id], item_summary_from_row)?
        .collect()
}

fn read_blocked_by(
    conn: &rusqlite::Connection,
    item_id: &str,
) -> Result<Vec<ItemSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "select i.display_value, i.title, i.item_class, i.status, i.priority \
           from dependencies d \
           join items i on i.id = d.blocking_id \
          where d.blocked_id = ?1 \
            and i.status <> 'done' \
          order by i.created_seq asc",
    )?;
    stmt.query_map(params![item_id], item_summary_from_row)?
        .collect()
}

fn read_blocking(
    conn: &rusqlite::Connection,
    item_id: &str,
) -> Result<Vec<ItemSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "select i.display_value, i.title, i.item_class, i.status, i.priority \
           from dependencies d \
           join items i on i.id = d.blocked_id \
          where d.blocking_id = ?1 \
            and i.status <> 'done' \
          order by i.created_seq asc",
    )?;
    stmt.query_map(params![item_id], item_summary_from_row)?
        .collect()
}

fn read_external_blockers(
    conn: &rusqlite::Connection,
    item_id: &str,
) -> Result<Vec<ExternalBlockerSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "select reason \
           from external_blockers \
          where item_id = ?1 \
            and resolved_at is null \
          order by created_at asc",
    )?;
    stmt.query_map(params![item_id], |row| {
        Ok(ExternalBlockerSummary {
            reason: row.get(0)?,
        })
    })?
    .collect()
}

fn item_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemSummary> {
    Ok(ItemSummary {
        display_id: row.get(0)?,
        title: row.get(1)?,
        item_class: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, insert_dependency, insert_external_blocker, insert_fixture_item,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store { conn }
    }

    fn seed_ticket(store: &Store, id: &str, display: &str, title: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_epic(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn missing_display_id_returns_none() {
        let store = open_seeded();
        assert!(show_item(&store, "nope").unwrap().is_none());
    }

    #[test]
    fn ticket_detail_includes_kind_and_priority() {
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Ship it", 1);
        let item = show_item(&store, "tk-1").unwrap().unwrap();
        assert_eq!(item.display_id, "tk-1");
        assert_eq!(item.item_class, ItemClass::Ticket);
        assert_eq!(item.ticket_kind, Some(TicketKind::Task));
        assert_eq!(item.priority, Some(Priority::P2));
        assert_eq!(item.selection_state, Some(SelectionState::Accepted));
        assert_eq!(item.status, ItemStatus::Open);
        assert!(item.parent.is_none());
        assert!(item.children.is_empty());
        assert!(item.blocked_by.is_empty());
        assert!(item.blocking.is_empty());
        assert!(item.external_blockers.is_empty());
    }

    #[test]
    fn epic_detail_lists_children_in_creation_order() {
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "child-a",
                display: "tk-2",
                title: "A",
                container_id: Some("epic"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "child-b",
                display: "tk-3",
                title: "B",
                container_id: Some("epic"),
                container_class: Some("epic"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let epic = show_item(&store, "tk-1").unwrap().unwrap();
        assert_eq!(epic.item_class, ItemClass::Epic);
        // Epics stay outside Selection State (ADR-0027).
        assert!(epic.selection_state.is_none());
        assert_eq!(
            epic.children
                .iter()
                .map(|c| c.display_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tk-2", "tk-3"]
        );
    }

    #[test]
    fn child_detail_carries_parent_summary() {
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "child",
                display: "tk-2",
                title: "Child",
                container_id: Some("epic"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let child = show_item(&store, "tk-2").unwrap().unwrap();
        let parent = child.parent.expect("parent expected");
        assert_eq!(parent.display_id, "tk-1");
        assert_eq!(parent.item_class, ItemClass::Epic);
    }

    #[test]
    fn blocked_by_and_blocking_exclude_done_dependencies() {
        let store = open_seeded();
        seed_ticket(&store, "t", "tk-1", "Subject", 1);
        seed_ticket(&store, "blocker-open", "tk-2", "Open blocker", 2);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "blocker-done",
                display: "tk-3",
                title: "Done blocker",
                status: "done",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&store.conn, "blocker-open", "t").unwrap();
        insert_dependency(&store.conn, "blocker-done", "t").unwrap();

        seed_ticket(&store, "blocked-open", "tk-4", "Open blocked", 4);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "blocked-done",
                display: "tk-5",
                title: "Done blocked",
                status: "done",
                created_seq: 5,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&store.conn, "t", "blocked-open").unwrap();
        insert_dependency(&store.conn, "t", "blocked-done").unwrap();

        let detail = show_item(&store, "tk-1").unwrap().unwrap();
        let blocked_by_ids: Vec<&str> = detail
            .blocked_by
            .iter()
            .map(|s| s.display_id.as_str())
            .collect();
        assert_eq!(blocked_by_ids, vec!["tk-2"]);
        let blocking_ids: Vec<&str> = detail
            .blocking
            .iter()
            .map(|s| s.display_id.as_str())
            .collect();
        assert_eq!(blocking_ids, vec!["tk-4"]);
    }

    #[test]
    fn resolves_via_alias_to_the_canonical_display_id() {
        let store = open_seeded();
        seed_ticket(&store, "t", "tk-1", "Subject", 1);
        crate::store::testing::insert_alias(&store.conn, "subject", "t").unwrap();
        let detail = show_item(&store, "subject").unwrap().unwrap();
        assert_eq!(detail.display_id, "tk-1");
        assert_eq!(detail.title, "Subject");
    }

    #[test]
    fn external_blockers_show_only_unresolved() {
        let store = open_seeded();
        seed_ticket(&store, "t", "tk-1", "Subject", 1);
        insert_external_blocker(&store.conn, "eb-unresolved", "t", None).unwrap();
        insert_external_blocker(
            &store.conn,
            "eb-resolved",
            "t",
            Some("2026-05-10T00:00:00.000Z"),
        )
        .unwrap();
        let detail = show_item(&store, "tk-1").unwrap().unwrap();
        let reasons: Vec<&str> = detail
            .external_blockers
            .iter()
            .map(|eb| eb.reason.as_str())
            .collect();
        assert_eq!(reasons, vec!["fixture blocker"]);
    }
}
