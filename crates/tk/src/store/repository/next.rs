//! `tk next` ready-Ticket selection with Effective Priority (ADR-0015).
//!
//! A ready Ticket is one with `status = open`, no unresolved Dependencies,
//! and no unresolved External Blockers. Each ready candidate's *Effective
//! Priority* is the lowest Priority reachable through unresolved
//! Dependency edges or Epic-membership edges, walked only within the
//! active Workspace Scope. Selection sorts by `(effective_priority,
//! own_priority, created_seq)` so a ticket inherits urgency from work that
//! transitively waits on it.
//!
//! The Repository Store owns readiness, scope interpretation, Effective
//! Priority computation, and creation-order tie breaks. The command
//! renderer owns the compact stdout row and the optional stderr
//! rationale, so the typed [`NextTicket`] carries only the Display ID
//! plus an optional [`Rationale`].

use rusqlite::params;

use crate::domain::item_class::ItemClass;

use super::{Store, resolve_item_ref};

/// One ready Ticket selected by [`next_ready_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextTicket {
    pub display_id: String,
    /// Populated only when Effective Priority < own Priority — i.e. the
    /// urgency was inherited from a Blocked Item rather than the candidate
    /// itself.
    pub rationale: Option<Rationale>,
}

/// Selection rationale. Rendered on stderr (ADR-0015) so `id="$(tk next)"`
/// scripting stays uncluttered on stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rationale {
    pub effective_priority: String,
    pub blocked_display_id: String,
}

/// Workspace Scope input for ready-Ticket selection.
#[derive(Debug, Clone)]
pub enum NextScope<'a> {
    /// No scope; consider every ready Ticket in the store.
    None,
    /// A Display ID or Alias the user supplied; the store resolves it.
    DisplayArg(&'a str),
}

/// Read options for ready-Ticket selection.
#[derive(Debug, Clone)]
pub struct NextOptions<'a> {
    pub scope: NextScope<'a>,
}

impl Default for NextOptions<'_> {
    fn default() -> Self {
        Self {
            scope: NextScope::None,
        }
    }
}

/// Result of [`next_ready_ticket`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextOutcome {
    Ticket(NextTicket),
    NoReadyTicket,
    /// The supplied Workspace Scope Display ID did not resolve.
    ScopeNotFound,
}

/// SQL ordering already guarantees `eff.ep <= ann.priority`, so a byte
/// difference between the two priority text columns means Effective
/// Priority came from a Blocked Item and a rationale is owed. The
/// contributor sub-SELECT picks the lowest-`created_seq` Ticket carrying
/// that Priority to ensure a deterministic rationale row.
const NEXT_READY_TICKET_SQL: &str = "\
with recursive \
  annotated as ( \
      select i.id, i.display_value, i.item_class, i.priority, i.status, \
             i.container_id, i.created_seq, \
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
  prop_edge(src, dst) as ( \
      select d.blocking_id, d.blocked_id \
        from dependencies d \
        join items b on b.id = d.blocked_id \
       where b.status <> 'done' \
         and ( \
             ?1 = 'all' \
             or (?1 = 'ticket' and b.id = ?2) \
             or (?1 = 'epic' and (b.id = ?2 or b.container_id = ?2)) \
         ) \
      union all \
      select i.container_id, i.id \
        from items i \
       where i.container_class = 'epic' \
         and i.item_class = 'ticket' \
         and i.status <> 'done' \
         and ( \
             ?1 = 'all' \
             or (?1 = 'ticket' and i.id = ?2) \
             or (?1 = 'epic' and (i.id = ?2 or i.container_id = ?2)) \
         ) \
  ), \
  reachable(start_id, node_id, path, depth) as ( \
      select id, id, ',' || id || ',', 0 \
        from items \
       where item_class = 'ticket' \
         and status = 'open' \
         and ( \
             ?1 = 'all' \
             or (?1 = 'ticket' and id = ?2) \
             or (?1 = 'epic' and container_id = ?2) \
         ) \
      union all \
      select r.start_id, e.dst, r.path || e.dst || ',', r.depth + 1 \
        from reachable r \
        join prop_edge e on e.src = r.node_id \
       where r.depth < 1024 \
         and instr(r.path, ',' || e.dst || ',') = 0 \
  ), \
  eff(start_id, ep) as ( \
      select r.start_id, min(i.priority) \
        from reachable r \
        join items i on i.id = r.node_id \
       where i.item_class = 'ticket' \
         and i.status <> 'done' \
         and i.priority is not null \
       group by r.start_id \
  ) \
