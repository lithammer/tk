//! `tk list` List Tree read (`default` / `ready` / `blocked` / `active`).
//!
//! The Repository Store owns filtering, ordering, and the readiness
//! derivation (an Item is *blocked* when it has any unresolved Dependency
//! or unresolved External Blocker). The command-side renderer owns the
//! tree glyph and the compact plain-text row shape, so the query returns
//! a typed [`ListRow`] per match rather than a pre-rendered string.

use rusqlite::params;

use crate::domain::item_class::ItemClass;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;

use super::Store;

/// One current-state row for a List Tree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub id: String,
    pub display_id: String,
    pub item_class: ItemClass,
    pub ticket_kind: Option<TicketKind>,
    pub priority: Option<Priority>,
    pub title: String,
    pub status: ItemStatus,
    pub origin: Origin,
    /// Internal stable ID of the parent Epic, if any.
    pub container_id: Option<String>,
    pub created_seq: i64,
    pub has_unresolved_blocker: bool,
}

/// Item-selection mode for `tk list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListView {
    /// Open + Active items (no Done unless under a matching Epic in another view).
    #[default]
    Default,
    /// Tickets that are open and free of unresolved blockers.
    Ready,
    /// Open or Active Tickets blocked by Dependency or External Blocker.
    Blocked,
    /// Items with status = active.
    Active,
}

impl ListView {
    fn sql_text(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Active => "active",
        }
    }
}

/// Stored-Origin filter for `tk list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListOriginFilter {
    #[default]
    Any,
    Local,
    /// User-facing flag is `--remote`; storage column is `backend`. The
    /// public name matches the CLI flag.
    Remote,
}

impl ListOriginFilter {
    fn sql_text(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Local => "local",
            Self::Remote => "backend",
        }
    }
}

/// Item-class filter for `tk list`. Orthogonal to [`ListView`] and
/// [`ListOriginFilter`] — it narrows the result set to Epics without
/// changing which view or Origin is selected, so it composes with any
/// of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListClassFilter {
    #[default]
    Any,
    Epic,
}

impl ListClassFilter {
    fn sql_text(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Epic => "epic",
        }
    }
}

/// Read options for the List Tree query.
///
/// `scope` is the stable `items.id` of an Epic when a Scope is active
/// (ADR-0022), confining rows to that Epic and its direct child Tickets;
/// `None` reads the whole store. The command layer resolves and
/// Epic-validates the `<epic-id>` argument / `TK_SCOPE` before setting it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListOptions<'a> {
    pub view: ListView,
    pub origin: ListOriginFilter,
    pub class: ListClassFilter,
    pub scope: Option<&'a str>,
}

/// SQL for the List Tree read. Bound with four text parameters:
///   `?1` — [`ListView::sql_text`] selecting which items match.
///   `?2` — [`ListOriginFilter::sql_text`] filtering stored Origin.
///   `?3` — [`ListClassFilter::sql_text`] narrowing to a single Item class.
///   `?4` — Scope (ADR-0022): the stable `items.id` of an Epic, or `''`
///          for no Scope. When set, rows are confined to that Epic and its
///          direct child Tickets.
///
/// The `case ?1 when '<tag>' then ...` arms cover every [`ListView`]
/// variant; the test below catches a missing arm.
const LIST_ROWS_SQL: &str = "\
with annotated as ( \
    select i.id, i.display_value, i.item_class, i.ticket_kind, \
           i.priority, i.title, i.status, i.origin, i.container_id, \
           i.created_seq, \
           exists ( \
               select 1 \
                 from dependencies d \
                 join items blocking on blocking.id = d.blocking_id \
                where d.blocked_id = i.id \
                  and blocking.status <> 'done' \
           ) as has_unresolved_dependency, \
           exists ( \
               select 1 \
                 from external_blockers eb \
                where eb.item_id = i.id \
                  and eb.resolved_at is null \
           ) as has_unresolved_external_blocker \
      from items i \
), \
matching as ( \
    select *, \
           case ?1 \
             when 'default' then status in ('open', 'active') \
             when 'ready' then item_class = 'ticket' \
                               and status = 'open' \
                               and not has_unresolved_dependency \
                               and not has_unresolved_external_blocker \
             when 'blocked' then item_class = 'ticket' \
                                 and status in ('open', 'active') \
                                 and ( \
                                     has_unresolved_dependency \
                                     or has_unresolved_external_blocker \
                                 ) \
             when 'active' then status = 'active' \
           end as self_matches \
      from annotated \
) \
select id, display_value, item_class, ticket_kind, priority, title, \
       status, origin, container_id, created_seq, \
       (has_unresolved_dependency or has_unresolved_external_blocker) as has_unresolved_blocker \
  from matching parent \
 where (?2 = 'any' or parent.origin = ?2) \
   and (?3 = 'any' or parent.item_class = ?3) \
   and (?4 = '' or parent.id = ?4 or parent.container_id = ?4) \
   and ( \
       parent.self_matches \
       or ( \
           ?1 in ('ready', 'blocked', 'active') \
           and \
           parent.item_class = 'epic' \
           and exists ( \
               select 1 \
                 from matching child \
                where child.container_id = parent.id \
                  and child.self_matches \
                  and (?2 = 'any' or child.origin = ?2) \
           ) \
       ) \
   ) \
 order by created_seq asc";

