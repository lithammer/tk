//! Local Ticket and Epic creation (`tk add`).
//!
//! Allocates the next monotonic sequence values inside a `begin immediate`
//! transaction, derives a Display ID from the seeded prefix, and inserts the
//! item row plus its `display`-source `item_ids` row in lockstep so the
//! deferred composite foreign key on
//! `(display_value, id, display_source) -> item_ids` holds at COMMIT.

use rand::Rng;
use rusqlite::{OptionalExtension, params};

use crate::clock::Clock;
use crate::domain::item_class::ItemClass;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::store::sequences;

use super::Store;

/// Input shape for creating a local Ticket.
#[derive(Debug, Clone)]
pub struct CreateLocalTicketInput<'a> {
    pub kind: TicketKind,
    pub priority: Priority,
    /// Internal stable `items.id` of the parent Epic, if any.
    pub parent_id: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
}

/// Input shape for creating a local Epic.
#[derive(Debug, Clone)]
pub struct CreateLocalEpicInput<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

/// Created-Ticket snapshot returned to the caller for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedTicket {
    pub id: String,
    pub display_id: String,
    pub kind: TicketKind,
    pub priority: Priority,
    pub status: ItemStatus,
    pub origin: Origin,
    pub title: String,
    pub body: String,
}

/// Created-Epic snapshot returned to the caller for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedEpic {
    pub id: String,
    pub display_id: String,
    pub status: ItemStatus,
    pub origin: Origin,
    pub title: String,
    pub body: String,
}

/// Errors returned by [`create_local_ticket`] and [`create_local_epic`].
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Sequence(#[from] sequences::SequenceError),
    /// `store_config.display_prefix` was not seeded; the store has not been
    /// initialised. Reachable only on a corrupted or partially-restored
    /// database — `tk init` always seeds the row.
    #[error("repository store is missing the display_prefix seed")]
    DisplayPrefixMissing,
}

/// Create a local Ticket and its current Display ID resolver row.
///
/// `rng` supplies the 128-bit internal stable ID; production code threads
/// `Deps::rng`, and tests inject a seeded `StdRng` for reproducible IDs.
pub fn create_local_ticket<C, R>(
    store: &mut Store,
    clock: &C,
    rng: &mut R,
    input: CreateLocalTicketInput<'_>,
) -> Result<CreatedTicket, CreateError>
where
    C: Clock + ?Sized,
    R: Rng + ?Sized,
{
    let now_iso = clock.now_iso();
    let tx = store.conn.transaction()?;

    let id = generate_internal_id(rng);
    let display_id = next_display_id(&tx)?;
    let created_seq = sequences::next(&tx, "item_created_seq")?;

    let container_class: Option<&str> = input.parent_id.map(|_| ItemClass::Epic.text());
    tx.execute(
        "insert into items(\
            id, display_value, item_class, ticket_kind, priority, title, body, \
            container_id, container_class, origin, status, created_seq, \
            created_at, updated_at\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            id,
            display_id,
            ItemClass::Ticket.text(),
            input.kind.text(),
            input.priority.text(),
            input.title,
            input.body,
            input.parent_id,
            container_class,
            Origin::Local.text(),
            ItemStatus::default().text(),
            created_seq,
            now_iso,
        ],
    )?;
    insert_display_resolver(&tx, &display_id, &id, &now_iso)?;
    tx.commit()?;

    Ok(CreatedTicket {
        id,
        display_id,
        kind: input.kind,
        priority: input.priority,
        status: ItemStatus::default(),
        origin: Origin::Local,
        title: input.title.to_owned(),
        body: input.body.to_owned(),
    })
}

/// Create a local Epic and its current Display ID resolver row.
pub fn create_local_epic<C, R>(
    store: &mut Store,
    clock: &C,
    rng: &mut R,
    input: CreateLocalEpicInput<'_>,
) -> Result<CreatedEpic, CreateError>
where
    C: Clock + ?Sized,
    R: Rng + ?Sized,
{
    let now_iso = clock.now_iso();
    let tx = store.conn.transaction()?;

    let id = generate_internal_id(rng);
    let display_id = next_display_id(&tx)?;
    let created_seq = sequences::next(&tx, "item_created_seq")?;

    tx.execute(
        "insert into items(\
            id, display_value, item_class, title, body, origin, status, \
            created_seq, created_at, updated_at\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            id,
            display_id,
            ItemClass::Epic.text(),
            input.title,
            input.body,
            Origin::Local.text(),
            ItemStatus::default().text(),
            created_seq,
            now_iso,
        ],
    )?;
    insert_display_resolver(&tx, &display_id, &id, &now_iso)?;
    tx.commit()?;

    Ok(CreatedEpic {
        id,
        display_id,
        status: ItemStatus::default(),
        origin: Origin::Local,
        title: input.title.to_owned(),
        body: input.body.to_owned(),
    })
}

