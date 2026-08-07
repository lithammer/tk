//! Add/remove Dependency edges between Tickets and Epics.
//!
//! Dependencies are current-state relationship data; an edge whose endpoints
//! are backend-bound to the same Backend also appends intent through the
//! Mutation Log. Which of the two an edge is — and which edges may not exist
//! at all — is [`crate::domain::dependency_rule`]'s call, the same rules
//! Promotion preflight judges its resulting graph by (ADR-0035), so `tk block`
//! and `tk promote` cannot drift. The cycle check happens before the INSERT so
//! a typed [`AddDependencyError::Cycle`] reaches the command layer rather than
//! a raw constraint-trigger SQLite error.

use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{DependencyRef, MutationPayload};
use crate::domain::mutation_type::MutationType;
use crate::domain::status::ItemStatus;
use crate::store::mutations;

use super::Store;

/// Input for [`add_dependency`] and [`remove_dependency`].
#[derive(Debug, Clone, Copy)]
pub struct DependencyEdge<'a> {
    /// Internal stable ID of the Blocked Item whose readiness changes.
    pub blocked_id: &'a str,
    /// Internal stable ID of the Blocking Item that must finish first.
    pub blocking_id: &'a str,
}

/// Why [`add_dependency`] refused or failed. Success is `Ok(())` — an edge is
/// present after the call (idempotent; an already-existing edge succeeds
/// without emitting a Mutation). The refusal variants render at exit 1; the
/// `#[error]` strings are internal — `tk block` interpolates the user's
/// arguments into its own per-variant lines.
#[derive(Debug, thiserror::Error)]
pub enum AddDependencyError {
    /// Either endpoint was missing in `items`. The schema's foreign keys
    /// would surface this too, but distinguishing it lets the command render
    /// the typed diagnostic before the FK fires.
    #[error("endpoint missing in items table")]
    EndpointMissing,
    /// The Blocked Item is already `Done`; v1 only models live blocking.
    #[error("blocked item is done")]
    BlockedDone,
    /// The Blocking Item is already `Done`; v1 only models live blocking.
    #[error("blocking item is done")]
    BlockingDone,
    /// The edge would close a cycle in the Dependency graph.
    #[error("dependency cycle")]
    Cycle,
    /// A backend-bound Blocked Item cannot wait on a Blocking Item with no
    /// backend intent: the Mutation would target an unaddressable reference.
    #[error("backend blocked item cannot depend on a local blocking item")]
    BackendBlockedLocalBlocking,
    /// Endpoints bound to two different Backends cannot share a Dependency
    /// Mutation.
    #[error("backend endpoints from different backend kinds")]
    BackendKindMismatch,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

impl From<DependencyRejection> for AddDependencyError {
    fn from(rejection: DependencyRejection) -> Self {
        match rejection {
            DependencyRejection::BackendBlockedLocalBlocking => Self::BackendBlockedLocalBlocking,
            DependencyRejection::BackendKindMismatch => Self::BackendKindMismatch,
        }
    }
}

/// Why [`remove_dependency`] failed. Success is `Ok(())` — the edge is absent
/// after the call (idempotent; a missing edge succeeds without a Mutation).
#[derive(Debug, thiserror::Error)]
pub enum RemoveDependencyError {
    /// Either endpoint was missing in `items`.
    #[error("endpoint missing in items table")]
    EndpointMissing,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
}

struct EndpointInfo {
    blocked_status: ItemStatus,
    blocked_class: ItemClass,
    blocking_status: ItemStatus,
}

/// Insert a Dependency edge from `blocking_id` to `blocked_id`.
pub fn add_dependency<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    edge: DependencyEdge<'_>,
) -> Result<(), AddDependencyError> {
    let now_iso = clock.now_iso();
    let tx = crate::store::write_transaction(&mut store.conn)?;

    let Some(info) = read_endpoint_info(&tx, edge)? else {
        return Err(AddDependencyError::EndpointMissing);
    };

    if info.blocked_status == ItemStatus::Done {
        tx.commit()?;
        return Err(AddDependencyError::BlockedDone);
    }
    if info.blocking_status == ItemStatus::Done {
        tx.commit()?;
        return Err(AddDependencyError::BlockingDone);
    }

    let classification = classify_edge(&tx, edge)?;
    if let DependencyClassification::Rejected(rejection) = classification {
        tx.commit()?;
        return Err(rejection.into());
    }

    let cycles_into_existing = tx
        .query_row(
            "with recursive reachable(id) as (\
                select ?2 \
                union \
                select d.blocking_id \
                  from dependencies d, reachable \
                 where d.blocked_id = reachable.id\
              ) \
              select 1 from reachable where id = ?1",
            params![edge.blocked_id, edge.blocking_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if cycles_into_existing.is_some() {
        tx.commit()?;
        return Err(AddDependencyError::Cycle);
    }

    let had_edge = edge_exists(&tx, edge)?;

    tx.execute(
        "insert or ignore into dependencies(blocking_id, blocked_id, created_at) \
         values (?1, ?2, ?3)",
        params![edge.blocking_id, edge.blocked_id, now_iso],
    )?;

    if !had_edge && classification == DependencyClassification::BecomesBackendIntent {
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type: MutationType::AddDependency,
                item_id: edge.blocked_id,
                item_class: info.blocked_class,
                payload: &MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: edge.blocking_id.to_owned(),
                }),
                promotion_operation_id: None,
                now_iso: &now_iso,
            },
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Remove the Dependency edge from `blocking_id` to `blocked_id`. Missing
/// edges are a successful no-op (`tk unblock` is a desired-state cleanup).
pub fn remove_dependency<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    edge: DependencyEdge<'_>,
) -> Result<(), RemoveDependencyError> {
    let now_iso = clock.now_iso();
    let tx = crate::store::write_transaction(&mut store.conn)?;

    let Some(info) = read_endpoint_info(&tx, edge)? else {
        return Err(RemoveDependencyError::EndpointMissing);
    };
    // The same pairing question `add_dependency` asks, so an edge only leaves
    // the Mutation Log the way it entered: a pairing the rule rejects never
    // carried a Mutation, and dropping its edge is current-state cleanup.
    let classification = classify_edge(&tx, edge)?;
    let had_edge = edge_exists(&tx, edge)?;

    tx.execute(
        "delete from dependencies where blocking_id = ?1 and blocked_id = ?2",
        params![edge.blocking_id, edge.blocked_id],
    )?;

    if had_edge && classification == DependencyClassification::BecomesBackendIntent {
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type: MutationType::RemoveDependency,
                item_id: edge.blocked_id,
                item_class: info.blocked_class,
                payload: &MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: edge.blocking_id.to_owned(),
                }),
                promotion_operation_id: None,
                now_iso: &now_iso,
            },
        )?;
    }

    tx.commit()?;
    Ok(())
}

