//! Add/remove Dependency edges between Tickets and Epics.
//!
//! Dependencies are current-state relationship data; same-backend Dependency
//! changes between two backend-origin endpoints also append intent through
//! the Mutation Log. The cycle check happens before the INSERT so a typed
//! [`AddDependencyError::Cycle`] reaches the command layer rather than a
//! raw constraint-trigger SQLite error.

use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{DependencyRef, MutationPayload};
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
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
    /// A Backend Blocked Item cannot wait on a still-local Blocking Item:
    /// the Mutation would target an unaddressable reference.
    #[error("backend blocked item cannot depend on a local blocking item")]
    BackendBlockedLocalBlocking,
    /// Two backend-origin endpoints from different backend kinds cannot
    /// share a Dependency Mutation.
    #[error("backend endpoints from different backend kinds")]
    BackendKindMismatch,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Mutation(#[from] mutations::AppendError),
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
    Mutation(#[from] mutations::AppendError),
}

struct EndpointInfo {
    blocked_status: ItemStatus,
    blocked_origin: Origin,
    blocked_class: ItemClass,
    blocked_backend_kind: Option<String>,
    blocking_status: ItemStatus,
    blocking_origin: Origin,
    blocking_backend_kind: Option<String>,
}

/// Insert a Dependency edge from `blocking_id` to `blocked_id`.
pub fn add_dependency<C: Clock + ?Sized>(
    store: &mut Store,
    clock: &C,
    edge: DependencyEdge<'_>,
) -> Result<(), AddDependencyError> {
    let now_iso = clock.now_iso();
    let tx = store.conn.transaction()?;

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
    if info.blocked_origin == Origin::Backend && info.blocking_origin == Origin::Local {
        tx.commit()?;
        return Err(AddDependencyError::BackendBlockedLocalBlocking);
    }
    if info.blocked_origin == Origin::Backend
        && info.blocking_origin == Origin::Backend
        && info.blocked_backend_kind != info.blocking_backend_kind
    {
        tx.commit()?;
        return Err(AddDependencyError::BackendKindMismatch);
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

    let same_backend_pair = info.blocked_origin == Origin::Backend
        && info.blocking_origin == Origin::Backend
        && info.blocked_backend_kind == info.blocking_backend_kind;
    if !had_edge && same_backend_pair {
        mutations::append(
            &tx,
            MutationType::AddDependency,
            edge.blocked_id,
            info.blocked_class,
            &MutationPayload::DependencyRef(DependencyRef {
                blocking_id: edge.blocking_id.to_owned(),
            }),
            &now_iso,
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
    let tx = store.conn.transaction()?;

    let Some(info) = read_endpoint_info(&tx, edge)? else {
        return Err(RemoveDependencyError::EndpointMissing);
    };
    let had_edge = edge_exists(&tx, edge)?;

    tx.execute(
        "delete from dependencies where blocking_id = ?1 and blocked_id = ?2",
        params![edge.blocking_id, edge.blocked_id],
    )?;

    let same_backend_pair = info.blocked_origin == Origin::Backend
        && info.blocking_origin == Origin::Backend
        && info.blocked_backend_kind == info.blocking_backend_kind;
    if had_edge && same_backend_pair {
        mutations::append(
            &tx,
            MutationType::RemoveDependency,
            edge.blocked_id,
            info.blocked_class,
            &MutationPayload::DependencyRef(DependencyRef {
                blocking_id: edge.blocking_id.to_owned(),
            }),
            &now_iso,
        )?;
    }

    tx.commit()?;
    Ok(())
}

fn read_endpoint_info(
    conn: &rusqlite::Connection,
    edge: DependencyEdge<'_>,
) -> Result<Option<EndpointInfo>, rusqlite::Error> {
    conn.query_row(
        "select blocked.status, blocked.origin, blocked.item_class, blocked.backend_kind, \
                blocking.status, blocking.origin, blocking.backend_kind \
           from items blocked \
           join items blocking on blocking.id = ?2 \
          where blocked.id = ?1",
        params![edge.blocked_id, edge.blocking_id],
        |row| {
            Ok(EndpointInfo {
                blocked_status: row.get(0)?,
                blocked_origin: row.get(1)?,
                blocked_class: row.get(2)?,
                blocked_backend_kind: row.get(3)?,
                blocking_status: row.get(4)?,
                blocking_origin: row.get(5)?,
                blocking_backend_kind: row.get(6)?,
            })
        },
    )
    .optional()
}

fn edge_exists(
    conn: &rusqlite::Connection,
    edge: DependencyEdge<'_>,
) -> Result<bool, rusqlite::Error> {
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
    use crate::store::testing::{FixtureItem, insert_dependency, insert_fixture_item};
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
