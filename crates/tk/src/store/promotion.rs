//! Store-side Promotion helpers: the preflight graph read at the start of
//! `tk promote`, the outbox commit that follows planning, and receipt
//! application at the end of it (ADR-0035, ADR-0036).
//!
//! [`read_graph`] gathers the facts
//! [`crate::domain::promotion_graph::PromotionGraph`] carries; the planner that
//! reasons over them owns every decision.
//!
//! [`commit_plan`] writes the planner's ordered
//! [`crate::domain::promotion_plan::PromotionPlan`] to the Mutation Log outbox
//! as one Promotion Operation, in the single local transaction ADR-0035
//! requires before any Backend call.
//!
//! [`apply_receipt`] runs inside the caller's open transaction — it
//! takes a borrowed connection and neither begins nor commits one — so the
//! conversion commits together with the Mutation Log state transition that
//! records the Promotion as applied. No window exists in which a Mutation is
//! `applied` while its Item is still Local.
//!
//! [`earliest_applicable_mutation`] and [`unresolved_in_operation`]
//! close the loop: after the sync that follows the commit, they are how the
//! command tells a Promotion queued behind an older Mutation from one whose
//! own Mutations did not land.

use std::collections::BTreeSet;

use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::apply_outcome::PromotionReceipt;
use crate::domain::mutation_state::MutationState;
use crate::domain::origin::Origin;
use crate::domain::promotion_graph::{GraphDependency, GraphItem, PromotionGraph};
use crate::domain::promotion_plan::PromotionPlan;
use crate::store::mutations;
use crate::store::repository::create::generate_internal_id;

/// Error returned by [`read_graph`].
#[derive(Debug, Error)]
pub enum ReadGraphError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
}

/// Snapshot the Items and Dependency edges a `tk promote` of `target_id`
/// preflights over. `children_requested` mirrors `--children`.
///
/// Membership, in order of how it is built:
///
/// - the target;
/// - every Item the target directly contains, whether or not children were
///   requested. `--children` decides which contained Tickets are Promotion
///   Children, but promoting an Epic also snapshots membership for the
///   Tickets it already contains on the same Backend, so the planner needs
///   them either way. Only an Epic can contain Items, so a Ticket target
///   contributes none, and Origin is not filtered here because both
///   questions are the planner's;
/// - the target's containing Epic, when it has one — Epic membership is a
///   facet the operation has to decide about;
/// - both endpoints of every Dependency edge that touches any of the above.
///
/// Edges are collected from that pre-expansion set alone: Promotion inspects
/// one hop out to judge the resulting graph but never follows Dependencies to
/// discover further items (ADR-0035). That bound is what keeps both endpoints
/// of every returned edge present in `items`.
///
/// Edges whose Blocking Item is `done` are retained — a done Blocking Item
/// resolves readiness without removing its Dependency (ADR-0035), so the
/// resulting backend graph still needs it. The `tk show` dependency reads
/// filter `status <> 'done'` because they answer a readiness question; this
/// one does not.
///
/// A `target_id` no `items` row matches is a caller fault (the command
/// resolves the Display ID first) and surfaces as
/// [`ReadGraphError::Storage`].
pub fn read_graph(
    conn: &Connection,
    target_id: &str,
    children_requested: bool,
) -> Result<PromotionGraph, ReadGraphError> {
    let mut core: BTreeSet<String> = BTreeSet::new();
    core.insert(target_id.to_owned());

    let container_id: Option<String> = conn.query_row(
        "select container_id from items where id = ?1",
        params![target_id],
        |r| r.get(0),
    )?;
    if let Some(container_id) = container_id {
        core.insert(container_id);
    }

    {
        let mut stmt = conn.prepare("select id from items where container_id = ?1")?;
        let rows = stmt.query_map(params![target_id], |r| r.get::<_, String>(0))?;
        for id in rows {
            core.insert(id?);
        }
    }

    // An edge with both endpoints in `core` matches this query twice; the set
    // collapses it, and orders the result deterministically for a caller that
    // does not re-sort.
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    let mut stmt = conn.prepare(
        "select blocked_id, blocking_id from dependencies \
          where blocked_id = ?1 or blocking_id = ?1",
    )?;
    for id in &core {
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for edge in rows {
            edges.insert(edge?);
        }
    }

    let mut item_ids = core;
    for (blocked_id, blocking_id) in &edges {
        item_ids.insert(blocked_id.clone());
        item_ids.insert(blocking_id.clone());
    }

    let mut items = Vec::with_capacity(item_ids.len());
    for id in &item_ids {
        items.push(read_graph_item(conn, id)?);
    }
    items.sort_by_key(|item| item.created_seq);

    Ok(PromotionGraph {
        target_id: target_id.to_owned(),
        children_requested,
        items,
        dependencies: edges
            .into_iter()
            .map(|(blocked_id, blocking_id)| GraphDependency {
                blocked_id,
                blocking_id,
            })
            .collect(),
    })
}

