//! The Containment rules no constraint can hold (ADR-0055): nesting stops at
//! Epic, Ticket, Ticket, and a Ticket Container must not wait on itself.
//! `set` and `clear` change an existing Ticket's Container for `tk update`
//! and Backend Pull; `tk add` calls `check` before its insert.
//!
//! A CHECK constraint cannot span rows, so the write that sets a Ticket's
//! Container calls `check` inside its own transaction. A check made earlier,
//! where the Parent Argument resolves, would leave a gap for a concurrent
//! `tk update` to add a third level.

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::item_class::ItemClass;
use crate::store::repository::{current_display_id, dependency};

/// Why a Ticket cannot be held by the named Container. Each variant is an
/// expected outcome: `tk add` and `tk update` render it at exit 1, and
/// Backend Pull skips the Containment and warns with it (ADR-0057).
#[derive(Debug, Error)]
pub enum ContainmentError {
    /// The Ticket names itself.
    #[error("a Ticket cannot hold itself")]
    HoldsItself,
    /// The Container is a Ticket that another Ticket holds, and a Ticket held
    /// by a Ticket holds nothing. Names both by Display ID.
    #[error("{container} is held by Ticket {holder}, and a Ticket held by a Ticket holds nothing")]
    ContainerHeldByTicket { container: String, holder: String },
    /// The Ticket holds Tickets, so its Container can only be an Epic. Names
    /// both by Display ID.
    #[error("{ticket} holds Tickets, so only an Epic can hold it, not Ticket {container}")]
    TicketHoldsTickets { ticket: String, container: String },
    /// The Ticket already waits on the Ticket Container, which would then
    /// wait on it. Names both, and the Items between them on the Ticket's
    /// wait path in order, by Display ID.
    #[error("{ticket} already waits on {container}{}", dependency::through_clause(.through))]
    WaitsOnContainer {
        ticket: String,
        container: String,
        through: Vec<String>,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Check that `ticket` (`None` for one not yet created) may be held by
/// `container_id` under the depth cap and the cycle rule.
///
/// Reads the graph inside the caller's transaction, before the write. The
/// stored graph cannot show a Ticket naming itself, so `check` tests that
/// first. A pair that would hold each other fails with `ContainerHeldByTicket`.
pub(crate) fn check(
    conn: &Connection,
    ticket_id: Option<&str>,
    container_id: &str,
) -> Result<(), ContainmentError> {
    if ticket_id == Some(container_id) {
        return Err(ContainmentError::HoldsItself);
    }
    let (class, container, holder): (ItemClass, String, Option<String>) = conn.query_row(
        "select c.item_class, c.display_value, \
                (select h.display_value from items h \
                  where h.id = c.container_id and h.item_class = 'ticket') \
           from items c where c.id = ?1",
        params![container_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if class == ItemClass::Epic {
        return Ok(());
    }
    if let Some(holder) = holder {
        return Err(ContainmentError::ContainerHeldByTicket { container, holder });
    }
    if let Some(ticket_id) = ticket_id {
        let holding_ticket: Option<String> = conn
            .query_row(
                "select display_value from items \
                  where id = ?1 and exists(select 1 from items where container_id = ?1)",
                params![ticket_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(ticket) = holding_ticket {
            return Err(ContainmentError::TicketHoldsTickets { ticket, container });
        }
        // A Ticket Container waits on each Ticket it holds, so holding this
        // one closes a cycle when it already waits on the Container.
        if let Some(through) = dependency::wait_path_display_ids(conn, ticket_id, container_id)? {
            return Err(ContainmentError::WaitsOnContainer {
                ticket: current_display_id(conn, ticket_id)?,
                container,
                through,
            });
        }
    }
    Ok(())
}

/// Set `ticket_id`'s Container once [`check`] allows it.
pub(crate) fn set(
    conn: &Connection,
    ticket_id: &str,
    container_id: &str,
) -> Result<(), ContainmentError> {
    check(conn, Some(ticket_id), container_id)?;
    write(conn, ticket_id, Some(container_id))?;
    Ok(())
}

/// Clear `ticket_id`'s Container, which no rule refuses.
pub(crate) fn clear(conn: &Connection, ticket_id: &str) -> rusqlite::Result<()> {
    write(conn, ticket_id, None)
}

/// Write the Container, taking its Item Class from its row.
fn write(conn: &Connection, ticket_id: &str, container_id: Option<&str>) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "update items set container_id = ?2, \
                          container_class = (select item_class from items where id = ?2) \
          where id = ?1",
    )?
    .execute(params![ticket_id, container_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_fixture_item};

    /// An Epic `e`, a top-level Ticket `a`, a Ticket `b` held by `e`, a Ticket
    /// `c` held by `a` (so `a` holds Tickets and `c` is held by a Ticket), and
    /// a free Ticket `d`.
    fn seeded() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        seed(&conn);
        conn
    }

    fn seed(conn: &Connection) {
        let ticket = |id, container: Option<(&'static str, &'static str)>, seq| FixtureItem {
            id,
            display: id,
            title: id,
            container_id: container.map(|c| c.0),
            container_class: container.map_or("epic", |c| c.1),
            created_seq: seq,
            ..FixtureItem::default()
        };
        let epic = FixtureItem {
            id: "e",
            display: "e",
            item_class: "epic",
            ticket_kind: None,
            priority: None,
            title: "e",
            created_seq: 1,
            ..FixtureItem::default()
        };
        insert_fixture_item(conn, epic).unwrap();
        insert_fixture_item(conn, ticket("a", None, 2)).unwrap();
        insert_fixture_item(conn, ticket("b", Some(("e", "epic")), 3)).unwrap();
        insert_fixture_item(conn, ticket("c", Some(("a", "ticket")), 4)).unwrap();
        insert_fixture_item(conn, ticket("d", None, 5)).unwrap();
    }

    #[test]
    fn an_epic_holds_any_ticket() {
        let conn = seeded();
        // `a` holds Tickets, yet an Epic may still hold it.
        check(&conn, Some("a"), "e").unwrap();
        check(&conn, None, "e").unwrap();
    }

    #[test]
    fn a_top_level_ticket_or_one_held_by_an_epic_may_hold_a_ticket() {
        let conn = seeded();
        check(&conn, Some("d"), "a").unwrap();
        check(&conn, None, "b").unwrap();
    }

    #[test]
    fn a_ticket_cannot_hold_itself() {
        let conn = seeded();
        assert!(matches!(
            check(&conn, Some("d"), "d"),
            Err(ContainmentError::HoldsItself)
        ));
    }

    #[test]
    fn a_ticket_held_by_a_ticket_holds_nothing() {
        let conn = seeded();
        let Err(ContainmentError::ContainerHeldByTicket { container, holder }) =
            check(&conn, Some("d"), "c")
        else {
            panic!("expected the depth cap to refuse a held Ticket as Container");
        };
        assert_eq!((container.as_str(), holder.as_str()), ("c", "a"));
        assert!(matches!(
            check(&conn, None, "c"),
            Err(ContainmentError::ContainerHeldByTicket { .. })
        ));
    }

    #[test]
    fn a_ticket_that_holds_tickets_has_no_container_but_an_epic() {
        let conn = seeded();
        let Err(ContainmentError::TicketHoldsTickets { ticket, container }) =
            check(&conn, Some("a"), "d")
        else {
            panic!("expected the depth cap to refuse a holder under a Ticket");
        };
        assert_eq!((ticket.as_str(), container.as_str()), ("a", "d"));
    }
}
