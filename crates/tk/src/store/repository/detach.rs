//! Atomic Repository Store transition for `tk detach` (ADR-0047).

use rusqlite::params;
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::binding_display_provenance::{
    BindingDisplayProvenance, InvalidBindingDisplayProvenance,
};
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::store::mutations::{self, BackendBindingError};
use crate::store::promotion;
use crate::store::sequences::SequenceError;

use super::{
    Store, current_display_id, insert_display_resolver, next_display_id, resolve_item_ref,
};

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
    /// Every Mutation Detach withdrew, in Mutation Sequence order.
    pub withdrawn: Vec<WithdrawnMutation>,
}

/// One pending or failed Mutation withdrawn because it needed the removed
/// Backend identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawnMutation {
    pub sequence: i64,
    pub mutation_type: MutationType,
    /// Display ID of the Mutation's target Item as Detach leaves it, so a
    /// withdrawal on the detached Item names its restored local identity.
    pub target_display_id: String,
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
    #[error(transparent)]
    InvalidDisplayProvenance(#[from] InvalidBindingDisplayProvenance),
    #[error("mutation {sequence} belongs to a Promotion Operation with an unresolved Promotion")]
    UnresolvedPromotionOperation {
        sequence: i64,
        mutation_type: MutationType,
        promotion: promotion::UnresolvedPromotion,
    },
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
    Transition(#[from] mutations::IllegalTransition),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
}

impl From<mutations::TransitionError> for DetachError {
    fn from(error: mutations::TransitionError) -> Self {
        match error {
            mutations::TransitionError::Storage(error) => Self::Storage(error),
            mutations::TransitionError::Illegal(error) => Self::Transition(error),
        }
    }
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
    let (backend_kind, backend_key, stored_provenance, displaced_display_id): (
        Option<BackendKind>,
        Option<String>,
        String,
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
    let provenance =
        BindingDisplayProvenance::from_stored(&stored_provenance, displaced_display_id)?;
    let displaced_display_id = match provenance {
        BindingDisplayProvenance::None => None,
        BindingDisplayProvenance::Known(display_id) => Some(display_id),
        BindingDisplayProvenance::Ambiguous => {
            return Err(DetachError::AmbiguousDisplayProvenance);
        }
    };

    if let Some((display_id, item_class)) = backend_item_blocked_by_local(&tx, &reference.id)? {
        return Err(DetachError::BackendBlockedByDetached {
            display_id,
            item_class,
            detached_item_class: reference.item_class,
        });
    }
    let affected = affected_mutations(&tx, &reference.id)?;
    for mutation in &affected {
        if let Some(operation_id) = mutation.promotion_operation_id.as_deref()
            && let Some(promotion) = promotion::unresolved_promotion(&tx, operation_id)?
        {
            return Err(DetachError::UnresolvedPromotionOperation {
                sequence: mutation.sequence,
                mutation_type: mutation.mutation_type,
                promotion,
            });
        }
    }

    let restores_displaced_id = displaced_display_id.is_some();
    let local_display_id = match displaced_display_id {
        Some(display_id) => display_id,
        None => next_display_id(&tx)?.ok_or(DetachError::DisplayPrefixMissing)?,
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
    if restores_displaced_id {
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
    // Withdrawn after the identity change, so each report line names the Item
    // Display ID Detach leaves behind rather than the one it removed.
    let mut withdrawn = Vec::with_capacity(affected.len());
    for mutation in affected {
        mutations::transition(
            &tx,
            mutations::TransitionRequest {
                sequence: mutation.sequence,
                from: mutation.state,
                to: MutationState::Cancelled,
                failure: None,
                now,
            },
        )?;
        withdrawn.push(WithdrawnMutation {
            sequence: mutation.sequence,
            mutation_type: mutation.mutation_type,
            target_display_id: current_display_id(&tx, &mutation.item_id)?,
        });
    }
    tx.commit()?;

    Ok(DetachReport {
        backend_display_id: reference.display_id,
        local_display_id,
        backend_key,
        item_class: reference.item_class,
        withdrawn,
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

/// One Mutation Detach withdraws, with the state its transition starts from.
struct AffectedMutation {
    sequence: i64,
    state: MutationState,
    mutation_type: MutationType,
    item_id: String,
    promotion_operation_id: Option<String>,
}

/// Every pending or failed Mutation that needs `item_id`'s Backend address to
/// be delivered, in Mutation Sequence order.
///
/// Two roles qualify: the Mutation's own target, and the counterpart its
/// Mutation Type addresses (ADR-0038). A Promotion is excluded in both roles —
/// it creates a Backend identity rather than needing one, and only Promotion
/// Cancellation withdraws it. That also keeps `applying` out of the set: the
/// `mutations` CHECK admits that state for a Promotion alone, so an unrelated
/// indeterminate creation leaves this Store-only operation alone (ADR-0047).
fn affected_mutations(
    conn: &rusqlite::Connection,
    item_id: &str,
) -> Result<Vec<AffectedMutation>, DetachError> {
    let mut stmt = conn.prepare(
        "select sequence, state, mutation_type, item_id, payload_json, promotion_operation_id \
           from mutations \
          where state in ('pending','failed') \
            and mutation_type not in ('promote_ticket', 'promote_epic') \
          order by sequence asc",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            AffectedMutation {
                sequence: row.get(0)?,
                state: row.get(1)?,
                mutation_type: row.get(2)?,
                item_id: row.get(3)?,
                promotion_operation_id: row.get(5)?,
            },
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut affected = Vec::new();
    for row in rows {
        let (mutation, payload_json) = row?;
        // A payload that does not decode cannot prove that it addresses this
        // Item, and Sync cannot deliver it either: it decodes the same payload
        // to find the counterpart's Backend address. Promotion Cancellation
        // treats the same row as corruption and refuses, because it is
        // withdrawing intent the operator asked about; Detach is local
        // recovery and stays available, leaving the row for Sync to diagnose.
        let addresses_item = mutation.item_id == item_id
            || mutations::addressed_counterpart_id(mutation.mutation_type, &payload_json)
                .is_ok_and(|counterpart| counterpart.as_deref() == Some(item_id));
        if addresses_item {
            affected.push(mutation);
        }
    }
    Ok(affected)
}