fn read_graph_item(conn: &Connection, id: &str) -> Result<GraphItem, ReadGraphError> {
    let backend_binding = mutations::resolve_backend_intent(conn, id)?;
    let item = conn.query_row(
        "select display_value, item_class, ticket_kind, selection_state, status, \
                title, body, created_seq, container_id \
           from items where id = ?1",
        params![id],
        |r| {
            Ok(GraphItem {
                id: id.to_owned(),
                display_id: r.get(0)?,
                item_class: r.get(1)?,
                ticket_kind: r.get(2)?,
                selection_state: r.get(3)?,
                status: r.get(4)?,
                title: r.get(5)?,
                body: r.get(6)?,
                created_seq: r.get(7)?,
                container_id: r.get(8)?,
                backend_binding,
            })
        },
    )?;
    Ok(item)
}

/// Error returned by [`commit_plan`].
#[derive(Debug, Error)]
pub enum CommitPlanError {
    /// Underlying SQLite error opening or committing the transaction.
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// Underlying error from appending one draft to the outbox.
    #[error(transparent)]
    Append(#[from] mutations::AppendError),
}

/// Commit `plan` to the Mutation Log outbox as one Promotion Operation, in
/// the single local transaction ADR-0035 requires before any Backend call: a
/// process failure part-way through must leave no partial Promotion.
///
/// Mints one Promotion Operation identity for the invocation with
/// [`generate_internal_id`] — the same opaque-ID scheme `items.id` uses,
/// reused rather than inventing a second one — and stamps every appended
/// Mutation with it (ADR-0036), so a caller can later ask whether every
/// Mutation belonging to it resolved (CONTEXT.md Promotion Operation).
/// Drafts are appended in plan order, which is what makes the resulting
/// ascending Mutation Sequence order match the plan's contract order: `append`
/// allocates sequences as it goes.
///
/// An empty plan is a legitimate no-op — re-invoking `tk promote` on work
/// already Backend or already Pending Promotion appends nothing (see
/// [`crate::domain::promotion_plan::PromotionPlan::is_empty`]) — and returns
/// `None` rather than minting an identity that would own zero Mutations.
pub fn commit_plan<R: Rng + ?Sized>(
    conn: &mut Connection,
    plan: &PromotionPlan,
    rng: &mut R,
    now: &str,
) -> Result<Option<String>, CommitPlanError> {
    if plan.is_empty() {
        return Ok(None);
    }

    let operation_id = generate_internal_id(rng);
    let tx = crate::store::write_transaction(conn)?;
    for draft in &plan.mutations {
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type: draft.mutation_type,
                item_id: &draft.item_id,
                item_class: draft.item_class,
                payload: &draft.payload,
                promotion_operation_id: Some(&operation_id),
                now_iso: now,
            },
        )?;
    }
    tx.commit()?;
    Ok(Some(operation_id))
}

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
pub fn apply_receipt(
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

/// One Mutation Log entry as `tk promote`'s post-sync report names it: the
/// Mutation Sequence the user hands to `tk sync log`, the state that says
/// whether it was rejected or simply never reached, and the Display ID of the
/// Item it targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationSummary {
    pub sequence: i64,
    pub state: MutationState,
    pub target_display_id: String,
}