fn read_endpoint_info(
    conn: &Connection,
    edge: DependencyEdge<'_>,
) -> Result<Option<EndpointInfo>, rusqlite::Error> {
    conn.query_row(
        "select blocked.status, blocked.item_class, blocking.status \
           from items blocked \
           join items blocking on blocking.id = ?2 \
          where blocked.id = ?1",
        params![edge.blocked_id, edge.blocking_id],
        |row| {
            Ok(EndpointInfo {
                blocked_status: row.get(0)?,
                blocked_class: row.get(1)?,
                blocking_status: row.get(2)?,
            })
        },
    )
    .optional()
}

/// What this edge means for the Mutation Log, judged from both endpoints'
/// Backend Binding rather than their Origin: a Pending Promotion Item is bound
/// to the Backend its Promotion targets (ADR-0036), so an edge it carries is
/// ordered behind that Promotion instead of being swallowed as a local-only
/// edit.
///
/// Both endpoints are known to exist — [`read_endpoint_info`] answered first —
/// so a missing `items` row here is a Repository Store fault, not a caller
/// mistake.
fn classify_edge(
    conn: &Connection,
    edge: DependencyEdge<'_>,
) -> Result<DependencyClassification, mutations::BackendBindingError> {
    let blocked = mutations::resolve_backend_intent(conn, edge.blocked_id)?;
    let blocking = mutations::resolve_backend_intent(conn, edge.blocking_id)?;
    Ok(dependency_rule::classify(&blocked, &blocking))
}

