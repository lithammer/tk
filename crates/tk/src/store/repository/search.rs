//! `tk search` title-substring lookup (ADR-0025).
//!
//! Finds Tickets and Epics whose title contains the query as a
//! case-insensitive literal substring, across the whole Repository Store
//! and every Item Status. Unlike `tk list`, search applies no view, Origin,
//! class, or Scope filter and never nests a List Tree — the command-side
//! renderer lays the matches out flat. The query returns the same typed
//! [`ListRow`] the List Tree read produces, so both commands share one row
//! renderer.

use rusqlite::params;

use super::Store;
use super::list::{ListRow, row_from_sql};

/// SQL for the title-substring search. Bound with one text parameter (`?1`,
/// the query). `instr(lower(title), lower(?1)) > 0` is a case-insensitive
/// *literal* substring test — the query is never interpreted as a `LIKE`
/// pattern or regex, so `%` and `_` match themselves. The
/// `has_unresolved_blocker`, `has_pending_mutation`, and `has_failed_mutation`
/// expressions mirror the List Tree read so both commands feed the shared row
/// renderer the same derived flags.
const SEARCH_ROWS_SQL: &str = "\
select i.id, i.display_value, i.item_class, i.ticket_kind, i.priority, i.title, \
       i.status, i.origin, i.container_id, i.selection_state, i.created_seq, \
       ( \
           exists ( \
               select 1 \
                 from dependencies d \
                 join items blocking on blocking.id = d.blocking_id \
                where d.blocked_id = i.id \
                  and blocking.status <> 'done' \
           ) \
           or exists ( \
               select 1 \
                 from external_blockers eb \
                where eb.item_id = i.id \
                  and eb.resolved_at is null \
           ) \
       ) as has_unresolved_blocker, \
       exists ( \
           select 1 \
             from mutations m \
            where m.item_id = i.id \
              and m.state = 'pending' \
              and m.mutation_type not in ('promote_ticket', 'promote_epic') \
       ) as has_pending_mutation, \
       exists ( \
           select 1 \
             from mutations m \
            where m.item_id = i.id \
              and m.state = 'failed' \
              and m.mutation_type not in ('promote_ticket', 'promote_epic') \
       ) as has_failed_mutation \
  from items i \
 where instr(lower(i.title), lower(?1)) > 0 \
 order by i.created_seq asc";