/// Read current-state rows for the List Tree.
pub fn list_rows(store: &Store, options: ListOptions<'_>) -> Result<Vec<ListRow>, rusqlite::Error> {
    let mut stmt = store.conn.prepare(LIST_ROWS_SQL)?;
    let rows = stmt
        .query_map(
            params![
                options.view.sql_text(),
                options.origin.sql_text(),
                options.class.sql_text(),
                options.scope.unwrap_or(""),
            ],
            row_from_sql,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ListRow> {
    let has_unresolved_blocker: i64 = row.get(10)?;
    Ok(ListRow {
        id: row.get(0)?,
        display_id: row.get(1)?,
        item_class: row.get(2)?,
        ticket_kind: row.get(3)?,
        priority: row.get(4)?,
        title: row.get(5)?,
        status: row.get(6)?,
        origin: row.get(7)?,
        container_id: row.get(8)?,
        created_seq: row.get(9)?,
        has_unresolved_blocker: has_unresolved_blocker != 0,
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

    fn seed_ticket(store: &Store, id: &str, display: &str, status: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: id,
                status,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_epic(store: &Store, id: &str, display: &str, status: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: id,
                status,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn display_ids(rows: &[ListRow]) -> Vec<&str> {
        rows.iter().map(|r| r.display_id.as_str()).collect()
    }

    #[test]
    fn default_view_returns_open_and_active_items() {
        let store = open_seeded();
        seed_ticket(&store, "open-t", "tk-1", "open", 1);
        seed_ticket(&store, "active-t", "tk-2", "active", 2);
        seed_ticket(&store, "done-t", "tk-3", "done", 3);
        let rows = list_rows(&store, ListOptions::default()).unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1", "tk-2"]);
    }

    #[test]
    fn ready_excludes_tickets_with_unresolved_dependencies() {
        let store = open_seeded();
        seed_ticket(&store, "ready", "tk-1", "open", 1);
        seed_ticket(&store, "blocked-dep", "tk-2", "open", 2);
        seed_epic(&store, "open-blocker", "tk-3", "open", 3);
        insert_dependency(&store.conn, "open-blocker", "blocked-dep").unwrap();

        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn ready_excludes_tickets_with_unresolved_external_blockers() {
        let store = open_seeded();
        seed_ticket(&store, "ready", "tk-1", "open", 1);
        seed_ticket(&store, "blocked-ext", "tk-2", "open", 2);
        insert_external_blocker(&store.conn, "eb1", "blocked-ext", None).unwrap();

        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn resolved_blockers_do_not_count() {
        let store = open_seeded();
        seed_ticket(&store, "ticket", "tk-1", "open", 1);
        seed_epic(&store, "done-blocker", "tk-2", "done", 2);
        insert_dependency(&store.conn, "done-blocker", "ticket").unwrap();

        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn blocked_view_surfaces_tickets_with_any_unresolved_blocker() {
        let store = open_seeded();
        seed_ticket(&store, "ready", "tk-1", "open", 1);
        seed_ticket(&store, "blocked-dep", "tk-2", "open", 2);
        seed_epic(&store, "open-blocker", "tk-3", "open", 3);
        insert_dependency(&store.conn, "open-blocker", "blocked-dep").unwrap();
        seed_ticket(&store, "blocked-ext", "tk-4", "open", 4);
        insert_external_blocker(&store.conn, "eb", "blocked-ext", None).unwrap();

        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Blocked,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-2", "tk-4"]);
    }

    #[test]
    fn ready_includes_parent_epic_when_one_child_is_ready() {
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "open", 1);
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
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                ..ListOptions::default()
            },
        )
        .unwrap();
        // Parent Epic surfaces alongside the child Ticket; the renderer
        // uses the parent to plot the tree.
        assert!(display_ids(&rows).contains(&"tk-1"));
        assert!(display_ids(&rows).contains(&"tk-2"));
    }

    #[test]
    fn origin_local_filter_excludes_backend_rows() {
        let store = open_seeded();
        seed_ticket(&store, "local", "tk-1", "open", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "backend",
                display: "tk-2",
                title: "Backend",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let rows = list_rows(
            &store,
            ListOptions {
                origin: ListOriginFilter::Local,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn epic_class_filter_returns_only_epics() {
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "open", 1);
        seed_ticket(&store, "ticket", "tk-2", "open", 2);
        let rows = list_rows(
            &store,
            ListOptions {
                class: ListClassFilter::Epic,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn epic_class_filter_keeps_parent_epic_in_ready_view() {
        // The epic-parent-inclusion branch surfaces an Epic whose child is
        // ready; the class filter drops the child Ticket but keeps the Epic,
        // so `--ready --epic` answers "which Epics contain ready work?".
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "open", 1);
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
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                class: ListClassFilter::Epic,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn ready_epic_origin_filter_applies_to_child_subquery() {
        // The epic-parent-inclusion branch re-applies the Origin filter to the
        // child (`child.origin = ?2`), so under `--ready --epic --local` a Local
        // Epic surfaces only when it has a ready *Local* child — not merely any
        // ready child. Both Epics here are Local (they pass the parent Origin
        // filter); they differ only in the Origin of their ready child.
        let store = open_seeded();
        seed_epic(&store, "epic-local-child", "tk-1", "open", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "local-child",
                display: "tk-2",
                title: "Local child",
                container_id: Some("epic-local-child"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_epic(&store, "epic-backend-child", "tk-3", "open", 3);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "backend-child",
                display: "tk-4",
                title: "Backend child",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                container_id: Some("epic-backend-child"),
                container_class: Some("epic"),
                created_seq: 4,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Ready,
                class: ListClassFilter::Epic,
                origin: ListOriginFilter::Local,
                scope: None,
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn scope_confines_rows_to_the_epic_and_its_children() {
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "open", 1);
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
        // An unrelated Epic and a top-level Ticket must be excluded.
        seed_epic(&store, "other-epic", "tk-3", "open", 3);
        seed_ticket(&store, "loose", "tk-4", "open", 4);

        let rows = list_rows(
            &store,
            ListOptions {
                scope: Some("epic"),
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1", "tk-2"]);
    }

    #[test]
    fn active_view_surfaces_only_active_status_rows() {
        let store = open_seeded();
        seed_ticket(&store, "open-t", "tk-1", "open", 1);
        seed_ticket(&store, "active-t", "tk-2", "active", 2);
        seed_ticket(&store, "done-t", "tk-3", "done", 3);
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Active,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-2"]);
    }

    #[test]
    fn every_view_variant_has_a_case_arm_in_sql() {
        // Drive each view through the query so a missing `when` arm in
        // LIST_ROWS_SQL surfaces as a SQLite NULL → row-filter mismatch.
        let store = open_seeded();
        for view in [
            ListView::Default,
            ListView::Ready,
            ListView::Blocked,
            ListView::Active,
        ] {
            list_rows(
                &store,
                ListOptions {
                    view,
                    ..ListOptions::default()
                },
            )
            .unwrap_or_else(|err| panic!("variant {view:?} failed: {err}"));
        }
    }
}