fn edge_exists(conn: &Connection, edge: DependencyEdge<'_>) -> Result<bool, rusqlite::Error> {
    let present: Option<i64> = conn
        .query_row(
            "select 1 from dependencies where blocking_id = ?1 and blocked_id = ?2",
            params![edge.blocking_id, edge.blocked_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(present.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, commit_promotion, insert_dependency, insert_fixture_item, mutation_types,
    };
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store { conn }
    }

    fn seed_ticket(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
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

    fn seed_backend(
        store: &Store,
        id: &str,
        display: &str,
        backend_kind: &str,
        backend_key: &str,
        created_seq: i64,
    ) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Backend",
                origin: "backend",
                backend_kind: Some(backend_kind),
                backend_key: Some(backend_key),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_done(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: "Done",
                status: "done",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn clock() -> FakeClock {
        FakeClock::new(1_778_284_800_000)
    }

    #[test]
    fn add_dependency_inserts_edge_and_no_mutation_for_local() {
        let mut store = open_seeded();
        seed_ticket(&store, "blocking", "tk-1", 1);
        seed_ticket(&store, "blocked", "tk-2", 2);

        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        let count: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }

    #[test]
    fn add_dependency_same_backend_kind_emits_mutation() {
        let mut store = open_seeded();
        seed_backend(&store, "blocking", "tk-1", "github", "1", 1);
        seed_backend(&store, "blocked", "tk-2", "github", "2", 2);

        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        let (mt, payload): (String, String) = store
            .conn
            .query_row(
                "select mutation_type, payload_json from mutations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mt, "add_dependency");
        assert_eq!(payload, r#"{"blocking_id":"blocking"}"#);
    }

    #[test]
    fn add_dependency_refuses_backend_blocked_local_blocking() {
        let mut store = open_seeded();
        seed_backend(&store, "blocked", "tk-1", "github", "1", 1);
        seed_ticket(&store, "blocking", "tk-2", 2);

        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AddDependencyError::BackendBlockedLocalBlocking
        ));
        let count: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn add_dependency_refuses_backend_kind_mismatch() {
        let mut store = open_seeded();
        seed_backend(&store, "blocked", "tk-1", "github", "1", 1);
        seed_backend(&store, "blocking", "tk-2", "jira", "j-2", 2);

        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap_err();
        assert!(matches!(err, AddDependencyError::BackendKindMismatch));
    }

    #[test]
    fn add_dependency_refuses_done_endpoints() {
        let mut store = open_seeded();
        seed_done(&store, "done-block", "tk-1", 1);
        seed_ticket(&store, "open-block", "tk-2", 2);

        // Blocked is done.
        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "done-block",
                blocking_id: "open-block",
            },
        )
        .unwrap_err();
        assert!(matches!(err, AddDependencyError::BlockedDone));

        // Blocking is done.
        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "open-block",
                blocking_id: "done-block",
            },
        )
        .unwrap_err();
        assert!(matches!(err, AddDependencyError::BlockingDone));
    }

    #[test]
    fn add_dependency_detects_simple_cycle() {
        let mut store = open_seeded();
        seed_ticket(&store, "a", "tk-1", 1);
        seed_ticket(&store, "b", "tk-2", 2);
        insert_dependency(&store.conn, "a", "b").unwrap();

        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "a",
                blocking_id: "b",
            },
        )
        .unwrap_err();
        assert!(matches!(err, AddDependencyError::Cycle));
        let edges: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, 1);
    }

    #[test]
    fn add_dependency_is_idempotent_and_does_not_re_emit_mutation() {
        let mut store = open_seeded();
        seed_backend(&store, "blocking", "tk-1", "github", "1", 1);
        seed_backend(&store, "blocked", "tk-2", "github", "2", 2);

        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();
        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 1);
    }

    #[test]
    fn add_dependency_missing_endpoint_returns_typed_error() {
        let mut store = open_seeded();
        seed_ticket(&store, "lone", "tk-1", 1);
        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "lone",
                blocking_id: "nope",
            },
        )
        .unwrap_err();
        assert!(matches!(err, AddDependencyError::EndpointMissing));
    }

    #[test]
    fn add_dependency_refuses_a_pending_blocked_item_waiting_on_a_local_blocking_item() {
        // A Pending Promotion Blocked Item is backend-bound (ADR-0036), so the
        // edge must be refused here: accepting it would let `tk block` build a
        // graph the next `tk promote` preflight rejects.
        let mut store = open_seeded();
        seed_ticket(&store, "blocked", "tk-1", 1);
        seed_ticket(&store, "blocking", "tk-2", 2);
        commit_promotion(&mut store.conn, "blocked");

        let err = add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap_err();

        assert!(matches!(
            err,
            AddDependencyError::BackendBlockedLocalBlocking
        ));
        let edges: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, 0);
    }

    #[test]
    fn add_dependency_between_two_pending_items_emits_intent_behind_both_promotions() {
        let mut store = open_seeded();
        seed_ticket(&store, "blocked", "tk-1", 1);
        seed_ticket(&store, "blocking", "tk-2", 2);
        commit_promotion(&mut store.conn, "blocked");
        commit_promotion(&mut store.conn, "blocking");

        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "promote_ticket", "add_dependency"]
        );
    }

    #[test]
    fn add_dependency_from_a_pending_item_onto_the_same_backend_emits_a_mutation() {
        let mut store = open_seeded();
        seed_ticket(&store, "blocked", "tk-1", 1);
        seed_backend(&store, "blocking", "gh-2", "github", "2", 2);
        commit_promotion(&mut store.conn, "blocked");

        add_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "add_dependency"]
        );
    }

    #[test]
    fn remove_dependency_drops_edge_and_emits_mutation_for_backend_pair() {
        let mut store = open_seeded();
        seed_backend(&store, "blocking", "tk-1", "github", "1", 1);
        seed_backend(&store, "blocked", "tk-2", "github", "2", 2);
        insert_dependency(&store.conn, "blocking", "blocked").unwrap();

        remove_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        let edges: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, 0);
        let mt: String = store
            .conn
            .query_row("select mutation_type from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mt, "remove_dependency");
    }

    #[test]
    fn remove_dependency_between_two_pending_items_emits_a_mutation() {
        let mut store = open_seeded();
        seed_ticket(&store, "blocked", "tk-1", 1);
        seed_ticket(&store, "blocking", "tk-2", 2);
        commit_promotion(&mut store.conn, "blocked");
        commit_promotion(&mut store.conn, "blocking");
        insert_dependency(&store.conn, "blocking", "blocked").unwrap();

        remove_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        assert_eq!(
            mutation_types(&store.conn).unwrap(),
            vec!["promote_ticket", "promote_ticket", "remove_dependency"]
        );
    }

    #[test]
    fn remove_dependency_emits_nothing_when_only_the_blocking_item_is_pending() {
        // The Blocked Item carries no backend intent, so the edge was never
        // backend intent and its removal is current-state cleanup — the same
        // answer `add_dependency` gives this pairing.
        let mut store = open_seeded();
        seed_ticket(&store, "blocked", "tk-1", 1);
        seed_ticket(&store, "blocking", "tk-2", 2);
        insert_dependency(&store.conn, "blocking", "blocked").unwrap();
        commit_promotion(&mut store.conn, "blocking");

        remove_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();

        assert_eq!(mutation_types(&store.conn).unwrap(), vec!["promote_ticket"]);
        let edges: i64 = store
            .conn
            .query_row("select count(*) from dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edges, 0);
    }

    #[test]
    fn remove_dependency_is_a_noop_on_missing_edges() {
        let mut store = open_seeded();
        seed_ticket(&store, "blocking", "tk-1", 1);
        seed_ticket(&store, "blocked", "tk-2", 2);

        remove_dependency(
            &mut store,
            &clock(),
            DependencyEdge {
                blocked_id: "blocked",
                blocking_id: "blocking",
            },
        )
        .unwrap();
    }
}