/// Draw 16 random bytes from `rng` and render them as lowercase hex.
///
/// The internal stable ID survives Display ID changes — Promotion swaps the
/// Display ID but keeps `items.id` and any backend references intact.
pub fn generate_internal_id<R: Rng + ?Sized>(rng: &mut R) -> String {
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn insert_display_resolver(
    conn: &rusqlite::Connection,
    display_id: &str,
    item_id: &str,
    now_iso: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "insert into item_ids(value, source, item_id, created_at) \
         values (?1, 'display', ?2, ?3)",
        params![display_id, item_id, now_iso],
    )?;
    Ok(())
}

fn next_display_id(conn: &rusqlite::Connection) -> Result<String, CreateError> {
    let display_seq = sequences::next(conn, "display_seq")?;
    let prefix: Option<String> = conn
        .query_row(
            "select value from store_config where key = 'display_prefix'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let prefix = prefix.ok_or(CreateError::DisplayPrefixMissing)?;
    Ok(format!("{prefix}-{display_seq}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::store::migrations;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into store_config(key, value) values ('display_prefix', 'tk')",
            [],
        )
        .unwrap();
        Store { conn }
    }

    fn fixed_rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    #[test]
    fn create_local_ticket_inserts_row_and_resolver() {
        let mut store = open_seeded();
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = fixed_rng();

        let created = create_local_ticket(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalTicketInput {
                kind: TicketKind::Task,
                priority: Priority::P1,
                parent_id: None,
                title: "Ship it",
                body: "Body text",
            },
        )
        .unwrap();

        assert_eq!(created.display_id, "tk-1");
        assert_eq!(created.kind, TicketKind::Task);
        assert_eq!(created.priority, Priority::P1);
        assert_eq!(created.status, ItemStatus::Open);
        assert_eq!(created.origin, Origin::Local);
        assert_eq!(created.title, "Ship it");
        assert_eq!(created.body, "Body text");
        assert_eq!(created.id.len(), 32);

        let (id, display, ticket_kind, priority, title, body, origin, status, created_seq): (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
        ) = store
            .conn
            .query_row(
                "select id, display_value, ticket_kind, priority, title, body, origin, status, created_seq \
                 from items",
                [],
                |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                        r.get(6)?, r.get(7)?, r.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(id, created.id);
        assert_eq!(display, "tk-1");
        assert_eq!(ticket_kind, "task");
        assert_eq!(priority, "P1");
        assert_eq!(title, "Ship it");
        assert_eq!(body, "Body text");
        assert_eq!(origin, "local");
        assert_eq!(status, "open");
        assert_eq!(created_seq, 1);

        let alias_count: i64 = store
            .conn
            .query_row(
                "select count(*) from item_ids where source = 'display' and item_id = ?1",
                params![&created.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alias_count, 1);
    }

    #[test]
    fn create_local_ticket_with_parent_writes_container_columns() {
        let mut store = open_seeded();
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = fixed_rng();

        let epic = create_local_epic(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalEpicInput {
                title: "Epic",
                body: "",
            },
        )
        .unwrap();

        let ticket = create_local_ticket(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalTicketInput {
                kind: TicketKind::Task,
                priority: Priority::P2,
                parent_id: Some(&epic.id),
                title: "Child",
                body: "",
            },
        )
        .unwrap();

        assert_eq!(ticket.display_id, "tk-2");
        let (container_id, container_class): (Option<String>, Option<String>) = store
            .conn
            .query_row(
                "select container_id, container_class from items where id = ?1",
                params![&ticket.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(container_id.as_deref(), Some(epic.id.as_str()));
        assert_eq!(container_class.as_deref(), Some("epic"));
    }

    #[test]
    fn display_ids_increment_monotonically() {
        let mut store = open_seeded();
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = fixed_rng();

        let a = create_local_ticket(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalTicketInput {
                kind: TicketKind::Task,
                priority: Priority::P2,
                parent_id: None,
                title: "A",
                body: "",
            },
        )
        .unwrap();
        let b = create_local_ticket(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalTicketInput {
                kind: TicketKind::Task,
                priority: Priority::P2,
                parent_id: None,
                title: "B",
                body: "",
            },
        )
        .unwrap();
        assert_eq!(a.display_id, "tk-1");
        assert_eq!(b.display_id, "tk-2");
    }

    #[test]
    fn create_local_epic_inserts_epic_row_with_no_priority_or_kind() {
        let mut store = open_seeded();
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = fixed_rng();

        let epic = create_local_epic(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalEpicInput {
                title: "Big work",
                body: "Multi-paragraph\nbody",
            },
        )
        .unwrap();

        assert_eq!(epic.display_id, "tk-1");
        let (class, ticket_kind, priority): (String, Option<String>, Option<String>) = store
            .conn
            .query_row(
                "select item_class, ticket_kind, priority from items where id = ?1",
                params![&epic.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(class, "epic");
        assert!(ticket_kind.is_none());
        assert!(priority.is_none());
    }

    #[test]
    fn local_creates_emit_no_mutations() {
        let mut store = open_seeded();
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = fixed_rng();

        create_local_ticket(
            &mut store,
            &clock,
            &mut rng,
            CreateLocalTicketInput {
                kind: TicketKind::Task,
                priority: Priority::P2,
                parent_id: None,
                title: "Local",
                body: "",
            },
        )
        .unwrap();
        let mutations: i64 = store
            .conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }
}
