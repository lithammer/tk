//! Atomic Repository Store transition for `tk detach` (ADR-0047).

use rusqlite::params;
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::binding_display_provenance::BindingDisplayProvenance;
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_type::MutationType;
use crate::store::mutations::{self, BackendBindingError};
use crate::store::sequences::SequenceError;

use super::{Store, insert_display_resolver, next_display_id, resolve_item_ref};

/// Successful Backend-to-Local identity transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachReport {
    /// Backend Display ID removed from the resolver.
    pub backend_display_id: String,
    /// Local Display ID made current by Detach.
    pub local_display_id: String,
    /// Canonical identity of the Backend object left unchanged.
    pub backend_key: String,
    /// Item Class preserved across the transition.
    pub item_class: ItemClass,
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
    #[error("the Binding has ambiguous local Display ID provenance")]
    AmbiguousDisplayProvenance,
    #[error("the Item has unresolved Mutations")]
    UnresolvedMutations,
    #[error("a Backend-bound Blocked Item would wait on the detached Local Item")]
    BackendBlockedByDetached {
        display_id: String,
        item_class: ItemClass,
        detached_item_class: ItemClass,
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
    let (backend_kind, backend_key, provenance, displaced_display_id): (
        Option<BackendKind>,
        Option<String>,
        BindingDisplayProvenance,
        Option<String>,
    ) = tx.query_row(
        "select backend_kind, backend_key, binding_display_provenance, \
                binding_local_display_value \
           from items where id = ?1",
        params![&reference.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
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

    if let Some((display_id, item_class)) = backend_item_blocked_by_local(&tx, &reference.id)? {
        return Err(DetachError::BackendBlockedByDetached {
            display_id,
            item_class,
            detached_item_class: reference.item_class,
        });
    }
    if has_unresolved_mutations(&tx, &reference.id)? {
        return Err(DetachError::UnresolvedMutations);
    }

    let local_display_id = match provenance {
        BindingDisplayProvenance::None => {
            next_display_id(&tx)?.ok_or(DetachError::DisplayPrefixMissing)?
        }
        BindingDisplayProvenance::Known => displaced_display_id
            .expect("known Binding Display provenance carries a value by schema contract"),
        BindingDisplayProvenance::Ambiguous => {
            return Err(DetachError::AmbiguousDisplayProvenance);
        }
    };
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
    if provenance == BindingDisplayProvenance::Known {
        tx.execute(
            "update item_ids set source = 'display' \
              where item_id = ?1 and value = ?2 and source = 'alias'",
            params![&reference.id, &local_display_id],
        )?;
    } else {
        insert_display_resolver(&tx, &local_display_id, &reference.id, now)?;
    }
    tx.execute(
        "update items \
            set display_value = ?2, origin = 'local', backend_kind = null, backend_key = null, \
                updated_at = ?3, binding_display_provenance = 'none', \
                binding_local_display_value = null \
          where id = ?1",
        params![&reference.id, &local_display_id, now],
    )?;
    tx.commit()?;

    Ok(DetachReport {
        backend_display_id: reference.display_id,
        local_display_id,
        backend_key,
        item_class: reference.item_class,
    })
}

fn backend_item_blocked_by_local(
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
    let rows = stmt.query_map(params![blocking_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, ItemClass>(2)?,
        ))
    })?;
    for row in rows {
        let (id, display_id, item_class) = row?;
        let blocked = mutations::resolve_backend_binding(conn, &id)?;
        if matches!(
            dependency_rule::classify(&blocked, &BackendBinding::Local),
            DependencyClassification::Rejected(DependencyRejection::BackendBlockedLocalBlocking)
        ) {
            return Ok(Some((display_id, item_class)));
        }
    }
    Ok(None)
}

fn has_unresolved_mutations(
    conn: &rusqlite::Connection,
    item_id: &str,
) -> Result<bool, DetachError> {
    let mut stmt = conn.prepare(
        "select mutation_type, item_id, payload_json \
           from mutations \
          where state in ('pending','failed','applying')",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, MutationType>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (mutation_type, mutation_item_id, payload_json) = row?;
        if mutation_item_id == item_id {
            return Ok(true);
        }
        // Detach does not consume Mutation payloads. A payload that does not
        // decode cannot prove that it addresses this Item; Sync diagnoses it
        // when the row reaches the head of the Mutation Log.
        if mutations::addressed_counterpart_id(mutation_type, &payload_json)
            .is_ok_and(|counterpart| counterpart.as_deref() == Some(item_id))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
