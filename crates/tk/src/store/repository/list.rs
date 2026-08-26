//! `tk list` List Tree read (`default` / `ready` / `blocked` / `active` /
//! `triage` / `parked`).
//!
//! The Repository Store owns filtering, ordering, and the readiness
//! derivation (an Item is *blocked* when it has any unresolved Dependency
//! or unresolved External Blocker). The command-side renderer owns the
//! tree glyph and the compact plain-text row shape, so the query returns
//! a typed [`ListRow`] per match rather than a pre-rendered string.

use rusqlite::params;

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
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
    /// Internal stable ID of the parent Epic, if any.
    pub container_id: Option<String>,
    /// Local-only Selection State; `None` for Epics (ADR-0027). Drives the dim
    /// `[parked]` list badge.
    pub selection_state: Option<SelectionState>,
    pub has_unresolved_blocker: bool,
    /// True when the Item has a `pending` Mutation whose type is not a
    /// Promotion (`promote_ticket` / `promote_epic`). The Promotion
    /// exclusion is part of the flag's meaning, not a display choice: it
    /// keeps "a queued edit to an existing Backend object" from being
    /// conflated with "this Item is not on the Backend at all". There is no
    /// Origin filter — `is_backend_bound()` is true for a Pending Promotion,
    /// so a Local Item can carry non-Promotion Mutations queued behind it,
    /// and those are genuinely unsent.
    pub has_pending_mutation: bool,
    /// Same contract as `has_pending_mutation`, for the `failed` state.
    pub has_failed_mutation: bool,
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
    /// Open Tickets in triage (captured, not yet accepted).
    Triage,
    /// Open Tickets parked out of automatic selection.
    Parked,
}

