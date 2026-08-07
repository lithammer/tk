//! Promotion receipt application: converting a Local Item into a Backend Item
//! in place once its Promotion Mutation has been applied (ADR-0036).
//!
//! [`apply_promotion_receipt`] runs inside the caller's open transaction — it
//! takes a borrowed connection and neither begins nor commits one — so the
//! conversion commits together with the Mutation Log state transition that
//! records the Promotion as applied. No window exists in which a Mutation is
//! `applied` while its Item is still Local.

use rusqlite::{Connection, params};

use crate::domain::apply_outcome::PromotionReceipt;
use crate::domain::origin::Origin;

/// Convert the Local Item `item_id` into a Backend Item using the identity its
/// Promotion receipt carries: store `backend_kind` / `receipt.backend_key`,
/// replace the Display ID with `receipt.display_id`, and keep the outgoing
/// Display ID resolvable as an Alias (CONTEXT.md Promotion).
///
/// Selection State, Priority, status, title, and body are Local Fields that
/// survive Promotion untouched; leaving `status` out of the `items` update also
/// keeps the `items_no_escape_from_done` trigger (which fires `before update of
/// status`) out of this path.
///
/// A Display ID the receipt names but another Item already claims fails the
/// `item_ids` insert; the error propagates so the caller's transaction rolls
/// back with the Mutation still applicable (ADR-0036 Consequences).
pub fn apply_promotion_receipt(
    conn: &Connection,
    item_id: &str,
    backend_kind: &str,
    receipt: &PromotionReceipt,
    now: &str,
) -> rusqlite::Result<()> {
    // Statement order is load-bearing. `item_ids_one_display_per_item` is a
    // plain partial unique index on (item_id) where source = 'display' and is
    // not deferrable, so the outgoing row must be demoted to an Alias *before*
    // the receipt's Display ID is inserted; the reverse order fails with
    // `UNIQUE constraint failed: item_ids.item_id`. The composite foreign key
    // from `items.display_value` into `item_ids` is deferred, which is what
    // tolerates the window where `items` still points at a row that has just
    // become an Alias.
    conn.execute(
        "update item_ids set source = 'alias' where item_id = ?1 and source = 'display'",
        params![item_id],
    )?;
    conn.execute(
        "insert into item_ids(value, source, item_id, created_at) \
         values (?1, 'display', ?2, ?3)",
        params![receipt.display_id, item_id, now],
    )?;
    // The `items` Origin CHECK requires backend_kind and backend_key to be
    // non-null exactly when origin = 'backend', so all three move together.
    conn.execute(
        "update items \
            set origin = ?2, backend_kind = ?3, backend_key = ?4, \
                display_value = ?5, updated_at = ?6 \
          where id = ?1",
        params![
            item_id,
            Origin::Backend.text(),
            backend_kind,
            receipt.backend_key,
            receipt.display_id,
            now,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use crate::store::repository::resolve_item_ref;
    use crate::store::testing::{FixtureItem, insert_fixture_item};

    const NOW: &str = "2026-06-01T00:00:00Z";

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn local_ticket(conn: &Connection, id: &str, display: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Local work",
                selection_state: Some("parked"),
                priority: Some("P0"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn receipt(backend_key: &str, display_id: &str) -> PromotionReceipt {
        PromotionReceipt {
            backend_key: backend_key.into(),
            display_id: display_id.into(),
        }
    }

    /// Drive the receipt through a caller-owned transaction, the shape
    /// [`apply_promotion_receipt`] contracts for: the deferred `items` →
    /// `item_ids` foreign key is only tolerated mid-transaction, and a failure
    /// rolls the whole conversion back.
    fn promote(
        conn: &mut Connection,
        item_id: &str,
        receipt: &PromotionReceipt,
    ) -> rusqlite::Result<()> {
        let tx = crate::store::write_transaction(conn)?;
        apply_promotion_receipt(&tx, item_id, "github", receipt, NOW)?;
        tx.commit()
    }

    #[test]
    fn receipt_makes_the_item_a_backend_item_and_replaces_the_display_id() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (display, origin, kind, key, updated): (String, String, String, String, String) = conn
            .query_row(
                "select display_value, origin, backend_kind, backend_key, updated_at \
                   from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-42");
        assert_eq!(origin, "backend");
        assert_eq!(kind, "github");
        assert_eq!(key, "42");
        assert_eq!(updated, NOW);
    }

    #[test]
    fn the_outgoing_display_id_still_resolves_as_an_alias() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        // Both identifiers reach the same Item; the local one is the Alias.
        assert_eq!(resolve_item_ref(&conn, "tk-1").unwrap().unwrap().id, "t1");
        assert_eq!(resolve_item_ref(&conn, "gh-42").unwrap().unwrap().id, "t1");

        let mut stmt = conn
            .prepare("select value, source from item_ids where item_id = 't1' order by value")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("gh-42".to_string(), "display".to_string()),
                ("tk-1".to_string(), "alias".to_string()),
            ],
            "exactly one display row and one alias row after the swap"
        );
    }

    #[test]
    fn local_fields_survive_promotion() {
        // Selection State and Priority are Local Fields (ADR-0027) that
        // Promotion preserves; status/title/body are not the receipt's to move.
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (selection, priority, status, title): (String, String, String, String) = conn
            .query_row(
                "select selection_state, priority, status, title from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(selection, "parked");
        assert_eq!(priority, "P0");
        assert_eq!(status, "open");
        assert_eq!(title, "Local work");
    }

    #[test]
    fn a_display_id_another_item_claims_is_refused_and_rolls_back() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "gh-42",
                title: "Squatter",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let err = promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap_err();
        assert!(
            matches!(
                err,
                rusqlite::Error::SqliteFailure(e, _)
                    if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                        || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ),
            "expected item_ids to refuse the already-claimed Display ID, got {err:?}"
        );

        // The demote is part of the same transaction, so the Item is untouched.
        let (display, origin): (String, String) = conn
            .query_row(
                "select display_value, origin from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(display, "tk-1");
        assert_eq!(origin, "local");
        let source: String = conn
            .query_row(
                "select source from item_ids where value = 'tk-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "display", "the demote rolled back with the insert");
    }

    #[test]
    fn promoting_an_epic_keeps_its_null_ticket_only_columns() {
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Local epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        promote(&mut conn, "e1", &receipt("7", "gh-7")).unwrap();

        let (display, origin, selection): (String, String, Option<String>) = conn
            .query_row(
                "select display_value, origin, selection_state from items where id = 'e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-7");
        assert_eq!(origin, "backend");
        assert_eq!(selection, None, "Epics stay outside Selection State");
    }
}
