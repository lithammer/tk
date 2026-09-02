//! Atomic Repository Store transition for `tk detach` (ADR-0047).

use rusqlite::params;
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::item_class::ItemClass;
use crate::store::mutations::{self, BackendBindingError};
use crate::store::sequences::SequenceError;

use super::{Store, next_display_id, resolve_item_ref};

/// Successful Backend-to-Local identity transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachReport {
    pub item_class: ItemClass,
    pub backend_display_id: String,
    pub local_display_id: String,
    pub backend_key: String,
}

/// Expected refusal or Repository Store failure from Detach.
#[derive(Debug, Error)]
pub enum DetachError {
    #[error("Display ID or Alias not found")]
    NotFound,
    #[error("the Item is already local")]
    Local,
    #[error("the Item is a Pending Promotion")]
    PendingPromotion,
    #[error("the Item is not an ordinarily adopted Backend Ticket")]
    UnsupportedHistory,
    #[error("the Item has unresolved Mutations")]
    UnresolvedMutations,
    #[error("a Backend-bound Blocked Item would wait on the detached Local Ticket")]
    BackendBlockedByDetached {
        display_id: String,
        item_class: ItemClass,
    },
    #[error("repository store is missing the display_prefix seed")]
    DisplayPrefixMissing,
    #[error(transparent)]
    BackendBinding(#[from] BackendBindingError),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
}

/// Apply Detach's atomic Backend-to-Local Repository Store transition
/// (ADR-0047).
pub fn detach(
    store: &mut Store,
    display_arg: &str,
    now: &str,
) -> Result<DetachReport, DetachError> {
    let tx = crate::store::write_transaction(&mut store.conn)?;
    let Some(reference) = resolve_item_ref(&tx, display_arg)? else {
        return Err(DetachError::NotFound);
    };
    let (backend_kind, backend_key): (Option<BackendKind>, Option<String>) = tx.query_row(
        "select backend_kind, backend_key from items where id = ?1",
        params![&reference.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let (Some(backend_kind), Some(backend_key)) = (backend_kind, backend_key) else {
        match mutations::resolve_backend_binding(&tx, &reference.id)? {
            BackendBinding::Local => return Err(DetachError::Local),
            BackendBinding::PendingPromotion { .. } => {
                return Err(DetachError::PendingPromotion);
            }
            BackendBinding::Backend { .. } => {
                unreachable!("Backend Origin rows carry identity by schema contract")
            }
        }
    };

    let aliases: i64 = tx.query_row(
        "select count(*) from item_ids where item_id = ?1 and source = 'alias'",
        params![&reference.id],
        |row| row.get(0),
    )?;
    if reference.item_class != ItemClass::Ticket || aliases != 0 {
        return Err(DetachError::UnsupportedHistory);
    }
    if let Some((display_id, item_class)) = backend_bound_blocked_item(&tx, &reference.id)? {
        return Err(DetachError::BackendBlockedByDetached {
            display_id,
            item_class,
        });
    }
    let unresolved: i64 = tx.query_row(
        "select count(*) \
           from mutations \
          where state in ('pending','failed','applying') \
            and ( \
                item_id = ?1 \
                or ( \
                    mutation_type in ('add_dependency','remove_dependency') \
                    and json_extract(payload_json, '$.blocking_id') = ?1 \
                ) \
                or ( \
                    mutation_type = 'add_ticket_to_epic' \
                    and json_extract(payload_json, '$.epic_id') = ?1 \
                ) \
            )",
        params![&reference.id],
        |row| row.get(0),
    )?;
    if unresolved != 0 {
        return Err(DetachError::UnresolvedMutations);
    }

    let local_display_id = next_display_id(&tx)?.ok_or(DetachError::DisplayPrefixMissing)?;
    let detached_seq: i64 = tx.query_row(
        "select coalesce(max(detached_seq), 0) + 1 from former_backend_identities",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "insert into former_backend_identities(backend_kind, backend_key, item_id, \
                                                backend_display_value, detached_seq, detached_at) \
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            backend_kind.text(),
            backend_key,
            &reference.id,
            &reference.display_id,
            detached_seq,
            now
        ],
    )?;
    tx.execute(
        "delete from item_ids where item_id = ?1 and source = 'display'",
        params![&reference.id],
    )?;
    tx.execute(
        "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, ?3)",
        params![&local_display_id, &reference.id, now],
    )?;
    tx.execute(
        "update items \
            set display_value = ?2, origin = 'local', backend_kind = null, backend_key = null, \
                updated_at = ?3 \
          where id = ?1",
        params![&reference.id, &local_display_id, now],
    )?;
    tx.commit()?;

    Ok(DetachReport {
        item_class: reference.item_class,
        backend_display_id: reference.display_id,
        local_display_id,
        backend_key,
    })
}

fn backend_bound_blocked_item(
    conn: &rusqlite::Connection,
    blocking_id: &str,
) -> Result<Option<(String, ItemClass)>, DetachError> {
    let mut stmt = conn.prepare(
        "select blocked.id, blocked.display_value, blocked.item_class \
           from dependencies d \
           join items blocked on blocked.id = d.blocked_id \
          where d.blocking_id = ?1 \
          order by blocked.created_seq",
    )?;
    let rows = stmt
        .query_map(params![blocking_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, ItemClass>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, display_id, item_class) in rows {
        if mutations::resolve_backend_binding(conn, &id)?.is_backend_bound() {
            return Ok(Some((display_id, item_class)));
        }
    }
    Ok(None)
}