/// Read current-state rows whose title contains `query` (case-insensitive
/// literal substring), ordered by `created_seq` ascending. Covers every
/// Item Status, including `done`.
pub fn search_rows(store: &Store, query: &str) -> Result<Vec<ListRow>, rusqlite::Error> {
    let mut stmt = store.conn.prepare(SEARCH_ROWS_SQL)?;
    let rows = stmt
        .query_map(params![query], row_from_sql)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::super::list::{ListOptions, list_rows};
    use super::*;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, insert_external_blocker, insert_fixture_item,
        insert_fixture_mutation,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store::for_test(conn)
    }

    fn seed_ticket(store: &Store, id: &str, display: &str, title: &str, status: &str, seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title,
                status,
                created_seq: seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_epic(store: &Store, id: &str, display: &str, title: &str, status: &str, seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title,
                status,
                created_seq: seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// Seed one Mutation against `item_id`, supplying the `failure_json` the
    /// `failed` state's CHECK requires.
    fn seed_mutation(
        store: &Store,
        sequence: i64,
        item_id: &str,
        mutation_type: &str,
        state: &str,
    ) {
        insert_fixture_mutation(
            &store.conn,
            FixtureMutation {
                sequence,
                mutation_type,
                item_id,
                item_class: "ticket",
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    fn display_ids(rows: &[ListRow]) -> Vec<&str> {
        rows.iter().map(|r| r.display_id.as_str()).collect()
    }

    #[test]
    fn matches_title_substring() {
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Fix the flaky test", "open", 1);
        seed_ticket(&store, "t2", "tk-2", "Unrelated chore", "open", 2);
        let rows = search_rows(&store, "flaky").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn match_is_case_insensitive() {
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Fix the FLAKY test", "open", 1);
        let rows = search_rows(&store, "flaky").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn matches_both_tickets_and_epics() {
        let store = open_seeded();
        seed_epic(&store, "e1", "tk-1", "Auth rework", "open", 1);
        seed_ticket(&store, "t1", "tk-2", "Add auth middleware", "open", 2);
        seed_ticket(&store, "t2", "tk-3", "Unrelated chore", "open", 3);
        let rows = search_rows(&store, "auth").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1", "tk-2"]);
    }

    #[test]
    fn includes_every_item_status() {
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Auth open", "open", 1);
        seed_ticket(&store, "t2", "tk-2", "Auth active", "active", 2);
        seed_ticket(&store, "t3", "tk-3", "Auth done", "done", 3);
        let rows = search_rows(&store, "auth").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1", "tk-2", "tk-3"]);
    }

    #[test]
    fn orders_by_created_seq_ascending() {
        let store = open_seeded();
        seed_ticket(&store, "t2", "tk-2", "Auth second", "open", 2);
        seed_ticket(&store, "t1", "tk-1", "Auth first", "open", 1);
        seed_ticket(&store, "t3", "tk-3", "Auth third", "open", 3);
        let rows = search_rows(&store, "auth").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1", "tk-2", "tk-3"]);
    }

    #[test]
    fn query_is_a_literal_substring_not_a_wildcard() {
        // `%` and `_` are LIKE metacharacters; `instr` treats them literally,
        // so a `50%` query matches only the title that really contains `50%`.
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Migrate 50% of traffic", "open", 1);
        seed_ticket(&store, "t2", "tk-2", "Migrate 50 of traffic", "open", 2);
        let rows = search_rows(&store, "50%").unwrap();
        assert_eq!(display_ids(&rows), vec!["tk-1"]);
    }

    #[test]
    fn no_match_returns_empty() {
        let store = open_seeded();
        seed_ticket(&store, "t1", "tk-1", "Auth rework", "open", 1);
        let rows = search_rows(&store, "nonexistent").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn done_match_still_reports_its_unresolved_blocker() {
        // The store reports `has_unresolved_blocker` truthfully even for a
        // `done` item; suppressing the `⊘` for done rows is the renderer's job
        // (ADR-0025), not the query's.
        let store = open_seeded();
        seed_ticket(
            &store,
            "t1",
            "tk-1",
            "Auth shipped via workaround",
            "done",
            1,
        );
        insert_external_blocker(&store.conn, "eb1", "t1", None).unwrap();
        let rows = search_rows(&store, "auth").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_unresolved_blocker);
    }

    #[test]
    fn done_match_still_reports_its_pending_mutation() {
        // Same split as `done_match_still_reports_its_unresolved_blocker`: a
        // `done` Backend Item can carry a queued Mutation the Backend has not
        // received (ADR-0040), and the store reports that truthfully;
        // suppressing it for done rows is the renderer's job.
        let store = open_seeded();
        seed_ticket(
            &store,
            "t1",
            "tk-1",
            "Auth shipped via workaround",
            "done",
            1,
        );
        insert_fixture_mutation(
            &store.conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                item_class: "ticket",
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let rows = search_rows(&store, "auth").unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_pending_mutation);
        assert!(!rows[0].has_failed_mutation);
    }

    #[test]
    fn search_rows_and_list_rows_agree_on_mutation_flags() {
        // Nothing else guards these two hand-mirrored queries against drift,
        // and the fixture has to discriminate every cell it claims to guard:
        // a single failed non-Promotion Mutation leaves both `pending` values
        // false, so drift in search's pending subquery would compare equal.
        // The Promotion rows are what catch a dropped type exclusion, which
        // search spells separately from the List Tree read.
        let store = open_seeded();
        seed_ticket(&store, "pending-edit", "tk-1", "Auth pending", "open", 1);
        seed_ticket(&store, "failed-edit", "tk-2", "Auth failed", "open", 2);
        seed_ticket(&store, "promo-only", "tk-3", "Auth promoted", "open", 3);
        seed_mutation(&store, 1, "pending-edit", "update_ticket", "pending");
        seed_mutation(&store, 2, "failed-edit", "update_ticket", "failed");
        seed_mutation(&store, 3, "promo-only", "promote_ticket", "pending");
        seed_mutation(&store, 4, "promo-only", "promote_ticket", "failed");

        let search = search_rows(&store, "auth").unwrap();
        let list = list_rows(&store, ListOptions::default()).unwrap();
        assert_eq!(search.len(), 3);
        assert_eq!(list.len(), 3);

        // Pin the values too, so the comparison cannot pass by both sides
        // being uniformly wrong.
        let flags = |rows: &[ListRow], id: &str| {
            let row = rows.iter().find(|r| r.id == id).unwrap();
            (row.has_pending_mutation, row.has_failed_mutation)
        };
        assert_eq!(flags(&search, "pending-edit"), (true, false));
        assert_eq!(flags(&search, "failed-edit"), (false, true));
        assert_eq!(flags(&search, "promo-only"), (false, false));

        for row in &search {
            assert_eq!(
                flags(&search, &row.id),
                flags(&list, &row.id),
                "search and list disagree on {}: the two hand-mirrored \
                 subqueries have drifted",
                row.display_id
            );
        }
    }
}