/// The earliest Mutation an Apply would still attempt: the lowest Mutation
/// Sequence in `pending` or `failed` state, across the whole Mutation Log.
///
/// Deliberately not scoped to a Promotion Operation. A Promotion is appended
/// behind whatever the outbox already held, so the Mutation that stops the sync
/// may predate the operation and carry no Promotion Operation at all — an
/// operation-scoped query could never name it. It is also the only place the
/// answer exists when Apply hits an environment failure, which leaves the
/// in-flight row `pending` and writes no outcome.
pub fn earliest_applicable_mutation(
    conn: &Connection,
) -> rusqlite::Result<Option<MutationSummary>> {
    conn.query_row(
        "select m.sequence, m.state, i.display_value \
           from mutations m join items i on i.id = m.item_id \
          where m.state in ('pending','failed') \
          order by m.sequence asc limit 1",
        [],
        |r| {
            Ok(MutationSummary {
                sequence: r.get(0)?,
                state: r.get(1)?,
                target_display_id: r.get(2)?,
            })
        },
    )
    .optional()
}

/// The Mutations of `operation_id` that have not reached `applied`, in Mutation
/// Sequence order.
///
/// An empty result is the success condition for one `tk promote`: overall
/// success requires every Mutation in the requested Promotion Operation to
/// resolve (CONTEXT.md Promotion Operation). Comparing a non-empty result
/// against [`earliest_applicable_mutation`] separates a Promotion queued behind
/// an older Mutation from one of the operation's own Mutations being rejected.
pub fn unresolved_in_operation(
    conn: &Connection,
    operation_id: &str,
) -> rusqlite::Result<Vec<MutationSummary>> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.state, i.display_value \
           from mutations m join items i on i.id = m.item_id \
          where m.promotion_operation_id = ?1 and m.state <> 'applied' \
          order by m.sequence asc",
    )?;
    let rows = stmt.query_map(params![operation_id], |r| {
        Ok(MutationSummary {
            sequence: r.get(0)?,
            state: r.get(1)?,
            target_display_id: r.get(2)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_binding::BackendBinding;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{MutationPayload, Promotion, TitleBody};
    use crate::domain::mutation_type::MutationType;
    use crate::domain::promotion_plan::MutationDraft;
    use crate::domain::status::ItemStatus;
    use crate::store::migrations;
    use crate::store::repository::resolve_item_ref;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, insert_dependency, insert_fixture_item,
        insert_fixture_mutation, mutation_count,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

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
    /// [`apply_receipt`] contracts for: the deferred `items` →
    /// `item_ids` foreign key is only tolerated mid-transaction, and a failure
    /// rolls the whole conversion back.
    fn promote(
        conn: &mut Connection,
        item_id: &str,
        receipt: &PromotionReceipt,
    ) -> rusqlite::Result<()> {
        let tx = crate::store::write_transaction(conn)?;
        apply_receipt(&tx, item_id, "github", receipt, NOW)?;
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
    fn an_accepted_ticket_stays_accepted_through_promotion() {
        // Both Selection States a Ticket may carry into Promotion survive it
        // (CONTEXT.md Promotion); `parked` is covered above, and `accepted` is
        // the one the default path takes.
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                selection_state: Some("accepted"),
                priority: Some("P2"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (selection, priority): (String, String) = conn
            .query_row(
                "select selection_state, priority from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(selection, "accepted");
        assert_eq!(priority, "P2");
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

    // ── Preflight graph read ──────────────────────────────────────────────

    fn seed_ticket(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Ticket",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_epic(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
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

    fn seed_child(conn: &Connection, id: &str, display: &str, epic_id: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Child",
                container_id: Some(epic_id),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn item_ids(graph: &PromotionGraph) -> Vec<&str> {
        graph.items.iter().map(|i| i.id.as_str()).collect()
    }

    fn edges(graph: &PromotionGraph) -> Vec<(&str, &str)> {
        graph
            .dependencies
            .iter()
            .map(|d| (d.blocked_id.as_str(), d.blocking_id.as_str()))
            .collect()
    }

    #[test]
    fn an_unrelated_target_is_the_whole_graph() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "elsewhere", "tk-2", 2);

        let graph = read_graph(&conn, "t1", false).unwrap();

        assert_eq!(graph.target_id, "t1");
        assert!(!graph.children_requested);
        assert_eq!(item_ids(&graph), vec!["t1"]);
        assert!(graph.dependencies.is_empty());

        let target = &graph.items[0];
        assert_eq!(target.display_id, "tk-1");
        assert_eq!(target.item_class, ItemClass::Ticket);
        assert_eq!(target.status, ItemStatus::Open);
        assert_eq!(target.created_seq, 1);
        assert_eq!(target.container_id, None);
    }

    #[test]
    fn requested_children_join_the_snapshot_whatever_their_origin() {
        // Origin is not filtered here: which contained Tickets are Promotion
        // Children is the planner's call, and it needs to see an already
        // backend-backed sibling to judge the resulting graph.
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "local-child", "tk-2", "epic", 2);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "backend-child",
                display: "gh-9",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                container_id: Some("epic"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_ticket(&conn, "outside", "tk-4", 4);

        let graph = read_graph(&conn, "epic", true).unwrap();

        assert!(graph.children_requested);
        assert_eq!(
            item_ids(&graph),
            vec!["epic", "local-child", "backend-child"]
        );
        assert_eq!(graph.items[1].container_id.as_deref(), Some("epic"));
    }

    #[test]
    fn contained_tickets_are_snapshotted_even_without_children() {
        // Promoting an Epic snapshots membership for the Tickets it already
        // contains on the same Backend, so the snapshot carries them whether
        // or not `--children` was passed; `children_requested` only decides
        // which of them the planner treats as Promotion Children.
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "epic", 2);

        let graph = read_graph(&conn, "epic", false).unwrap();

        assert_eq!(item_ids(&graph), vec!["epic", "child"]);
        assert!(!graph.children_requested);
    }

    #[test]
    fn a_ticket_target_pulls_in_its_containing_epic() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "epic", 2);
        seed_child(&conn, "sibling", "tk-3", "epic", 3);

        let graph = read_graph(&conn, "child", false).unwrap();

        // The Epic joins because Promotion has to decide about membership; the
        // sibling does not — `--children` descends from an Epic target only.
        assert_eq!(item_ids(&graph), vec!["epic", "child"]);
    }

    #[test]
    fn a_dependency_endpoint_outside_the_operation_joins_as_an_item() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "outside", "tk-2", 2);
        insert_dependency(&conn, "outside", "t1").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        // Preflight rejects a backend-backed Blocked Item left waiting on a
        // Local Blocking Item (ADR-0035), which it can only see if the
        // out-of-scope endpoint is in the snapshot.
        assert_eq!(item_ids(&graph), vec!["t1", "outside"]);
        assert_eq!(edges(&graph), vec![("t1", "outside")]);
    }

    #[test]
    fn a_done_blocking_items_edge_is_retained() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "finished",
                display: "tk-2",
                title: "Finished",
                status: "done",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "finished", "t1").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        // A done Blocking Item resolves readiness but keeps its Dependency
        // (ADR-0035), so the resulting backend graph still carries the edge.
        assert_eq!(edges(&graph), vec![("t1", "finished")]);
        assert_eq!(graph.items[1].status, ItemStatus::Done);
    }

    #[test]
    fn edges_are_captured_in_both_directions() {
        let conn = open_seeded();
        seed_ticket(&conn, "blocker", "tk-1", 1);
        seed_ticket(&conn, "t1", "tk-2", 2);
        seed_ticket(&conn, "blocked", "tk-3", 3);
        insert_dependency(&conn, "blocker", "t1").unwrap();
        insert_dependency(&conn, "t1", "blocked").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        assert_eq!(item_ids(&graph), vec!["blocker", "t1", "blocked"]);
        let mut found = edges(&graph);
        found.sort_unstable();
        assert_eq!(found, vec![("blocked", "t1"), ("t1", "blocker")]);
    }

    #[test]
    fn a_dependency_between_two_children_is_captured_once() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "first", "tk-2", "epic", 2);
        seed_child(&conn, "second", "tk-3", "epic", 3);
        insert_dependency(&conn, "first", "second").unwrap();

        let graph = read_graph(&conn, "epic", true).unwrap();

        assert_eq!(edges(&graph), vec![("second", "first")]);
    }

    #[test]
    fn every_item_carries_its_backend_intent() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "plain-local", "tk-2", "epic", 2);
        seed_child(&conn, "pending", "tk-3", "epic", 3);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                mutation_type: "promote_ticket",
                item_id: "pending",
                payload_json: &MutationPayload::Promotion(Promotion {
                    title: "Child".into(),
                    body: String::new(),
                    backend_kind: "github".into(),
                })
                .to_json_string(),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "adopted",
                display: "gh-9",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                container_id: Some("epic"),
                created_seq: 4,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let graph = read_graph(&conn, "epic", true).unwrap();

        let intents: Vec<&BackendBinding> =
            graph.items.iter().map(|i| &i.backend_binding).collect();
        assert_eq!(
            intents,
            vec![
                &BackendBinding::Local,
                &BackendBinding::Local,
                &BackendBinding::PendingPromotion {
                    backend_kind: "github".into()
                },
                &BackendBinding::Backend {
                    backend_kind: "github".into()
                },
            ]
        );
    }

    // ── Outbox commit ─────────────────────────────────────────────────────

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    /// The `mutation_seq` counter's current value. It lives in `sequences`,
    /// not in `mutations`, so a rollback has to return it separately from the
    /// rows an aborted batch wrote.
    fn mutation_seq(conn: &Connection) -> rusqlite::Result<i64> {
        conn.query_row(
            "select value from sequences where name = 'mutation_seq'",
            [],
            |r| r.get(0),
        )
    }

    /// A `promote_ticket` draft naming `item_id`. The payload shape is not
    /// under test here — every draft reuses the same Promotion payload so
    /// the tests can focus on ordering, stamping, and atomicity.
    fn draft(item_id: &str) -> MutationDraft {
        MutationDraft {
            mutation_type: MutationType::PromoteTicket,
            item_id: item_id.to_owned(),
            item_class: ItemClass::Ticket,
            payload: MutationPayload::Promotion(Promotion {
                title: "T".into(),
                body: String::new(),
                backend_kind: "github".into(),
            }),
        }
    }

    #[test]
    fn commit_promotion_plan_on_an_empty_plan_mints_no_operation() {
        let mut conn = open_seeded();

        let result =
            commit_plan(&mut conn, &PromotionPlan::default(), &mut seeded_rng(), NOW).unwrap();

        assert_eq!(
            result, None,
            "re-invoking promote on already-resolved work must not mint an operation that owns no Mutations"
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn commit_promotion_plan_appends_drafts_in_plan_order() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_ticket(&conn, "t3", "tk-3", 3);
        // Deliberately out of created_seq order: the outbox must follow the
        // plan's order, not the Items' creation order.
        let plan = PromotionPlan {
            mutations: vec![draft("t3"), draft("t1"), draft("t2")],
        };

        commit_plan(&mut conn, &plan, &mut seeded_rng(), NOW).unwrap();

        let mut stmt = conn
            .prepare("select item_id from mutations order by sequence")
            .unwrap();
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec!["t3", "t1", "t2"]);
    }

    #[test]
    fn commit_promotion_plan_stamps_every_mutation_with_the_returned_operation() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        let plan = PromotionPlan {
            mutations: vec![draft("t1"), draft("t2")],
        };

        let operation_id = commit_plan(&mut conn, &plan, &mut seeded_rng(), NOW)
            .unwrap()
            .expect("a non-empty plan mints an operation");

        let mut stmt = conn
            .prepare("select promotion_operation_id from mutations order by sequence")
            .unwrap();
        let stamped: Vec<Option<String>> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            stamped,
            vec![Some(operation_id.clone()), Some(operation_id)]
        );
    }

    #[test]
    fn commit_promotion_plan_rolls_back_the_whole_batch_on_a_mid_batch_failure() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        // "missing" names no `items` row, so its append violates the
        // `mutations` -> `items` composite foreign key after "t1" has
        // already been inserted earlier in the same transaction.
        let plan = PromotionPlan {
            mutations: vec![draft("t1"), draft("missing")],
        };

        let before = mutation_seq(&conn).unwrap();
        let err = commit_plan(&mut conn, &plan, &mut seeded_rng(), NOW).unwrap_err();

        assert!(
            matches!(
                err,
                CommitPlanError::Append(mutations::AppendError::Sqlite(_))
            ),
            "expected a foreign key violation on the second draft, got {err:?}"
        );
        assert_eq!(
            mutation_count(&conn).unwrap(),
            0,
            "the draft that appended before the failure must not survive the rollback"
        );
        // The first draft appended before the second failed, consuming a
        // Mutation Sequence from a different table than the row it wrote. An
        // empty log alone does not prove that allocation came back; without it
        // the next `tk promote` would open at a gap.
        assert_eq!(
            mutation_seq(&conn).unwrap(),
            before,
            "the rollback must return the Mutation Sequence counter too"
        );
    }

    #[test]
    fn commit_promotion_plan_leaves_pre_existing_mutations_undisturbed() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "existing", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);

        // Seed a Mutation the way earlier tk activity would have: through the
        // production outbox writer, consuming a real `mutation_seq` value
        // before the plan commit ever runs.
        let seed_tx = conn.unchecked_transaction().unwrap();
        mutations::append(
            &seed_tx,
            mutations::AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "existing",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "Prior".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: NOW,
            },
        )
        .unwrap();
        seed_tx.commit().unwrap();

        let plan = PromotionPlan {
            mutations: vec![draft("t2")],
        };
        commit_plan(&mut conn, &plan, &mut seeded_rng(), NOW).unwrap();

        let (sequence, promotion_operation_id, mutation_type): (i64, Option<String>, String) = conn
            .query_row(
                "select sequence, promotion_operation_id, mutation_type \
                   from mutations where item_id = 'existing'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            sequence, 1,
            "the pre-existing row keeps its original sequence"
        );
        assert_eq!(promotion_operation_id, None);
        assert_eq!(mutation_type, "update_ticket");

        let new_sequence: i64 = conn
            .query_row(
                "select sequence from mutations where item_id = 't2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            new_sequence, 2,
            "the new append continues the counter rather than restarting it"
        );
    }

    // ── Post-sync operation state ─────────────────────────────────────────

    /// Seed one Mutation Log row directly, so a test can place states and
    /// Promotion Operations the outbox writer alone could not produce.
    fn seed_mutation(
        conn: &Connection,
        sequence: i64,
        item_id: &str,
        state: &str,
        op: Option<&str>,
    ) {
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "update_ticket",
                item_id,
                payload_json: r#"{"title":"T","body":""}"#,
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"boom"}"#),
                promotion_operation_id: op,
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn no_applicable_mutation_when_the_log_is_drained() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", None);
        seed_mutation(&conn, 2, "t1", "skipped", None);

        assert_eq!(earliest_applicable_mutation(&conn).unwrap(), None);
    }

    #[test]
    fn the_earliest_applicable_mutation_may_predate_the_promotion_operation() {
        // The whole reason this read is not operation-scoped: the row that
        // stops the sync carries no Promotion Operation.
        let conn = open_seeded();
        seed_ticket(&conn, "older", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_mutation(&conn, 1, "older", "failed", None);
        seed_mutation(&conn, 2, "t2", "pending", Some("op-1"));

        assert_eq!(
            earliest_applicable_mutation(&conn).unwrap(),
            Some(MutationSummary {
                sequence: 1,
                state: MutationState::Failed,
                target_display_id: "tk-1".to_owned(),
            })
        );
    }

    #[test]
    fn an_applied_row_ahead_of_a_pending_one_is_not_the_blocker() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", Some("op-1"));
        seed_mutation(&conn, 2, "t1", "pending", Some("op-1"));

        assert_eq!(
            earliest_applicable_mutation(&conn)
                .unwrap()
                .unwrap()
                .sequence,
            2
        );
    }

    #[test]
    fn a_fully_applied_operation_has_no_unresolved_mutations() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", Some("op-1"));
        seed_mutation(&conn, 2, "t1", "applied", Some("op-1"));

        assert!(
            unresolved_in_operation(&conn, "op-1").unwrap().is_empty(),
            "an operation whose every Mutation applied is resolved"
        );
    }

    #[test]
    fn unresolved_operation_mutations_ignore_other_operations() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_mutation(&conn, 1, "t1", "pending", None);
        seed_mutation(&conn, 2, "t2", "failed", Some("other"));
        seed_mutation(&conn, 3, "t1", "failed", Some("op-1"));
        seed_mutation(&conn, 4, "t1", "pending", Some("op-1"));

        assert_eq!(
            unresolved_in_operation(&conn, "op-1").unwrap(),
            vec![
                MutationSummary {
                    sequence: 3,
                    state: MutationState::Failed,
                    target_display_id: "tk-1".to_owned(),
                },
                MutationSummary {
                    sequence: 4,
                    state: MutationState::Pending,
                    target_display_id: "tk-1".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn a_skipped_mutation_of_the_operation_is_still_unresolved() {
        // `skipped` is abandoned intent, not a resolved one: the operation did
        // not do everything it promised.
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "skipped", Some("op-1"));

        assert_eq!(unresolved_in_operation(&conn, "op-1").unwrap().len(), 1);
    }
}