select ann.display_value, ann.priority, eff.ep, \
       ( \
           select contributor.display_value \
             from reachable r2 \
             join items contributor on contributor.id = r2.node_id \
            where r2.start_id = ann.id \
              and contributor.item_class = 'ticket' \
              and contributor.status <> 'done' \
              and contributor.priority = eff.ep \
              and r2.node_id <> ann.id \
            order by contributor.created_seq asc \
            limit 1 \
       ) as contributor_display \
  from annotated ann \
  join eff on eff.start_id = ann.id \
 where ann.item_class = 'ticket' \
   and ann.status = 'open' \
   and not ann.has_unresolved_dependency \
   and not ann.has_unresolved_external_blocker \
   and ( \
       ?1 = 'all' \
       or (?1 = 'ticket' and ann.id = ?2) \
       or (?1 = 'epic' and ann.container_id = ?2) \
   ) \
 order by eff.ep asc, ann.priority asc, ann.created_seq asc \
 limit 1";

/// Select the next ready Ticket from current Repository Store state.
pub fn next_ready_ticket(
    store: &Store,
    options: NextOptions<'_>,
) -> Result<NextOutcome, rusqlite::Error> {
    let (scope_mode, scope_id_owned) = match options.scope {
        NextScope::None => ("all", String::new()),
        NextScope::DisplayArg(arg) => {
            let Some(resolved) = resolve_item_ref(&store.conn, arg)? else {
                return Ok(NextOutcome::ScopeNotFound);
            };
            let mode = match resolved.item_class {
                ItemClass::Ticket => "ticket",
                ItemClass::Epic => "epic",
            };
            (mode, resolved.id)
        }
    };

    let row = store.conn.query_row(
        NEXT_READY_TICKET_SQL,
        params![scope_mode, scope_id_owned.as_str()],
        |row| {
            let display_id: String = row.get(0)?;
            let own_priority: String = row.get(1)?;
            let effective_priority: String = row.get(2)?;
            let contributor: Option<String> = row.get(3)?;
            Ok((display_id, own_priority, effective_priority, contributor))
        },
    );
    match row {
        Ok((display_id, own_priority, effective_priority, contributor)) => {
            let rationale = build_rationale(&own_priority, effective_priority, contributor);
            Ok(NextOutcome::Ticket(NextTicket {
                display_id,
                rationale,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(NextOutcome::NoReadyTicket),
        Err(err) => Err(err),
    }
}

fn build_rationale(
    own_priority: &str,
    effective_priority: String,
    contributor: Option<String>,
) -> Option<Rationale> {
    if own_priority == effective_priority {
        return None;
    }
    // SQL ordering proved EP < own Priority, but the contributor SELECT
    // yields SQL NULL when the lowest EP comes from the candidate itself
    // — in which case there's no foreign rationale to render.
    let blocked_display_id = contributor?;
    Some(Rationale {
        effective_priority,
        blocked_display_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_dependency, insert_fixture_item};
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store { conn }
    }

    fn seed(store: &Store, id: &str, display: &str, priority: &str, created_seq: i64) {
        insert_fixture_item(
            &store.conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: Some(priority),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn returns_no_ready_ticket_for_empty_store() {
        let store = open_seeded();
        assert_eq!(
            next_ready_ticket(&store, NextOptions::default()).unwrap(),
            NextOutcome::NoReadyTicket
        );
    }

    #[test]
    fn picks_highest_priority_first() {
        let store = open_seeded();
        seed(&store, "low", "tk-1", "P3", 1);
        seed(&store, "high", "tk-2", "P0", 2);
        match next_ready_ticket(&store, NextOptions::default()).unwrap() {
            NextOutcome::Ticket(t) => assert_eq!(t.display_id, "tk-2"),
            other => panic!("expected Ticket, got {other:?}"),
        }
    }

    #[test]
    fn breaks_priority_ties_by_creation_order() {
        let store = open_seeded();
        seed(&store, "second", "tk-1", "P1", 2);
        seed(&store, "first", "tk-2", "P1", 1);
        match next_ready_ticket(&store, NextOptions::default()).unwrap() {
            NextOutcome::Ticket(t) => assert_eq!(t.display_id, "tk-2"),
            other => panic!("expected Ticket, got {other:?}"),
        }
    }

    #[test]
    fn skips_tickets_with_unresolved_dependencies() {
        // `blocked` (tk-1) is the dependency target, so it must not appear
        // in the ready set. The blocker (tk-3) inherits priority P4 from
        // its blocked target — same as its own — so it does not outrank
        // `ready` (tk-2, P2) via Effective Priority. tk-2 should win.
        let store = open_seeded();
        seed(&store, "blocked", "tk-1", "P4", 1);
        seed(&store, "ready", "tk-2", "P2", 2);
        seed(&store, "blocker", "tk-3", "P4", 3);
        insert_dependency(&store.conn, "blocker", "blocked").unwrap();

        match next_ready_ticket(&store, NextOptions::default()).unwrap() {
            NextOutcome::Ticket(t) => assert_eq!(t.display_id, "tk-2"),
            other => panic!("expected Ticket, got {other:?}"),
        }
    }

    #[test]
    fn effective_priority_promotes_a_ticket_when_a_blocked_one_outranks_it() {
        let store = open_seeded();
        // blocked has high priority, can't be picked, but its blocker
        // inherits the higher priority via Effective Priority.
        seed(&store, "blocker", "tk-1", "P3", 1);
        seed(&store, "blocked-high", "tk-2", "P0", 2);
        insert_dependency(&store.conn, "blocker", "blocked-high").unwrap();
        seed(&store, "ready", "tk-3", "P1", 3);

        let outcome = next_ready_ticket(&store, NextOptions::default()).unwrap();
        let ticket = match outcome {
            NextOutcome::Ticket(t) => t,
            other => panic!("expected Ticket, got {other:?}"),
        };
        // Effective Priority of `blocker` is P0 (inherited via `blocked-high`),
        // so it sorts ahead of `ready` (P1, own = effective).
        assert_eq!(ticket.display_id, "tk-1");
        let rationale = ticket.rationale.expect("rationale required");
        assert_eq!(rationale.effective_priority, "P0");
        assert_eq!(rationale.blocked_display_id, "tk-2");
    }

    #[test]
    fn scope_unknown_display_arg_returns_scope_not_found() {
        let store = open_seeded();
        seed(&store, "t1", "tk-1", "P1", 1);
        let outcome = next_ready_ticket(
            &store,
            NextOptions {
                scope: NextScope::DisplayArg("missing"),
            },
        )
        .unwrap();
        assert_eq!(outcome, NextOutcome::ScopeNotFound);
    }

    #[test]
    fn scope_to_a_ticket_only_considers_that_ticket() {
        let store = open_seeded();
        seed(&store, "scope-target", "tk-1", "P3", 1);
        seed(&store, "other", "tk-2", "P0", 2);

        let outcome = next_ready_ticket(
            &store,
            NextOptions {
                scope: NextScope::DisplayArg("tk-1"),
            },
        )
        .unwrap();
        match outcome {
            NextOutcome::Ticket(t) => assert_eq!(t.display_id, "tk-1"),
            other => panic!("expected Ticket, got {other:?}"),
        }
    }

    #[test]
    fn ticket_scope_returns_no_ready_when_the_scoped_ticket_is_blocked() {
        let store = open_seeded();
        seed(&store, "scope-target", "tk-1", "P1", 1);
        seed(&store, "blocker", "tk-2", "P4", 2);
        insert_dependency(&store.conn, "blocker", "scope-target").unwrap();
        let outcome = next_ready_ticket(
            &store,
            NextOptions {
                scope: NextScope::DisplayArg("tk-1"),
            },
        )
        .unwrap();
        assert_eq!(outcome, NextOutcome::NoReadyTicket);
    }

    #[test]
    fn rationale_is_absent_when_own_priority_equals_effective_priority() {
        let store = open_seeded();
        seed(&store, "ready", "tk-1", "P1", 1);
        let outcome = next_ready_ticket(&store, NextOptions::default()).unwrap();
        let ticket = match outcome {
            NextOutcome::Ticket(t) => t,
            other => panic!("expected Ticket, got {other:?}"),
        };
        assert_eq!(ticket.display_id, "tk-1");
        assert!(ticket.rationale.is_none());
    }

    #[test]
    fn effective_priority_propagates_through_a_transitive_blocked_by_chain() {
        // ready P3 -> mid P3 -> tail P0. Through transitive deps, the ready
        // candidate inherits min priority P0.
        let store = open_seeded();
        seed(&store, "ready", "tk-1", "P3", 1);
        seed(&store, "mid", "tk-2", "P3", 2);
        seed(&store, "tail", "tk-3", "P0", 3);
        insert_dependency(&store.conn, "ready", "mid").unwrap();
        insert_dependency(&store.conn, "mid", "tail").unwrap();

        let outcome = next_ready_ticket(&store, NextOptions::default()).unwrap();
        let ticket = match outcome {
            NextOutcome::Ticket(t) => t,
            other => panic!("expected Ticket, got {other:?}"),
        };
        assert_eq!(ticket.display_id, "tk-1");
        let rationale = ticket.rationale.expect("multi-hop rationale required");
        assert_eq!(rationale.effective_priority, "P0");
        assert_eq!(rationale.blocked_display_id, "tk-3");
    }
}