impl ListView {
    fn sql_text(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Active => "active",
            Self::Triage => "triage",
            Self::Parked => "parked",
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
/// variant; the per-view tests below cover the arms.
const LIST_ROWS_SQL: &str = "\
with annotated as ( \
    select i.id, i.display_value, i.item_class, i.ticket_kind, \
           i.priority, i.title, i.status, i.origin, i.container_id, \
           i.selection_state, i.created_seq, \
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
                               and selection_state = 'accepted' \
                               and not has_unresolved_dependency \
                               and not has_unresolved_external_blocker \
             when 'blocked' then item_class = 'ticket' \
                                 and status in ('open', 'active') \
                                 and selection_state <> 'triage' \
                                 and ( \
                                     has_unresolved_dependency \
                                     or has_unresolved_external_blocker \
                                 ) \
             when 'active' then status = 'active' \
             when 'triage' then item_class = 'ticket' \
                                and status = 'open' \
                                and selection_state = 'triage' \
             when 'parked' then item_class = 'ticket' \
                                and status = 'open' \
                                and selection_state = 'parked' \
           end as self_matches \
      from annotated \
) \
select id, display_value, item_class, ticket_kind, priority, title, \
       status, container_id, selection_state, \
       (has_unresolved_dependency or has_unresolved_external_blocker) as has_unresolved_blocker, \
       exists ( \
           select 1 \
             from mutations m \
            where m.item_id = parent.id \
              and m.state = 'pending' \
              and m.mutation_type not in ('promote_ticket', 'promote_epic') \
       ) as has_pending_mutation, \
       exists ( \
           select 1 \
             from mutations m \
            where m.item_id = parent.id \
              and m.state = 'failed' \
              and m.mutation_type not in ('promote_ticket', 'promote_epic') \
       ) as has_failed_mutation \
  from matching parent \
 where (?2 = 'any' or parent.origin = ?2) \
   and (?3 = 'any' or parent.item_class = ?3) \
   and (?4 = '' or parent.id = ?4 or parent.container_id = ?4) \
   and ( \
       parent.self_matches \
       or ( \
           ?1 in ('ready', 'blocked', 'active', 'triage', 'parked') \
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

pub(super) fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ListRow> {
    Ok(ListRow {
        id: row.get(0)?,
        display_id: row.get(1)?,
        item_class: row.get(2)?,
        ticket_kind: row.get(3)?,
        priority: row.get(4)?,
        title: row.get(5)?,
        status: row.get(6)?,
        container_id: row.get(7)?,
        selection_state: row.get(8)?,
        has_unresolved_blocker: row.get(9)?,
        has_pending_mutation: row.get(10)?,
        has_failed_mutation: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::mutation_state::MutationState;
    use crate::domain::mutation_type::MutationType;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, insert_dependency, insert_external_blocker,
        insert_fixture_item, insert_fixture_mutation,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store::for_test(conn)
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

    fn seed_mutation(
        store: &Store,
        sequence: i64,
        item_id: &str,
        item_class: &str,
        mutation_type: &str,
        state: &str,
    ) {
        insert_fixture_mutation(
            &store.conn,
            FixtureMutation {
                sequence,
                mutation_type,
                item_id,
                item_class,
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
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
    fn ready_view_still_surfaces_a_done_epic_parent() {
        // Characterizes what LIST_ROWS_SQL does today, not what it should
        // do: the epic-parent-inclusion branch above carries no status
        // predicate on the parent, so a `done` Epic with a ready child still
        // surfaces here (tk-163). If this test starts failing, tk-163 added
        // that predicate on purpose — invert the assertion, don't chase a
        // regression.
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "done", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "child",
                display: "tk-2",
                title: "Child",
                container_id: Some("epic"),
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

    fn seed_triage(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: None,
                selection_state: Some("triage"),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn ready_excludes_triage_tickets() {
        let store = open_seeded();
        seed_ticket(&store, "accepted", "tk-1", "open", 1);
        seed_triage(&store, "triage", "tk-2", 2);
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
    fn triage_view_shows_only_triage_tickets() {
        let store = open_seeded();
        seed_ticket(&store, "accepted", "tk-1", "open", 1);
        seed_triage(&store, "triage", "tk-2", 2);
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Triage,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-2"]);
    }

    fn seed_parked(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: id,
                selection_state: Some("parked"),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn ready_excludes_parked_tickets() {
        // Mirror of `ready_excludes_triage_tickets`: the ready arm keys on
        // `selection_state = 'accepted'`, so parked (held) work is no more
        // selectable than triage (tk-75 AC).
        let store = open_seeded();
        seed_ticket(&store, "accepted", "tk-1", "open", 1);
        seed_parked(&store, "parked", "tk-2", 2);
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
    fn parked_view_shows_only_parked_tickets() {
        let store = open_seeded();
        seed_ticket(&store, "accepted", "tk-1", "open", 1);
        seed_triage(&store, "triage", "tk-2", 2);
        seed_parked(&store, "parked", "tk-3", 3);
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Parked,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-3"]);
    }

    #[test]
    fn blocked_includes_parked_with_unresolved_blocker() {
        // tk-75 AC: `tk list --blocked` surfaces parked Tickets that carry an
        // unresolved blocker, alongside accepted ones (the blocked arm matches
        // `selection_state <> 'triage'`).
        let store = open_seeded();
        seed_parked(&store, "parked", "tk-1", 1);
        seed_epic(&store, "blocker", "tk-2", "open", 2);
        insert_dependency(&store.conn, "blocker", "parked").unwrap();
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Blocked,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn blocked_excludes_triage_but_keeps_accepted() {
        let store = open_seeded();
        // A blocked accepted Ticket and a blocked triage Ticket; only the
        // accepted one belongs in the blocked view (ADR-0027).
        seed_ticket(&store, "accepted", "tk-1", "open", 1);
        seed_triage(&store, "triage", "tk-2", 2);
        seed_epic(&store, "blocker", "tk-3", "open", 3);
        insert_dependency(&store.conn, "blocker", "accepted").unwrap();
        insert_dependency(&store.conn, "blocker", "triage").unwrap();
        let rows = list_rows(
            &store,
            ListOptions {
                view: ListView::Blocked,
                ..ListOptions::default()
            },
        )
        .unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
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
        // Drive each view through the query so a malformed LIST_ROWS_SQL — bad
        // syntax, wrong bind count — fails here. A missing `when` arm is not
        // caught: it yields NULL for `self_matches`, and this store holds no
        // items, so the result is empty either way. The arms themselves are
        // covered by the per-view tests above.
        let store = open_seeded();
        for view in [
            ListView::Default,
            ListView::Ready,
            ListView::Blocked,
            ListView::Active,
            ListView::Triage,
            ListView::Parked,
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

    #[test]
    fn every_mutation_state_drives_the_pending_and_failed_flags() {
        // Driven off `MutationState::ALL` so a state added later fails here
        // rather than going silently untested. `applying` and `abandoned` are
        // seeded on `promote_ticket` because migration 010's CHECK confines
        // them there; they cover those two states, not the Promotion
        // exclusion — neither state matches either subquery, so both flags
        // stay clear whether the exclusion is there or not. The exclusion is
        // guarded by `every_promotion_mutation_type_is_excluded_from_the_flags`.
        let store = open_seeded();
        let mut expected = Vec::new();
        for (i, state) in MutationState::ALL.into_iter().enumerate() {
            let seq = i64::try_from(i).unwrap() + 1;
            let (mutation_type, expect_pending, expect_failed) = match state {
                MutationState::Pending => ("update_ticket", true, false),
                MutationState::Failed => ("update_ticket", false, true),
                MutationState::Applied | MutationState::Skipped | MutationState::Cancelled => {
                    ("update_ticket", false, false)
                }
                MutationState::Applying | MutationState::Abandoned => {
                    ("promote_ticket", false, false)
                }
            };
            let item_id = state.text();
            seed_ticket(&store, item_id, &format!("tk-{seq}"), "open", seq);
            seed_mutation(&store, seq, item_id, "ticket", mutation_type, state.text());
            expected.push((state, mutation_type, expect_pending, expect_failed));
        }

        let rows = list_rows(&store, ListOptions::default()).unwrap();
        for (state, mutation_type, expect_pending, expect_failed) in expected {
            let row = rows.iter().find(|r| r.id == state.text()).unwrap();
            assert_eq!(
                row.has_pending_mutation, expect_pending,
                "state={state} type={mutation_type}"
            );
            assert_eq!(
                row.has_failed_mutation, expect_failed,
                "state={state} type={mutation_type}"
            );
        }
    }

    #[test]
    fn every_promotion_mutation_type_is_excluded_from_the_flags() {
        // The queries spell the excluded types as a literal; this drives the
        // same set off `MutationType::is_promotion`, so a third Promotion kind
        // added later fails here rather than silently lighting a marker on an
        // Item that is not on the Backend at all.
        let store = open_seeded();
        seed_ticket(&store, "t", "tk-1", "open", 1);
        seed_epic(&store, "e", "tk-2", "open", 2);

        let promotions: Vec<MutationType> = MutationType::ALL
            .into_iter()
            .filter(|t| t.is_promotion())
            .collect();
        assert!(!promotions.is_empty(), "no Promotion types to check");

        // Both states the flags key on, because the two subqueries carry the
        // exclusion separately: seeding only `pending` leaves the `failed`
        // one unguarded.
        let mut seq = 0;
        for mutation_type in promotions {
            // The composite foreign key pins item_class to the target's class.
            let (item_id, item_class) = match mutation_type {
                MutationType::PromoteTicket => ("t", "ticket"),
                MutationType::PromoteEpic => ("e", "epic"),
                other => panic!(
                    "new Promotion type {} needs a fixture target Item here",
                    other.text()
                ),
            };
            for state in ["pending", "failed"] {
                seq += 1;
                seed_mutation(
                    &store,
                    seq,
                    item_id,
                    item_class,
                    mutation_type.text(),
                    state,
                );
            }
        }

        let rows = list_rows(&store, ListOptions::default()).unwrap();
        for row in &rows {
            assert!(
                !row.has_pending_mutation && !row.has_failed_mutation,
                "{} lit a marker from a Promotion alone",
                row.display_id
            );
        }
    }

    #[test]
    fn pending_promotion_with_a_later_edit_sets_pending() {
        // `is_backend_bound()` is true for a Pending Promotion, so nothing
        // filters this Local Item's later edit out — it is unsent work
        // exactly as pending as one queued behind a Backend Item.
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "open", 1);
        seed_mutation(&store, 1, "t1", "ticket", "promote_ticket", "pending");
        seed_mutation(&store, 2, "t1", "ticket", "set_item_status", "pending");
        let rows = list_rows(&store, ListOptions::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_pending_mutation);
        assert!(!rows[0].has_failed_mutation);
    }

    #[test]
    fn epic_with_its_own_failed_mutation_reports_failed() {
        let store = open_seeded();
        seed_epic(&store, "e1", "tk-1", "open", 1);
        seed_mutation(&store, 1, "e1", "epic", "update_epic", "failed");
        let rows = list_rows(&store, ListOptions::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_failed_mutation);
        assert!(!rows[0].has_pending_mutation);
    }

    #[test]
    fn epic_parent_does_not_inherit_a_childs_pending_mutation() {
        // The flag is per-Item, not a subtree rollup: a child's unsent edit
        // must not make the parent Epic's own row look unsent.
        let store = open_seeded();
        seed_epic(&store, "epic", "tk-1", "open", 1);
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id: "child",
                display: "tk-2",
                title: "Child",
                container_id: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&store, 1, "child", "ticket", "update_ticket", "pending");
        let rows = list_rows(&store, ListOptions::default()).unwrap();
        let epic_row = rows.iter().find(|r| r.id == "epic").unwrap();
        assert!(!epic_row.has_pending_mutation);
        assert!(!epic_row.has_failed_mutation);
    }
}
