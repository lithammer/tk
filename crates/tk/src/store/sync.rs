//! Repository Store operations for Remote configuration, retained Backend
//! cohort validation, canonical Adopt insertion, Pull refresh, and Mutation
//! Log replay and inspection.
//!
//! Every operation here is SQL on the `items` / `mutations` / `item_ids` /
//! `remotes` / `sync_cursors` tables, so it lives under [`crate::store`]. The
//! Adopt, Promote, Remote, and Sync flows compose these transactions with
//! the backend-blind [`crate::remote::adapter::Adapter`] boundary.
//!
//! Write helpers open their own transaction and take `&mut Connection`; read
//! helpers take `&Connection`.

use rusqlite::{Connection, OptionalExtension, params};
use std::str::FromStr;
use thiserror::Error;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemAddress, BackendItemRefresh,
    BackendOperation,
};
use crate::domain::backend_outcome::{
    BackendCreateOutcome, BackendEditOutcome, Failure, FailureClass,
};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, MutationPayload, Promotion, StatusChange, TitleBody,
};
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::store::mutations;
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::repository::create::generate_internal_id;
use crate::store::sequences::{self, SequenceError};

// ──────────────────────────────────────────────────────────────────────────
// Directional Backend reads
// ──────────────────────────────────────────────────────────────────────────

/// Error returned while validating the retained Backend-kind cohort.
#[derive(Debug, Error)]
pub enum BackendCohortError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error("Repository Store contains backend-bound state for multiple Backend kinds")]
    MultipleBackendKinds,
    #[error("Repository Store contains unknown Backend kind '{0}'")]
    UnknownBackendKind(String),
    #[error("Repository Store retains {retained} state, but the operation targets {expected}")]
    BackendKindMismatch {
        expected: BackendKind,
        retained: BackendKind,
    },
}

/// Error returned by canonical Adopt insertion.
#[derive(Debug, Error)]
pub enum AdoptStoreError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    /// An adopted Ticket's `display_id` collided with an existing `item_ids.value`
    /// (a Display ID or Alias already claimed by another Item). Carries the
    /// colliding Display ID for the command's verbatim diagnostic.
    #[error("Display ID '{0}' already claimed by an existing Item")]
    DisplayIdCollision(String),
    /// The Remote changed after an Adapter read and before its Store write.
    #[error("Remote changed while contacting the Backend; retry the command")]
    RemoteChanged {
        expected: BackendKind,
        actual: Option<BackendKind>,
    },
    #[error(transparent)]
    BackendCohort(#[from] BackendCohortError),
    #[error("Mutation {0} has an indeterminate Backend creation outcome")]
    ApplyingMutation(i64),
    #[error("{0} is a Backend Epic, not a Ticket")]
    BackendItemIsEpic(String),
}

/// Error returned by Backend Pull derivation and refresh merge.
#[derive(Debug, Error)]
pub enum RefreshStoreError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// The Remote changed after refresh and before the Store write.
    #[error("Remote changed while contacting the Backend; retry the command")]
    RemoteChanged {
        expected: BackendKind,
        actual: Option<BackendKind>,
    },
    #[error(transparent)]
    BackendCohort(#[from] BackendCohortError),
    #[error("Mutation {0} has an indeterminate Backend creation outcome")]
    ApplyingMutation(i64),
}

/// Result of canonical Backend Ticket insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    Inserted(BackendItemRow),
    AlreadyExists(BackendItemRow),
}

/// Insert canonical Adapter intake data as one Backend Ticket.
///
/// The Repository Store is the serialization point for canonical identity:
/// duplicate `(BackendKind, backend_key)` input returns the stored row without
/// changing it. The transaction rechecks the Remote before writing, closing
/// the Adapter-read to Store-write configuration race.
pub fn adopt_backend_ticket(
    conn: &mut Connection,
    expected_kind: BackendKind,
    rng: &mut dyn rand::Rng,
    adopted: &AdoptedItem,
    now: &str,
) -> Result<AdoptOutcome, AdoptStoreError> {
    let tx = crate::store::write_transaction(conn)?;
    ensure_adopt_available(&tx)?;
    ensure_adopt_remote(&tx, expected_kind)?;
    ensure_backend_cohort(&tx, expected_kind)?;
    let legacy_backend_key = legacy_adopt_backend_key(expected_kind, adopted);
    if let Some(row) = find_adopted_ticket_by_identity(
        &tx,
        expected_kind,
        &adopted.backend_key,
        legacy_backend_key,
    )? {
        return Ok(AdoptOutcome::AlreadyExists(row));
    }
    let id = generate_internal_id(rng);
    let created_seq = sequences::next(&tx, "item_created_seq")?;
    // Adopt inserts Backend Tickets as accepted; Epics stay outside Selection
    // State (ADR-0027). This is its own intake decision, not an inheritance of
    // the `tk add` default, so it names `Accepted` explicitly. Selection State
    // remains a Local Field and Backend Pull never refreshes it.
    tx.execute(
        "insert into items(\
                id, display_value, item_class, ticket_kind, priority, title, body, \
                origin, backend_kind, backend_key, status, selection_state, \
                created_seq, created_at, updated_at\
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'backend', ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            id,
            adopted.display_id,
            ItemClass::Ticket.text(),
            adopted.ticket_kind.text(),
            "P2",
            adopted.title,
            adopted.body,
            expected_kind.text(),
            adopted.backend_key,
            adopted.status.text(),
            SelectionState::Accepted.text(),
            created_seq,
            now,
        ],
    )?;

    // Display ID resolver row. A unique/PK violation means the Display ID
    // is already claimed by another Item → DisplayIdCollision. Dropping
    // `tx` on the early return rolls back the orphaned `items` insert.
    match tx.execute(
        "insert into item_ids(value, source, item_id, created_at) \
             values (?1, 'display', ?2, ?3)",
        params![adopted.display_id, id, now],
    ) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
        {
            return Err(AdoptStoreError::DisplayIdCollision(
                adopted.display_id.clone(),
            ));
        }
        Err(other) => return Err(other.into()),
    }
    tx.commit()?;
    Ok(AdoptOutcome::Inserted(
        find_adopted_ticket_by_identity(
            conn,
            expected_kind,
            &adopted.backend_key,
            legacy_backend_key,
        )?
        .expect("the adopted Backend Ticket was committed"),
    ))
}

/// Refuse Adopt before any Backend read while creation certainty is unresolved.
pub fn ensure_adopt_available(conn: &Connection) -> Result<(), AdoptStoreError> {
    if let Some(sequence) = applying_mutation_sequence(conn)? {
        return Err(AdoptStoreError::ApplyingMutation(sequence));
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Mutation Log decode (the engine's applicable-set load)
// ──────────────────────────────────────────────────────────────────────────

/// Error returned by the applicable-set load — [`load_applicable_mutations`]
/// and the per-Mutation [`resolve_backend_operation`].
#[derive(Debug, Error)]
pub enum LoadApplicableError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// SQL CHECK accepted a `mutation_type` text value the [`MutationType`]
    /// enum does not decode — fires when schema and enum drift.
    #[error("unrecognised mutation_type: {0}")]
    UnknownMutationType(String),
    /// `mutation_type` decoded, but [`MutationPayload`] has no matching variant
    /// (today: `*_external_blocker`). A forward-compatibility guard so a
    /// future Mutation kind cannot be silently skipped.
    #[error("no payload projection for mutation kind: {0}")]
    PayloadVariantMissing(MutationType),
    /// `payload_json` parsed as JSON (the column's CHECK guarantees that) but
    /// did not match the variant's shape — Repository Store corruption.
    #[error("malformed payload_json: {0}")]
    PayloadJson(#[from] serde_json::Error),
    #[error("mutation {mutation_type} cannot target Item Class {item_class}")]
    OperationShapeMismatch {
        mutation_type: MutationType,
        item_class: ItemClass,
    },
    #[error("mutation {mutation_type} requires Backend identity for Item {item_id}")]
    MissingBackendIdentity {
        mutation_type: MutationType,
        item_id: String,
    },
    #[error(
        "mutation {mutation_type} requires Item {item_id} to be {expected}, but it is {actual}"
    )]
    CounterpartClassMismatch {
        mutation_type: MutationType,
        item_id: String,
        expected: ItemClass,
        actual: ItemClass,
    },
}

/// One applicable Mutation Log entry, decoded but not yet bound to a backend
/// identity.
///
/// The engine materialises these once per run; the backend identities that
/// complete a [`BackendOperation`] are resolved per Mutation instead
/// ([`resolve_backend_operation`]), because a Promotion identity applied earlier in
/// the same run changes them (ADR-0036).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicableMutationRow {
    pub sequence: i64,
    pub mutation_type: MutationType,
    /// Internal stable `items.id` (NOT the Display ID — promote-safe).
    pub item_id: String,
    pub item_class: ItemClass,
    pub payload: MutationPayload,
}

/// Store-owned Mutation metadata paired with one Adapter-facing operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationDelivery {
    pub sequence: i64,
    pub operation: BackendOperation,
}

/// Decode the typed Mutation Log entries the engine should (re)apply.
///
/// Returns `state in ('pending','failed')` rows in `sequence` order. Decode
/// stays batched so one undecodable row fails the run before any backend write;
/// a single query is a consistent snapshot, and `sequences::next` allocates
/// inside `begin immediate`, so sequence order and commit order agree and rows
/// committed after this query are absent.
pub fn load_applicable_mutations(
    conn: &Connection,
) -> Result<Vec<ApplicableMutationRow>, LoadApplicableError> {
    let mut stmt = conn.prepare(
        "select sequence, mutation_type, item_id, item_class, payload_json \
           from mutations \
          where state in ('pending','failed') \
          order by sequence asc",
    )?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let sequence: i64 = row.get(0)?;
        let type_text: String = row.get(1)?;
        let item_id: String = row.get(2)?;
        let item_class: ItemClass = row.get(3)?;
        let payload_text: String = row.get(4)?;

        let mutation_type = MutationType::from_str(&type_text)
            .map_err(|_| LoadApplicableError::UnknownMutationType(type_text))?;
        let payload = decode_mutation_payload(mutation_type, &payload_text)?;

        out.push(ApplicableMutationRow {
            sequence,
            mutation_type,
            item_id,
            item_class,
            payload,
        });
    }

    Ok(out)
}

/// Resolve one decoded row into a typed edit or creation operation.
///
/// Called immediately before each Apply rather than at load time: a Promotion
/// receipt committed earlier in the same run gives its Item a backend identity
/// that the Mutations ordered behind it need (ADR-0036 "Resolve backend
/// identity when the applicable Mutations are loaded" — rejected).
///
/// Dependency and Epic-membership payloads carry a counterpart's internal
/// `items.id` (promote-safe). Its Backend identity is resolved immediately
/// before delivery; a missing identity rejects resolution before the Adapter
/// is invoked.
pub fn resolve_backend_operation(
    conn: &Connection,
    row: ApplicableMutationRow,
) -> Result<MutationDelivery, LoadApplicableError> {
    let ApplicableMutationRow {
        sequence,
        mutation_type,
        item_id,
        item_class,
        payload,
    } = row;
    let target = || backend_address(conn, &item_id, mutation_type);
    let shape_error = || LoadApplicableError::OperationShapeMismatch {
        mutation_type,
        item_class,
    };

    let operation = match (mutation_type, payload) {
        (MutationType::UpdateTicket, MutationPayload::UpdateTitleBody(snapshot)) => {
            if item_class != ItemClass::Ticket {
                return Err(shape_error());
            }
            BackendOperation::Edit(BackendEdit::UpdateTicket {
                ticket: target()?,
                snapshot,
            })
        }
        (MutationType::UpdateEpic, MutationPayload::UpdateTitleBody(snapshot)) => {
            if item_class != ItemClass::Epic {
                return Err(shape_error());
            }
            BackendOperation::Edit(BackendEdit::UpdateEpic {
                epic: target()?,
                snapshot,
            })
        }
        (MutationType::SetItemStatus, MutationPayload::ItemStatus(change)) => {
            BackendOperation::Edit(BackendEdit::SetItemStatus {
                item: target()?,
                change,
            })
        }
        (MutationType::AddDependency, MutationPayload::DependencyRef(dependency)) => {
            BackendOperation::Edit(BackendEdit::AddDependency {
                blocked: target()?,
                blocking: backend_address(conn, &dependency.blocking_id, mutation_type)?,
            })
        }
        (MutationType::RemoveDependency, MutationPayload::DependencyRef(dependency)) => {
            BackendOperation::Edit(BackendEdit::RemoveDependency {
                blocked: target()?,
                blocking: backend_address(conn, &dependency.blocking_id, mutation_type)?,
            })
        }
        (MutationType::AddTicketToEpic, MutationPayload::EpicRef(reference)) => {
            if item_class != ItemClass::Ticket {
                return Err(shape_error());
            }
            BackendOperation::Edit(BackendEdit::AddTicketToEpic {
                ticket: target()?,
                epic: backend_address_of_class(
                    conn,
                    &reference.epic_id,
                    mutation_type,
                    ItemClass::Epic,
                )?,
            })
        }
        (MutationType::RemoveTicketFromEpic, MutationPayload::EpicRef(_)) => {
            if item_class != ItemClass::Ticket {
                return Err(shape_error());
            }
            // The payload keeps the Epic it left for the Sync Log and the
            // emission-time same-Backend gate, but delivery does not resolve
            // it: clearing a 0..1 slot needs no counterpart address.
            BackendOperation::Edit(BackendEdit::RemoveTicketFromEpic { ticket: target()? })
        }
        (MutationType::PromoteTicket, MutationPayload::Promotion(promotion)) => {
            if item_class != ItemClass::Ticket {
                return Err(shape_error());
            }
            let Promotion { title, body, .. } = promotion;
            BackendOperation::Create(BackendCreate::Ticket {
                snapshot: TitleBody { title, body },
            })
        }
        (MutationType::PromoteEpic, MutationPayload::Promotion(promotion)) => {
            if item_class != ItemClass::Epic {
                return Err(shape_error());
            }
            let Promotion { title, body, .. } = promotion;
            BackendOperation::Create(BackendCreate::Epic {
                snapshot: TitleBody { title, body },
            })
        }
        (MutationType::AddExternalBlocker | MutationType::ResolveExternalBlocker, _) => {
            return Err(LoadApplicableError::PayloadVariantMissing(mutation_type));
        }
        _ => return Err(shape_error()),
    };

    Ok(MutationDelivery {
        sequence,
        operation,
    })
}

fn backend_address(
    conn: &Connection,
    item_id: &str,
    mutation_type: MutationType,
) -> Result<BackendItemAddress, LoadApplicableError> {
    let backend_key = conn
        .query_row(
            "select backend_key from items where id = ?1",
            params![item_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    backend_key
        .map(|backend_key| BackendItemAddress { backend_key })
        .ok_or_else(|| LoadApplicableError::MissingBackendIdentity {
            mutation_type,
            item_id: item_id.to_owned(),
        })
}

fn backend_address_of_class(
    conn: &Connection,
    item_id: &str,
    mutation_type: MutationType,
    expected: ItemClass,
) -> Result<BackendItemAddress, LoadApplicableError> {
    let row: Option<(ItemClass, Option<String>)> = conn
        .query_row(
            "select item_class, backend_key from items where id = ?1",
            params![item_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (actual, backend_key) = row.ok_or_else(|| LoadApplicableError::MissingBackendIdentity {
        mutation_type,
        item_id: item_id.to_owned(),
    })?;
    if actual != expected {
        return Err(LoadApplicableError::CounterpartClassMismatch {
            mutation_type,
            item_id: item_id.to_owned(),
            expected,
            actual,
        });
    }
    backend_key
        .map(|backend_key| BackendItemAddress { backend_key })
        .ok_or_else(|| LoadApplicableError::MissingBackendIdentity {
            mutation_type,
            item_id: item_id.to_owned(),
        })
}

/// Decode a `payload_json` text column into the [`MutationPayload`] variant the
/// [`MutationType`] selects.
fn decode_mutation_payload(
    mutation_type: MutationType,
    payload_text: &str,
) -> Result<MutationPayload, LoadApplicableError> {
    use MutationType as Mt;
    Ok(match mutation_type {
        Mt::UpdateTicket | Mt::UpdateEpic => {
            MutationPayload::UpdateTitleBody(serde_json::from_str::<TitleBody>(payload_text)?)
        }
        Mt::AddTicketToEpic | Mt::RemoveTicketFromEpic => {
            MutationPayload::EpicRef(serde_json::from_str::<EpicRef>(payload_text)?)
        }
        Mt::SetItemStatus => {
            MutationPayload::ItemStatus(serde_json::from_str::<StatusChange>(payload_text)?)
        }
        Mt::AddDependency | Mt::RemoveDependency => {
            MutationPayload::DependencyRef(serde_json::from_str::<DependencyRef>(payload_text)?)
        }
        Mt::PromoteTicket | Mt::PromoteEpic => {
            MutationPayload::Promotion(serde_json::from_str::<Promotion>(payload_text)?)
        }
        Mt::AddExternalBlocker | Mt::ResolveExternalBlocker => {
            return Err(LoadApplicableError::PayloadVariantMissing(mutation_type));
        }
    })
}

// ──────────────────────────────────────────────────────────────────────────
// Backend outcomes / mark skipped / cursor
// ──────────────────────────────────────────────────────────────────────────

/// Error returned while persisting a Backend write outcome.
#[derive(Debug, Error)]
pub enum PersistMutationOutcomeError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches `sequence`.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// The matched row's prior state is `applied` or `skipped` — the engine
    /// must never request a transition out of a terminal state.
    #[error("mutation {0} is not in an applicable state")]
    MutationNotApplicable(i64),
    /// A Promotion receipt or creation attempt targeted an Item that is no
    /// longer Local. This contradicts the retained Promotion intent.
    #[error("Promotion Mutation {sequence} targets non-Local Item {item_id}")]
    TargetNotLocal { sequence: i64, item_id: String },
    #[error("Mutation {0} has an indeterminate Backend creation outcome")]
    ApplyingMutation(i64),
    /// The Store API does not match the row's Mutation Type.
    #[error("mutation {sequence} of type {mutation_type} cannot carry this receipt")]
    OperationShapeMismatch {
        sequence: i64,
        mutation_type: MutationType,
    },
    /// A `promote_*` row's `payload_json` did not decode as a [`Promotion`]
    /// payload — Repository Store corruption, the same fault
    /// [`LoadApplicableError::PayloadJson`] names on the load side.
    #[error("malformed payload_json: {0}")]
    PayloadJson(#[from] serde_json::Error),
}

/// Persist an edit acknowledgement or rejection against its Mutation Log row.
///
/// A missing or terminal row is rejected, and a Promotion row cannot pass
/// through the edit outcome boundary.
pub fn persist_edit_outcome(
    conn: &mut Connection,
    sequence: i64,
    outcome: &BackendEditOutcome,
    now: &str,
) -> Result<(), PersistMutationOutcomeError> {
    let tx = crate::store::write_transaction(conn)?;
    if let Some(applying) = applying_mutation_sequence(&tx)? {
        return Err(PersistMutationOutcomeError::ApplyingMutation(applying));
    }
    let (_, mutation_type) = applicable_outcome_row(&tx, sequence)?;
    if mutation_type.is_promotion() {
        return Err(PersistMutationOutcomeError::OperationShapeMismatch {
            sequence,
            mutation_type,
        });
    }

    match outcome {
        BackendEditOutcome::Acknowledged => mutations::mark_applied(&tx, sequence, now)?,
        BackendEditOutcome::Rejected(failure) => persist_failed(&tx, sequence, failure, now)?,
    }

    tx.commit()?;
    Ok(())
}

/// Persist one Backend creation outcome and apply a confirmed identity in the
/// same transaction as the Mutation state transition.
pub fn persist_create_outcome(
    conn: &mut Connection,
    sequence: i64,
    outcome: &BackendCreateOutcome,
    now: &str,
) -> Result<(), PersistMutationOutcomeError> {
    let tx = crate::store::write_transaction(conn)?;
    let row: Option<(MutationState, MutationType, String, String)> = tx
        .query_row(
            "select state, mutation_type, item_id, payload_json \
               from mutations where sequence = ?1",
            params![sequence],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (prior, mutation_type, item_id, payload_json) =
        row.ok_or(PersistMutationOutcomeError::MutationNotFound(sequence))?;
    if prior != MutationState::Applying {
        return Err(PersistMutationOutcomeError::MutationNotApplicable(sequence));
    }
    if !mutation_type.is_promotion() {
        return Err(PersistMutationOutcomeError::OperationShapeMismatch {
            sequence,
            mutation_type,
        });
    }
    let origin: Origin = tx.query_row(
        "select origin from items where id = ?1",
        params![&item_id],
        |r| r.get(0),
    )?;
    if origin != Origin::Local {
        return Err(PersistMutationOutcomeError::TargetNotLocal { sequence, item_id });
    }

    match outcome {
        BackendCreateOutcome::Created(identity) => {
            let payload: Promotion = serde_json::from_str(&payload_json)?;
            crate::store::promotion::apply_receipt(
                &tx,
                &item_id,
                &payload.backend_kind,
                identity,
                now,
            )
            .map_err(|error| match error {
                crate::store::promotion::ApplyReceiptError::Storage(error) => {
                    PersistMutationOutcomeError::Storage(error)
                }
                crate::store::promotion::ApplyReceiptError::ItemNotFound(_) => {
                    PersistMutationOutcomeError::MutationNotFound(sequence)
                }
                crate::store::promotion::ApplyReceiptError::TargetNotLocal(_) => {
                    PersistMutationOutcomeError::TargetNotLocal {
                        sequence,
                        item_id: item_id.clone(),
                    }
                }
            })?;
            mutations::mark_applied(&tx, sequence, now)?;
        }
        BackendCreateOutcome::Rejected(failure) => {
            persist_failed(&tx, sequence, failure, now)?;
        }
        BackendCreateOutcome::Indeterminate(failure) => {
            persist_applying_failure(&tx, sequence, failure, now)?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Durably mark a Promotion as in flight before invoking non-idempotent
/// Backend creation.
pub fn begin_create(
    conn: &mut Connection,
    sequence: i64,
    now: &str,
) -> Result<(), PersistMutationOutcomeError> {
    let tx = crate::store::write_transaction(conn)?;
    if let Some(applying) = applying_mutation_sequence(&tx)? {
        return Err(PersistMutationOutcomeError::ApplyingMutation(applying));
    }
    let row: Option<(MutationState, MutationType, Origin, String)> = tx
        .query_row(
            "select m.state, m.mutation_type, i.origin, m.item_id \
               from mutations m join items i on i.id = m.item_id \
              where m.sequence = ?1",
            params![sequence],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (state, mutation_type, origin, item_id) =
        row.ok_or(PersistMutationOutcomeError::MutationNotFound(sequence))?;
    if !matches!(state, MutationState::Pending | MutationState::Failed) {
        return Err(PersistMutationOutcomeError::MutationNotApplicable(sequence));
    }
    if !mutation_type.is_promotion() {
        return Err(PersistMutationOutcomeError::OperationShapeMismatch {
            sequence,
            mutation_type,
        });
    }
    if origin != Origin::Local {
        return Err(PersistMutationOutcomeError::TargetNotLocal { sequence, item_id });
    }
    tx.execute(
        "update mutations \
            set state = 'applying', failure_json = null, state_changed_at = ?2 \
          where sequence = ?1",
        params![sequence, now],
    )?;
    tx.commit()?;
    Ok(())
}

fn applicable_outcome_row(
    conn: &Connection,
    sequence: i64,
) -> Result<(MutationState, MutationType), PersistMutationOutcomeError> {
    let row = conn
        .query_row(
            "select state, mutation_type \
               from mutations where sequence = ?1",
            params![sequence],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let row = row.ok_or(PersistMutationOutcomeError::MutationNotFound(sequence))?;
    if !matches!(row.0, MutationState::Pending | MutationState::Failed) {
        return Err(PersistMutationOutcomeError::MutationNotApplicable(sequence));
    }
    Ok(row)
}

fn persist_failed(
    conn: &Connection,
    sequence: i64,
    failure: &Failure,
    now: &str,
) -> rusqlite::Result<()> {
    let failure_json = serde_json::to_string(failure).expect("Failure serializes infallibly");
    conn.execute(
        "update mutations \
            set state = 'failed', failure_json = ?2, state_changed_at = ?3 \
          where sequence = ?1",
        params![sequence, failure_json, now],
    )?;
    Ok(())
}

fn persist_applying_failure(
    conn: &Connection,
    sequence: i64,
    failure: &Failure,
    now: &str,
) -> rusqlite::Result<()> {
    let failure_json = serde_json::to_string(failure).expect("Failure serializes infallibly");
    conn.execute(
        "update mutations \
            set failure_json = ?2, state_changed_at = ?3 \
          where sequence = ?1 and state = 'applying'",
        params![sequence, failure_json, now],
    )?;
    Ok(())
}

/// Return the sequence of the globally blocking `applying` Mutation, if any.
pub fn applying_mutation_sequence(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "select sequence from mutations where state = 'applying' order by sequence limit 1",
        [],
        |r| r.get(0),
    )
    .optional()
}

/// Error returned by [`mark_mutation_skipped`].
#[derive(Debug, Error)]
pub enum MarkSkippedError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches `sequence`.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// The matched row's prior state is not `failed`. Skipping is only a
    /// curation tool for Mutations the backend has already rejected.
    #[error("mutation {0} is not in the failed state")]
    MutationNotFailed(i64),
    /// The matched row's Mutation Type is `promote_ticket` / `promote_epic`.
    /// Every Mutation the same Promotion Operation queued behind it targets,
    /// or names as a counterpart, an Item whose backend identity only this
    /// Promotion's receipt would assign (ADR-0036 Pending Promotion);
    /// skipping it would leave those Mutations with no key to apply against.
    ///
    /// Abandoning a Pending Promotion is a broader recovery decision than
    /// curating one rejected Mutation, and this build offers no way to do it —
    /// the refusal says so, so a reader stops hunting for the flag.
    #[error("mutation {0} is a Promotion and cannot be skipped")]
    CannotSkipPromotion(i64),
}

/// Transition a `failed` Mutation Log entry into `skipped`, inside its own
/// transaction. Refuses a Mutation that is not `failed`, or whose Mutation
/// Type is `promote_ticket` / `promote_epic` ([`MarkSkippedError::CannotSkipPromotion`]).
/// Clears no metadata — the latest `failure_json` is preserved so `tk sync
/// log` can show why the Mutation was abandoned.
pub fn mark_mutation_skipped(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    sequence: i64,
    now: &str,
) -> Result<(), MarkSkippedError> {
    let tx = crate::store::write_transaction(conn)?;

    let row: Option<(MutationState, MutationType)> = tx
        .query_row(
            "select state, mutation_type from mutations where sequence = ?1",
            params![sequence],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (prior, mutation_type) = row.ok_or(MarkSkippedError::MutationNotFound(sequence))?;
    if mutation_type.is_promotion() {
        return Err(MarkSkippedError::CannotSkipPromotion(sequence));
    }
    if prior != MutationState::Failed {
        return Err(MarkSkippedError::MutationNotFailed(sequence));
    }

    tx.execute(
        "update mutations \
            set state = 'skipped', state_changed_at = ?2 \
          where sequence = ?1",
        params![sequence, now],
    )?;

    tx.commit()?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Remote read + pending/failed count
// ──────────────────────────────────────────────────────────────────────────

/// Loaded copy of the singleton Remote configuration plus its Sync Cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRow {
    pub backend_kind: String,
    pub config_json: String,
    pub last_applied_sequence: i64,
}

/// Read the v1 singleton Remote configuration plus its Sync Cursor. Returns
/// `None` when no Remote is configured.
pub fn get_remote(conn: &Connection) -> rusqlite::Result<Option<RemoteRow>> {
    conn.query_row(
        "select r.backend_kind, r.config_json, c.last_applied_sequence \
           from remotes r \
           join sync_cursors c on c.remote_name = r.name \
          where r.name = 'primary'",
        [],
        |row| {
            Ok(RemoteRow {
                backend_kind: row.get(0)?,
                config_json: row.get(1)?,
                last_applied_sequence: row.get(2)?,
            })
        },
    )
    .optional()
}

/// The rendered fields of one Backend item, addressed by its backend identity.
/// `tk adopt` renders its outcome from this stored row, keeping the displayed
/// Priority tied to the Repository Store rather than Adapter input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendItemRow {
    pub display_id: String,
    pub ticket_kind: Option<TicketKind>,
    pub priority: Option<Priority>,
    pub status: ItemStatus,
    pub title: String,
}

/// Look up a Backend Ticket for an idempotent Adopt.
///
/// Exact canonical keys make a repeated Adopt Store-only without equating
/// same-numbered issues from different repositories. Legacy numeric keys stay
/// exact-matchable; a Backend read canonicalizes other user input before the
/// transactional idempotence check.
pub fn find_adopted_ticket(
    conn: &Connection,
    backend_kind: BackendKind,
    input: &str,
) -> Result<Option<BackendItemRow>, AdoptStoreError> {
    find_adopted_ticket_by_identity(conn, backend_kind, input, None)
}

fn find_adopted_ticket_by_identity(
    conn: &Connection,
    backend_kind: BackendKind,
    backend_key: &str,
    legacy_backend_key: Option<&str>,
) -> Result<Option<BackendItemRow>, AdoptStoreError> {
    let row = conn
        .query_row(
            "select item_class, display_value, ticket_kind, priority, status, title \
           from items \
          where backend_kind = ?1 \
            and (backend_key = ?2 or (?3 is not null and backend_key = ?3))",
            params![backend_kind.text(), backend_key, legacy_backend_key],
            |row| {
                Ok((
                    row.get::<_, ItemClass>(0)?,
                    BackendItemRow {
                        display_id: row.get(1)?,
                        ticket_kind: row.get(2)?,
                        priority: row.get(3)?,
                        status: row.get(4)?,
                        title: row.get(5)?,
                    },
                ))
            },
        )
        .optional()?;
    match row {
        Some((ItemClass::Ticket, row)) => Ok(Some(row)),
        Some((ItemClass::Epic, row)) => Err(AdoptStoreError::BackendItemIsEpic(row.display_id)),
        None => Ok(None),
    }
}

fn legacy_adopt_backend_key(backend_kind: BackendKind, adopted: &AdoptedItem) -> Option<&str> {
    if backend_kind != BackendKind::Github {
        return None;
    }
    let number = adopted.display_id.strip_prefix("gh-")?;
    let url_number = adopted.backend_key.rsplit_once("/issues/")?.1;
    (number == url_number && number.parse::<u64>().is_ok()).then_some(number)
}

/// Count Mutation Log entries in `pending` or `failed` state.
pub fn pending_or_failed_mutation_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "select count(*) from mutations where state in ('pending','failed')",
        [],
        |r| r.get(0),
    )
}

/// Backend keys the next Backend Pull should refresh, validated against the
/// Adapter kind before the engine performs any Backend call.
pub fn active_backend_keys(
    conn: &Connection,
    expected_kind: BackendKind,
) -> Result<Vec<String>, RefreshStoreError> {
    if let Some(sequence) = applying_mutation_sequence(conn)? {
        return Err(RefreshStoreError::ApplyingMutation(sequence));
    }
    ensure_backend_cohort(conn, expected_kind)?;
    let mut stmt = conn.prepare(
        "select backend_key from items \
          where backend_kind = ?1 and backend_key is not null \
            and status in ('open', 'active') \
          order by created_seq asc",
    )?;
    let mut rows = stmt.query([expected_kind.text()])?;
    let mut keys = Vec::new();
    while let Some(row) = rows.next()? {
        keys.push(row.get(0)?);
    }
    Ok(keys)
}

/// Merge collected refresh results atomically. Refreshes address already-known
/// keys and therefore cannot insert or alter identity, Origin, Display ID, or
/// Item Class.
pub fn merge_backend_refreshes(
    conn: &mut Connection,
    expected_kind: BackendKind,
    refreshes: &[(String, BackendItemRefresh)],
    now: &str,
) -> Result<(), RefreshStoreError> {
    if refreshes.is_empty() {
        return Ok(());
    }
    let tx = crate::store::write_transaction(conn)?;
    if let Some(sequence) = applying_mutation_sequence(&tx)? {
        return Err(RefreshStoreError::ApplyingMutation(sequence));
    }
    ensure_refresh_remote(&tx, expected_kind)?;
    ensure_backend_cohort(&tx, expected_kind)?;
    for (key, refresh) in refreshes {
        let existing: Option<(String, ItemClass)> = tx
            .query_row(
                "select id, item_class from items where backend_kind = ?1 and backend_key = ?2",
                params![expected_kind.text(), key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((item_id, item_class)) = existing else {
            continue;
        };
        let in_flight: Option<i64> = tx
            .query_row(
                "select 1 from mutations where item_id = ?1 and item_class = ?2 \
             and state in ('pending','failed') limit 1",
                params![item_id, item_class.text()],
                |row| row.get(0),
            )
            .optional()?;
        if in_flight.is_some() {
            continue;
        }
        tx.execute(
            "update items set title = ?2, body = ?3, updated_at = ?5, \
              ticket_kind = case when item_class = 'ticket' then coalesce(?6, ticket_kind) else ticket_kind end, \
              status = case when ?4 = 'active' and selection_state <> 'accepted' then 'open' else ?4 end \
              where id = ?1",
            params![item_id, refresh.title, refresh.body, refresh.status.text(), now,
                refresh.ticket_kind.map(TicketKind::text)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Return an invariant error when retained backend-bound state spans kinds.
fn ensure_single_backend_kind(
    conn: &Connection,
) -> Result<Option<BackendKind>, BackendCohortError> {
    let kinds = retained_backend_kinds(conn)?;
    if kinds.len() > 1 {
        return Err(BackendCohortError::MultipleBackendKinds);
    }
    Ok(kinds.first().copied())
}

/// Validate that retained Backend-bound state belongs to the operation's Adapter.
pub(crate) fn ensure_backend_cohort(
    conn: &Connection,
    expected: BackendKind,
) -> Result<(), BackendCohortError> {
    if let Some(retained) = ensure_single_backend_kind(conn)?
        && retained != expected
    {
        return Err(BackendCohortError::BackendKindMismatch { expected, retained });
    }
    Ok(())
}

fn retained_backend_kinds(conn: &Connection) -> Result<Vec<BackendKind>, BackendCohortError> {
    let mut stmt = conn.prepare(
        "select distinct backend_kind from ( \
           select backend_kind from items where backend_kind is not null \
           union all \
           select json_extract(payload_json, '$.backend_kind') from mutations \
             where mutation_type in ('promote_ticket', 'promote_epic') \
               and state in ('pending', 'failed', 'applying') \
         ) where backend_kind is not null",
    )?;
    let texts = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut kinds = Vec::with_capacity(texts.len());
    for text in texts {
        let kind = text
            .parse()
            .map_err(|_| BackendCohortError::UnknownBackendKind(text))?;
        kinds.push(kind);
    }
    Ok(kinds)
}

pub(crate) fn configured_remote_kind(conn: &Connection) -> rusqlite::Result<Option<BackendKind>> {
    let actual = conn
        .query_row(
            "select backend_kind from remotes where name = 'primary'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(actual.map(|kind| {
        kind.parse()
            .expect("remotes.backend_kind is protected by a CHECK constraint")
    }))
}

fn ensure_adopt_remote(conn: &Connection, expected: BackendKind) -> Result<(), AdoptStoreError> {
    let actual = configured_remote_kind(conn)?;
    if actual != Some(expected) {
        return Err(AdoptStoreError::RemoteChanged { expected, actual });
    }
    Ok(())
}

fn ensure_refresh_remote(
    conn: &Connection,
    expected: BackendKind,
) -> Result<(), RefreshStoreError> {
    let actual = configured_remote_kind(conn)?;
    if actual != Some(expected) {
        return Err(RefreshStoreError::RemoteChanged { expected, actual });
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Remote set / clear (tk remote set / tk remote clear)
// ──────────────────────────────────────────────────────────────────────────

/// Outcome of [`set_remote`]: whether a `remotes` row was created, or the call
/// was an idempotent no-op because one already existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetRemoteOutcome {
    Created,
    Unchanged,
}

/// Error returned by [`set_remote`].
#[derive(Debug, Error)]
pub enum SetRemoteError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendCohort(#[from] BackendCohortError),
    #[error(
        "cannot configure {requested} Remote while retained backend-bound state uses {retained}"
    )]
    BackendKindConflict {
        requested: BackendKind,
        retained: BackendKind,
    },
    #[error("Remote is already configured for {existing}; clear it before configuring {requested}")]
    RemoteKindConflict {
        requested: BackendKind,
        existing: BackendKind,
    },
}

/// Configure the v1 singleton Remote (`name = 'primary'`) and seed its Sync
/// Cursor at 0, inside one IMMEDIATE transaction.
///
/// Idempotent by the v1 model (ADR-0033) when the requested Backend kind is
/// already configured. A v1 GitHub Remote stores no repository, so re-running
/// `tk remote set github` changes nothing; replacing a Remote is therefore not
/// modelled — switching Backends is `tk remote clear` (orphan-guarded) then
/// `tk remote set`. Retained Backend Items and pending Promotions constrain the
/// replacement to their single Backend kind. `config_json` is the caller's
/// already-built Backend configuration (today always `{}` for GitHub).
pub fn set_remote(
    conn: &mut Connection,
    kind: BackendKind,
    config_json: &str,
    now: &str,
) -> Result<SetRemoteOutcome, SetRemoteError> {
    let tx = crate::store::write_transaction(conn)?;

    if let Some(retained) = ensure_single_backend_kind(&tx)? {
        if retained != kind {
            return Err(SetRemoteError::BackendKindConflict {
                requested: kind,
                retained,
            });
        }
    }

    if let Some(existing) = configured_remote_kind(&tx)? {
        return if existing == kind {
            Ok(SetRemoteOutcome::Unchanged)
        } else {
            Err(SetRemoteError::RemoteKindConflict {
                requested: kind,
                existing,
            })
        };
    }

    tx.execute(
        "insert into remotes(name, backend_kind, config_json, created_at, updated_at) \
         values ('primary', ?1, ?2, ?3, ?3)",
        params![kind.text(), config_json, now],
    )?;
    tx.execute(
        "insert into sync_cursors(remote_name, backend_kind, last_applied_sequence, updated_at) \
         values ('primary', ?1, 0, ?2)",
        params![kind.text(), now],
    )?;

    tx.commit()?;
    Ok(SetRemoteOutcome::Created)
}

/// Error returned by [`clear_remote`].
#[derive(Debug, Error)]
pub enum ClearRemoteError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `remotes` row to remove.
    #[error("no Remote configured")]
    NotConfigured,
    /// Pending or failed Mutations still target the Backend; removing the
    /// Remote would orphan them (CONTEXT.md). Carries the count for the
    /// verbatim diagnostic.
    #[error(
        "{0} pending or failed Mutation(s) would be orphaned; resolve them before clearing the Remote"
    )]
    WouldOrphan(i64),
    #[error("Mutation {0} has an indeterminate Backend creation outcome")]
    ApplyingMutation(i64),
}

/// Remove the v1 singleton Remote and its Sync Cursor, inside one IMMEDIATE
/// transaction, but only when no pending or failed Mutations would be orphaned
/// (ADR-0033, CONTEXT.md).
///
/// Deletes `sync_cursors` before `remotes` because the
/// `sync_cursors.remote_name` foreign key is `on delete restrict`. Backend
/// `items` and applied/skipped Mutation history are left intact; clearing is
/// not a Mutation. A refusal drops the transaction, so nothing is removed.
pub fn clear_remote(conn: &mut Connection) -> Result<(), ClearRemoteError> {
    let tx = crate::store::write_transaction(conn)?;

    let exists = tx
        .query_row("select 1 from remotes where name = 'primary'", [], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !exists {
        return Err(ClearRemoteError::NotConfigured);
    }

    if let Some(sequence) = applying_mutation_sequence(&tx)? {
        return Err(ClearRemoteError::ApplyingMutation(sequence));
    }

    let in_flight = pending_or_failed_mutation_count(&tx)?;
    if in_flight > 0 {
        return Err(ClearRemoteError::WouldOrphan(in_flight));
    }

    tx.execute("delete from sync_cursors where remote_name = 'primary'", [])?;
    tx.execute("delete from remotes where name = 'primary'", [])?;

    tx.commit()?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Mutation Log read (tk sync log)
// ──────────────────────────────────────────────────────────────────────────

/// Filter for the `tk sync log` list view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogListFilter {
    /// Pending + failed + applying + skipped (the default).
    Default,
    Pending,
    Failed,
    Skipped,
}

/// One row of the `tk sync log` list view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogListRow {
    pub sequence: i64,
    pub state: MutationState,
    pub mutation_type: MutationType,
    pub target_display_id: String,
    pub created_at: String,
    /// Decoded `Failure.detail`; set for failed and indeterminate applying rows.
    pub failure_detail: Option<String>,
    /// Decoded `Failure.class`; set for rows carrying failure evidence. `Some(Unknown)` is
    /// a failure tk could not classify — rendering suppresses it.
    pub failure_class: Option<FailureClass>,
}

/// One row of the `tk sync log <sequence>` detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDetailRow {
    pub sequence: i64,
    pub state: MutationState,
    pub mutation_type: MutationType,
    pub target_display_id: String,
    pub item_class: ItemClass,
    pub payload_json: String,
    pub failure_detail: Option<String>,
    pub failure_class: Option<FailureClass>,
    pub created_at: String,
    pub state_changed_at: String,
}

/// Error returned by [`list_mutation_log`] and [`show_mutation_log`].
#[derive(Debug, Error)]
pub enum LogError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// No `mutations` row matches the supplied sequence.
    #[error("mutation {0} not found")]
    MutationNotFound(i64),
    /// Persisted `failure_json` did not decode as a [`Failure`] record.
    #[error("malformed failure_json: {0}")]
    FailureJson(#[from] serde_json::Error),
}

/// Return Mutation Log rows matching `filter` in ascending sequence order.
pub fn list_mutation_log(
    conn: &Connection,
    filter: LogListFilter,
) -> Result<Vec<LogListRow>, LogError> {
    let where_clause = match filter {
        LogListFilter::Default => "where m.state in ('pending', 'failed', 'applying', 'skipped')",
        LogListFilter::Pending => "where m.state = 'pending'",
        LogListFilter::Failed => "where m.state = 'failed'",
        LogListFilter::Skipped => "where m.state = 'skipped'",
    };
    let sql = format!(
        "select m.sequence, m.state, m.mutation_type, i.display_value, m.created_at, m.failure_json \
           from mutations m \
           join items i on i.id = m.item_id and i.item_class = m.item_class \
          {where_clause} \
          order by m.sequence asc"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let raw_failure: Option<String> = row.get(5)?;
        let failure = raw_failure.map(|raw| decode_failure(&raw)).transpose()?;
        let (failure_detail, failure_class) = match failure {
            Some(f) => (Some(f.detail), Some(f.class)),
            None => (None, None),
        };
        out.push(LogListRow {
            sequence: row.get(0)?,
            state: row.get(1)?,
            mutation_type: row.get(2)?,
            target_display_id: row.get(3)?,
            created_at: row.get(4)?,
            failure_detail,
            failure_class,
        });
    }
    Ok(out)
}

/// Look up one Mutation Log entry by sequence and return its full detail.
pub fn show_mutation_log(conn: &Connection, sequence: i64) -> Result<LogDetailRow, LogError> {
    // The closure stashes the raw `failure_json` in `failure_detail`; it is
    // decoded into the typed Failure after the query, because a rusqlite row
    // closure can only surface `rusqlite::Error`, not `LogError`.
    let mut detail = conn
        .query_row(
            "select m.sequence, m.state, m.mutation_type, i.display_value, \
                    m.item_class, m.payload_json, m.failure_json, \
                    m.created_at, m.state_changed_at \
               from mutations m \
               join items i on i.id = m.item_id and i.item_class = m.item_class \
              where m.sequence = ?1",
            params![sequence],
            |r| {
                Ok(LogDetailRow {
                    sequence: r.get(0)?,
                    state: r.get(1)?,
                    mutation_type: r.get(2)?,
                    target_display_id: r.get(3)?,
                    item_class: r.get(4)?,
                    payload_json: r.get(5)?,
                    failure_detail: r.get(6)?,
                    failure_class: None,
                    created_at: r.get(7)?,
                    state_changed_at: r.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or(LogError::MutationNotFound(sequence))?;

    if let Some(raw) = detail.failure_detail.take() {
        let failure = decode_failure(&raw)?;
        detail.failure_detail = Some(failure.detail);
        detail.failure_class = Some(failure.class);
    }
    Ok(detail)
}

/// Decode the `failure_json` text column into the typed [`Failure`] — the
/// inverse of the encoder in the outcome-persistence helpers. A legacy
/// `{"detail":"…"}` row decodes with class `unknown` and no retry hint.
fn decode_failure(raw: &str) -> Result<Failure, LogError> {
    Ok(serde_json::from_str(raw)?)
}

#[cfg(test)]
mod directional_tests {
    use super::*;
    use crate::store::migrations;
    use rand::SeedableRng;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    const NOW: &str = "2026-08-09T00:00:00Z";

    fn open() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, NOW).unwrap();
        set_remote(&mut conn, BackendKind::Github, "{}", NOW).unwrap();
        conn
    }

    fn adopted(key: &str, display_id: &str) -> AdoptedItem {
        AdoptedItem {
            backend_key: key.into(),
            display_id: display_id.into(),
            ticket_kind: TicketKind::Task,
            title: "Original".into(),
            body: "Original body".into(),
            status: ItemStatus::Open,
        }
    }

    #[test]
    fn canonical_adopt_is_idempotent_without_overwriting_stored_fields() {
        let mut conn = open();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let first = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42"),
            NOW,
        )
        .unwrap();
        assert!(matches!(first, AdoptOutcome::Inserted(_)));

        let mut canonical = adopted("42", "gh-42");
        canonical.title = "Changed remotely".into();
        let second =
            adopt_backend_ticket(&mut conn, BackendKind::Github, &mut rng, &canonical, NOW)
                .unwrap();
        assert!(matches!(second, AdoptOutcome::AlreadyExists(_)));
        assert_eq!(
            find_adopted_ticket(&conn, BackendKind::Github, "42")
                .unwrap()
                .unwrap()
                .title,
            "Original"
        );
    }

    #[test]
    fn adopt_does_not_alias_same_numbered_issues_across_repositories() {
        let mut conn = open();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("https://github.com/one/repo/issues/42", "gh-42"),
            NOW,
        )
        .unwrap();

        assert!(
            find_adopted_ticket(
                &conn,
                BackendKind::Github,
                "https://github.com/other/repo/issues/42",
            )
            .unwrap()
            .is_none()
        );
        assert!(matches!(
            adopt_backend_ticket(
                &mut conn,
                BackendKind::Github,
                &mut rng,
                &adopted("https://github.com/other/repo/issues/42", "gh-42"),
                NOW,
            ),
            Err(AdoptStoreError::DisplayIdCollision(id)) if id == "gh-42"
        ));
    }

    #[test]
    fn concurrent_canonical_adopt_inserts_once_without_consuming_a_second_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tk.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            conn.execute_batch("pragma foreign_keys = on").unwrap();
            migrations::apply_all(&mut conn, NOW).unwrap();
            set_remote(&mut conn, BackendKind::Github, "{}", NOW).unwrap();
        }

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for seed in [7, 11] {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut conn = Connection::open(path).unwrap();
                conn.busy_timeout(Duration::from_secs(5)).unwrap();
                conn.execute_batch("pragma foreign_keys = on").unwrap();
                let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
                barrier.wait();
                adopt_backend_ticket(
                    &mut conn,
                    BackendKind::Github,
                    &mut rng,
                    &adopted("42", "gh-42"),
                    NOW,
                )
                .map(|outcome| matches!(outcome, AdoptOutcome::Inserted(_)))
                .map_err(|error| error.to_string())
            }));
        }

        let mut inserted = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        inserted.sort_unstable();
        assert_eq!(inserted, [false, true]);

        let conn = Connection::open(path).unwrap();
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        let resolver_count: i64 = conn
            .query_row("select count(*) from item_ids", [], |row| row.get(0))
            .unwrap();
        let created_sequence: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((item_count, resolver_count, created_sequence), (1, 1, 1));
    }

    #[test]
    fn canonical_adopt_refuses_when_the_remote_was_cleared_after_the_read() {
        let mut conn = open();
        clear_remote(&mut conn).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);

        let error = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42"),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AdoptStoreError::RemoteChanged {
                expected: BackendKind::Github,
                actual: None,
            }
        ));
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        let created_sequence: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((item_count, created_sequence), (0, 0));
    }

    #[test]
    fn canonical_adopt_refuses_when_the_remote_kind_changed_after_the_read() {
        let mut conn = open();
        clear_remote(&mut conn).unwrap();
        set_remote(&mut conn, BackendKind::Jira, "{}", NOW).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);

        let error = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42"),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AdoptStoreError::RemoteChanged {
                expected: BackendKind::Github,
                actual: Some(BackendKind::Jira),
            }
        ));
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        let created_sequence: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((item_count, created_sequence), (0, 0));
    }

    #[test]
    fn canonical_adopt_refuses_a_remote_kind_incompatible_with_retained_items() {
        let mut conn = open();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("1", "gh-1"),
            NOW,
        )
        .unwrap();
        conn.execute(
            "update remotes set backend_kind = 'jira' where name = 'primary'",
            [],
        )
        .unwrap();

        let error = adopt_backend_ticket(
            &mut conn,
            BackendKind::Jira,
            &mut rng,
            &adopted("2", "jira-2"),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AdoptStoreError::BackendCohort(BackendCohortError::BackendKindMismatch {
                expected: BackendKind::Jira,
                retained: BackendKind::Github,
            })
        ));
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(item_count, 1);
    }

    #[test]
    fn refresh_preserves_ticket_identity_and_only_updates_owned_fields() {
        let mut conn = open();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42"),
            NOW,
        )
        .unwrap();
        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[(
                "42".into(),
                BackendItemRefresh {
                    title: "Fresh".into(),
                    body: "Fresh body".into(),
                    status: ItemStatus::Active,
                    ticket_kind: Some(TicketKind::Bug),
                },
            )],
            NOW,
        )
        .unwrap();
        let row = find_adopted_ticket(&conn, BackendKind::Github, "42")
            .unwrap()
            .unwrap();
        assert_eq!(row.display_id, "gh-42");
        assert_eq!(row.title, "Fresh");
        assert_eq!(row.ticket_kind, Some(TicketKind::Bug));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn adopted(
        backend_key: &str,
        display_id: &str,
        title: &str,
        status: ItemStatus,
    ) -> AdoptedItem {
        AdoptedItem {
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            ticket_kind: TicketKind::Task,
            title: title.into(),
            body: "Body".into(),
            status,
        }
    }

    fn refresh(title: &str, status: ItemStatus) -> BackendItemRefresh {
        BackendItemRefresh {
            title: title.into(),
            body: "Body".into(),
            status,
            ticket_kind: Some(TicketKind::Task),
        }
    }

    fn backend_ticket(conn: &Connection, id: &str, display: &str, key: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Old",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some(key),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    // ---- merge ----------------------------------------------------------

    #[test]
    fn canonical_adopt_inserts_new_backend_ticket() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut rng = StdRng::seed_from_u64(0);
        let outcome = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("1", "gh-1", "First", ItemStatus::Open),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();
        assert!(matches!(outcome, AdoptOutcome::Inserted(_)));

        let (title, origin, kind, source, selection, priority): (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "select i.title, i.origin, i.backend_kind, \
                        (select source from item_ids where value = i.display_value), \
                        i.selection_state, i.priority \
                   from items i where i.display_value = 'gh-1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(title, "First");
        assert_eq!(origin, "backend");
        assert_eq!(kind, "github");
        assert_eq!(source, "display");
        // Imported Backend Tickets default to accepted at Priority P2 (ADR-0027);
        // tripwire on both seed literals so a wrong value cannot pass silently.
        assert_eq!(selection.as_deref(), Some("accepted"));
        assert_eq!(priority.as_deref(), Some("P2"));
    }

    #[test]
    fn refresh_skips_item_with_pending_mutation() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local Edit","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        // Local title set to the in-flight edit.
        conn.execute("update items set title = 'Local Edit' where id = 't1'", [])
            .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Stale Backend View", ItemStatus::Open))],
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            title, "Local Edit",
            "pending mutation must shield the local edit"
        );
    }

    #[test]
    fn refresh_updates_item_without_in_flight_mutation() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend Wins", ItemStatus::Active))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, status, updated): (String, String, String) = conn
            .query_row(
                "select title, status, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Backend Wins");
        assert_eq!(status, "active");
        assert_eq!(updated, "2026-05-20T00:00:00Z");
    }

    #[test]
    fn refresh_merge_refuses_when_the_remote_was_cleared_after_pull() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        clear_remote(&mut conn).unwrap();

        let error = merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[
                ("1".into(), refresh("New one", ItemStatus::Done)),
                ("2".into(), refresh("New two", ItemStatus::Active)),
            ],
            "2026-05-20T00:00:00Z",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            RefreshStoreError::RemoteChanged {
                expected: BackendKind::Github,
                actual: None,
            }
        ));
        let rows = conn
            .prepare("select title, status, updated_at from items order by created_seq")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "Old".into(),
                    "open".into(),
                    "2026-05-09T00:00:00.000Z".into()
                ),
                (
                    "Old".into(),
                    "open".into(),
                    "2026-05-09T00:00:00.000Z".into()
                ),
            ]
        );
    }

    #[test]
    fn refresh_merge_refuses_when_the_remote_kind_changed_after_pull() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        conn.execute(
            "update remotes set backend_kind = 'jira' where name = 'primary'",
            [],
        )
        .unwrap();

        let error = merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[
                ("1".into(), refresh("New one", ItemStatus::Done)),
                ("2".into(), refresh("New two", ItemStatus::Active)),
            ],
            "2026-05-20T00:00:00Z",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            RefreshStoreError::RemoteChanged {
                expected: BackendKind::Github,
                actual: Some(BackendKind::Jira),
            }
        ));
        let changed: i64 = conn
            .query_row(
                "select count(*) from items where title <> 'Old' or status <> 'open' \
                 or updated_at <> '2026-05-09T00:00:00.000Z'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(changed, 0);
    }

    #[test]
    fn refresh_missing_identity_does_not_insert_an_item() {
        let mut conn = open_seeded();
        seed_remote(&conn);

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("404".into(), refresh("Unknown", ItemStatus::Open))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn refresh_preserves_a_locally_authoritative_epic_class() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "gh-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("9".into(), refresh("Fresh Epic", ItemStatus::Active))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (class, ticket_kind, title): (String, Option<String>, String) = conn
            .query_row(
                "select item_class, ticket_kind, title from items where id = 'e1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(class, "epic");
        assert_eq!(ticket_kind, None);
        assert_eq!(title, "Fresh Epic");
    }

    #[test]
    fn canonical_adopt_display_id_collision_is_surfaced_and_rolls_back() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        // A local item already owns Display ID "gh-1".
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "local1",
                display: "gh-1",
                title: "Local",
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let mut rng = StdRng::seed_from_u64(0);
        // A *different* backend item claims the same Display ID.
        let err = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("99", "gh-1", "Backend", ItemStatus::Open),
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err();
        match err {
            AdoptStoreError::DisplayIdCollision(id) => assert_eq!(id, "gh-1"),
            other => panic!("expected DisplayIdCollision, got {other:?}"),
        }
        // Rollback: no orphaned backend item landed.
        let count: i64 = conn
            .query_row(
                "select count(*) from items where backend_key = '99'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn refresh_preserves_local_selection_state_and_priority() {
        // Selection State is a Local Field (ADR-0027); Backend Pull never reads
        // it back, so a non-active Pull merging title/status must leave a local
        // `parked` state and its Priority untouched. Regression lock against a
        // future edit adding Selection State or Priority to the refresh write.
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Old",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                selection_state: Some("parked"),
                priority: Some("P0"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend Title", ItemStatus::Open))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, selection, priority): (String, String, String) = conn
            .query_row(
                "select title, selection_state, priority from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Backend Title", "synced field still merges");
        assert_eq!(selection, "parked", "local Selection State preserved");
        assert_eq!(priority, "P0", "local Priority preserved");
    }

    #[test]
    fn refresh_clamps_active_to_open_on_a_parked_ticket() {
        // A backend Ticket imported `accepted`, then locally parked (status
        // open at park time — tk-76 Door 2). A later Pull reports it `active`.
        // Backend Pull is the fourth door on `active ⟹ accepted` (ADR-0029):
        // it must not flip held work `active`, so the incoming status clamps to
        // `open` while the local Selection State and Priority are preserved.
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Old",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                selection_state: Some("parked"),
                priority: Some("P1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend Active", ItemStatus::Active))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, status, selection, priority): (String, String, String, String) = conn
            .query_row(
                "select title, status, selection_state, priority from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "Backend Active", "non-status fields still merge");
        assert_eq!(
            status, "open",
            "active clamped to open on a non-accepted row"
        );
        assert_eq!(selection, "parked", "local Selection State preserved");
        assert_eq!(priority, "P1", "local Priority preserved");
    }

    // ---- load_applicable_mutations --------------------------------------

    #[test]
    fn load_applicable_returns_pending_and_failed_in_sequence_order() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        for (seq, state) in [
            (3, "pending"),
            (1, "failed"),
            (2, "applied"),
            (4, "skipped"),
        ] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: seq,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"X","body":""}"#,
                    state,
                    failure_json: if state == "failed" {
                        Some(r#"{"detail":"prior"}"#)
                    } else {
                        None
                    },
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }

        let views = load_applicable_mutations(&conn).unwrap();
        let seqs: Vec<i64> = views.iter().map(|v| v.sequence).collect();
        assert_eq!(seqs, vec![1, 3], "only pending+failed, sequence order");
    }

    #[test]
    fn load_applicable_decodes_each_payload_variant() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        match &rows[0].payload {
            MutationPayload::ItemStatus(s) => assert_eq!(s.status, "done"),
            other => panic!("expected ItemStatus, got {other:?}"),
        }
    }

    // ---- resolve_backend_operation --------------------------------------

    #[test]
    fn resolve_operation_binds_the_target_items_backend_identity() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        let resolved = resolve_backend_operation(&conn, rows.into_iter().next().unwrap()).unwrap();
        assert_eq!(resolved.sequence, 1);
        let BackendOperation::Edit(edit) = resolved.operation else {
            panic!("ordinary Mutation must resolve as an edit")
        };
        let BackendEdit::SetItemStatus { item, .. } = edit else {
            panic!("expected status edit")
        };
        assert_eq!(item.backend_key, "1");
    }

    #[test]
    fn resolve_operation_leaves_identity_none_for_a_pending_promotion_item() {
        // A Promotion Mutation targets a Local Item (ADR-0036), so there is no
        // backend identity to bind until its own receipt lands.
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"B","backend_kind":"github"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        let resolved = resolve_backend_operation(&conn, rows.into_iter().next().unwrap()).unwrap();
        assert_eq!(resolved.sequence, 1);
        let BackendOperation::Create(create) = resolved.operation else {
            panic!("Promotion must resolve as creation")
        };
        let BackendCreate::Ticket { snapshot } = create else {
            panic!("expected Ticket creation")
        };
        assert_eq!(snapshot.title, "T");
    }

    #[test]
    fn resolve_dependency_addresses_both_backend_items() {
        // A dependency Mutation's payload stores the Blocking Item's internal
        // id; delivery needs both backend keys.
        let conn = open_seeded();
        backend_ticket(&conn, "blocked", "gh-5", "5", 1);
        backend_ticket(&conn, "blocking", "gh-9", "9", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_dependency",
                item_id: "blocked",
                payload_json: r#"{"blocking_id":"blocking"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        let resolved = resolve_backend_operation(&conn, rows.into_iter().next().unwrap()).unwrap();
        let BackendOperation::Edit(edit) = resolved.operation else {
            panic!("Dependency must resolve as an edit")
        };
        let BackendEdit::AddDependency {
            blocked, blocking, ..
        } = edit
        else {
            panic!("expected dependency edit")
        };
        assert_eq!(blocked.backend_key, "5", "blocked item");
        assert_eq!(blocking.backend_key, "9", "blocking item");
    }

    #[test]
    fn resolve_operation_resolves_epic_membership_identities() {
        let conn = open_seeded();
        backend_ticket(&conn, "ticket", "gh-5", "5", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
                display: "gh-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_ticket_to_epic",
                item_id: "ticket",
                payload_json: r#"{"epic_id":"epic"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        let BackendOperation::Edit(BackendEdit::AddTicketToEpic { ticket, epic, .. }) =
            resolve_backend_operation(&conn, rows.into_iter().next().unwrap())
                .unwrap()
                .operation
        else {
            panic!("expected Epic membership edit")
        };
        assert_eq!(ticket.backend_key, "5");
        assert_eq!(epic.backend_key, "9");
    }

    /// A removal clears a 0..1 slot, so it must stay deliverable even when the
    /// Epic it left has no Backend identity to address — the case a Promotion
    /// that has not yet received its receipt would otherwise stop.
    #[test]
    fn resolve_operation_removes_membership_without_addressing_the_epic() {
        let conn = open_seeded();
        backend_ticket(&conn, "ticket", "gh-5", "5", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
                display: "tk-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "remove_ticket_from_epic",
                item_id: "ticket",
                payload_json: r#"{"epic_id":"epic"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        let BackendOperation::Edit(BackendEdit::RemoveTicketFromEpic { ticket }) =
            resolve_backend_operation(&conn, rows.into_iter().next().unwrap())
                .unwrap()
                .operation
        else {
            panic!("expected Epic membership removal")
        };
        assert_eq!(ticket.backend_key, "5");
    }

    #[test]
    fn resolve_operation_rejects_a_ticket_as_the_membership_epic() {
        let conn = open_seeded();
        backend_ticket(&conn, "ticket", "gh-5", "5", 1);
        backend_ticket(&conn, "not-epic", "gh-9", "9", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_ticket_to_epic",
                item_id: "ticket",
                payload_json: r#"{"epic_id":"not-epic"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        assert!(matches!(
            resolve_backend_operation(&conn, rows.into_iter().next().unwrap()),
            Err(LoadApplicableError::CounterpartClassMismatch {
                mutation_type: MutationType::AddTicketToEpic,
                ref item_id,
                expected: ItemClass::Epic,
                actual: ItemClass::Ticket,
            }) if item_id == "not-epic"
        ));
    }

    #[test]
    fn resolve_operation_rejects_a_ticket_update_for_an_epic() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
                display: "gh-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let row = ApplicableMutationRow {
            sequence: 1,
            mutation_type: MutationType::UpdateTicket,
            item_id: "epic".into(),
            item_class: ItemClass::Epic,
            payload: MutationPayload::UpdateTitleBody(TitleBody {
                title: "Renamed".into(),
                body: String::new(),
            }),
        };

        assert!(matches!(
            resolve_backend_operation(&conn, row),
            Err(LoadApplicableError::OperationShapeMismatch {
                mutation_type: MutationType::UpdateTicket,
                item_class: ItemClass::Epic,
            })
        ));
    }

    #[test]
    fn resolve_operation_rejects_a_local_blocking_item() {
        let conn = open_seeded();
        backend_ticket(&conn, "blocked", "gh-5", "5", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocking",
                display: "tk-9",
                title: "Local blocker",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_dependency",
                item_id: "blocked",
                payload_json: r#"{"blocking_id":"blocking"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let rows = load_applicable_mutations(&conn).unwrap();
        assert!(matches!(
            resolve_backend_operation(&conn, rows.into_iter().next().unwrap()),
            Err(LoadApplicableError::MissingBackendIdentity {
                mutation_type: MutationType::AddDependency,
                ref item_id,
            }) if item_id == "blocking"
        ));
    }

    #[test]
    fn resolve_operation_reads_an_identity_a_receipt_just_assigned() {
        // Identity is resolved per Mutation precisely so a Promotion receipt
        // applied earlier in the same run is visible to the Mutations behind it.
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let rows = load_applicable_mutations(&conn).unwrap();

        let tx = crate::store::write_transaction(&mut conn).unwrap();
        crate::store::promotion::apply_receipt(
            &tx,
            "t1",
            "github",
            &BackendItemIdentity {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            },
            "2026-05-19T00:00:00Z",
        )
        .unwrap();
        tx.commit().unwrap();

        let resolved = resolve_backend_operation(&conn, rows.into_iter().next().unwrap()).unwrap();
        let BackendOperation::Edit(edit) = resolved.operation else {
            panic!("ordinary Mutation must resolve as an edit")
        };
        let BackendEdit::SetItemStatus { item, .. } = edit else {
            panic!("expected status edit")
        };
        assert_eq!(item.backend_key, "42");
    }

    #[test]
    fn load_applicable_rejects_payload_variant_missing() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_external_blocker",
                item_id: "t1",
                payload_json: "{}",
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match load_applicable_mutations(&conn).unwrap_err() {
            LoadApplicableError::PayloadVariantMissing(MutationType::AddExternalBlocker) => {}
            other => panic!("expected PayloadVariantMissing, got {other:?}"),
        }
    }

    #[test]
    fn load_applicable_decodes_promotion_payload() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        for (seq, mutation_type) in [(1, "promote_ticket"), (2, "promote_epic")] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: seq,
                    mutation_type,
                    item_id: "t1",
                    payload_json: r#"{"title":"T","body":"B","backend_kind":"github"}"#,
                    state: "pending",
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }

        let views = load_applicable_mutations(&conn).unwrap();
        assert_eq!(views.len(), 2);
        for view in &views {
            match &view.payload {
                MutationPayload::Promotion(p) => {
                    assert_eq!(p.title, "T");
                    assert_eq!(p.body, "B");
                    assert_eq!(p.backend_kind, "github");
                }
                other => panic!("expected Promotion, got {other:?}"),
            }
        }
    }

    // ---- outcome persistence -------------------------------------------

    fn seed_remote(conn: &Connection) {
        insert_fixture_remote(conn, FixtureRemote::default()).unwrap();
    }

    fn seed_pending(conn: &Connection, sequence: i64) {
        backend_ticket(conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"New","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn edit_outcome_pending_success_applies_and_advances_cursor() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending(&conn, 5);

        persist_edit_outcome(
            &mut conn,
            5,
            &BackendEditOutcome::Acknowledged,
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, Option<String>) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 5",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applied");
        assert_eq!(failure, None);

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 5);
    }

    #[test]
    fn edit_outcome_pending_failure_records_detail() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending(&conn, 1);

        persist_edit_outcome(
            &mut conn,
            1,
            &BackendEditOutcome::rejected("HTTP 422: title required"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "failed");
        assert!(failure.contains("title required"));
    }

    #[test]
    fn edit_outcome_failed_success_clears_failure_and_applies() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        persist_edit_outcome(
            &mut conn,
            3,
            &BackendEditOutcome::Acknowledged,
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, Option<String>) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "applied");
        assert_eq!(failure, None);
    }

    #[test]
    fn edit_outcome_failed_failure_keeps_state_refreshes_detail() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"old reason"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        persist_edit_outcome(
            &mut conn,
            2,
            &BackendEditOutcome::rejected("new reason"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "failed");
        assert!(failure.contains("new reason"));
        assert!(!failure.contains("old reason"));
    }

    #[test]
    fn edit_outcome_missing_row_returns_not_found() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        match persist_edit_outcome(
            &mut conn,
            999,
            &BackendEditOutcome::Acknowledged,
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err()
        {
            PersistMutationOutcomeError::MutationNotFound(999) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }

    #[test]
    fn edit_outcome_terminal_state_returns_not_applicable() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match persist_edit_outcome(
            &mut conn,
            7,
            &BackendEditOutcome::Acknowledged,
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err()
        {
            PersistMutationOutcomeError::MutationNotApplicable(7) => {}
            other => panic!("expected MutationNotApplicable, got {other:?}"),
        }
    }

    /// Seed a Local Ticket with a pending `promote_ticket` Mutation — the
    /// Pending Promotion shape a receipt resolves (ADR-0036).
    fn seed_pending_promotion(conn: &Connection, sequence: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "pending",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn create_outcome_identity_lands_with_the_state_transition() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();

        persist_create_outcome(
            &mut conn,
            4,
            &BackendCreateOutcome::Created(BackendItemIdentity {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let state: String = conn
            .query_row("select state from mutations where sequence = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applied");

        let (display, origin, kind, key): (String, String, String, String) = conn
            .query_row(
                "select display_value, origin, backend_kind, backend_key from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-42");
        assert_eq!(origin, "backend", "applied Promotion leaves no Local Item");
        assert_eq!(kind, "github", "backend kind comes from the payload");
        assert_eq!(key, "42");

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 4);
    }

    #[test]
    fn failed_creation_retried_as_created_clears_failure_and_applies_identity() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        conn.execute(
            "update mutations set state = 'failed', failure_json = \
             '{\"detail\":\"prior failure\"}' where sequence = 4",
            [],
        )
        .unwrap();
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();

        persist_create_outcome(
            &mut conn,
            4,
            &BackendCreateOutcome::Created(BackendItemIdentity {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure, display, origin, kind, key): (
            String,
            Option<String>,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "select m.state, m.failure_json, i.display_value, i.origin, \
                        i.backend_kind, i.backend_key \
                   from mutations m join items i on i.id = m.item_id \
                  where m.sequence = 4",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!((state.as_str(), failure), ("applied", None));
        assert_eq!(
            (
                display.as_str(),
                origin.as_str(),
                kind.as_str(),
                key.as_str()
            ),
            ("gh-42", "backend", "github", "42")
        );
        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 4);
    }

    #[test]
    fn create_rejection_records_failure_without_converting_the_item() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();

        persist_create_outcome(
            &mut conn,
            4,
            &BackendCreateOutcome::rejected("title is required"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure, origin): (String, String, String) = conn
            .query_row(
                "select m.state, m.failure_json, i.origin \
                   from mutations m join items i on i.id = m.item_id \
                  where m.sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "failed");
        assert!(failure.contains("title is required"));
        assert_eq!(origin, "local");
    }

    #[test]
    fn indeterminate_creation_records_failure_without_inventing_an_identity() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();

        persist_create_outcome(
            &mut conn,
            4,
            &BackendCreateOutcome::indeterminate("gh exited after sending the request"),
            "2026-05-19T00:00:00Z",
        )
        .unwrap();

        let (state, failure, key): (String, String, Option<String>) = conn
            .query_row(
                "select m.state, m.failure_json, i.backend_key \
                   from mutations m join items i on i.id = m.item_id \
                  where m.sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "applying");
        assert!(failure.contains("after sending"));
        assert_eq!(key, None);
    }

    #[test]
    fn create_outcome_rolls_back_when_the_identity_cannot_be_persisted() {
        // A Display ID another Item already claims rolls back the receipt while
        // preserving `applying`; automatic replay must not risk a second object.
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "squatter",
                display: "gh-42",
                title: "Already claimed",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();

        let err = persist_create_outcome(
            &mut conn,
            4,
            &BackendCreateOutcome::Created(BackendItemIdentity {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }),
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err();
        assert!(
            matches!(err, PersistMutationOutcomeError::Storage(_)),
            "the collision must surface, not be swallowed; got {err:?}"
        );

        let state: String = conn
            .query_row("select state from mutations where sequence = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_ne!(
            state, "applied",
            "a Mutation with no receipt is not applied"
        );
        assert_eq!(state, "applying");

        let (display, origin): (String, String) = conn
            .query_row(
                "select display_value, origin from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(display, "tk-1");
        assert_eq!(origin, "local");

        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 0, "the cursor rolled back with the receipt");
    }

    #[test]
    fn create_receipt_refuses_a_promotion_whose_item_became_backend_bound() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        begin_create(&mut conn, 4, "2026-05-19T00:00:00Z").unwrap();
        conn.execute(
            "update items set origin = 'backend', backend_kind = 'github', \
                    backend_key = 'existing' where id = 't1'",
            [],
        )
        .unwrap();

        assert!(matches!(
            persist_create_outcome(
                &mut conn,
                4,
                &BackendCreateOutcome::Created(BackendItemIdentity {
                    backend_key: "42".into(),
                    display_id: "gh-42".into(),
                }),
                "2026-05-19T00:00:00Z",
            ),
            Err(PersistMutationOutcomeError::TargetNotLocal {
                sequence: 4,
                ref item_id,
            }) if item_id == "t1"
        ));
        let state: String = conn
            .query_row("select state from mutations where sequence = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applying");
    }

    #[test]
    fn edit_outcome_refuses_a_promotion_mutation() {
        // Writing `applied` here would strand a Local Item behind an applied
        // Promotion — the state the typed Receipt exists to prevent.
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);

        match persist_edit_outcome(
            &mut conn,
            4,
            &BackendEditOutcome::Acknowledged,
            "2026-05-19T00:00:00Z",
        )
        .unwrap_err()
        {
            PersistMutationOutcomeError::OperationShapeMismatch {
                sequence: 4,
                mutation_type: MutationType::PromoteTicket,
            } => {}
            other => panic!("expected OperationShapeMismatch, got {other:?}"),
        }

        let (state, origin): (String, String) = conn
            .query_row(
                "select m.state, i.origin from mutations m join items i on i.id = m.item_id \
                  where m.sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "pending");
        assert_eq!(origin, "local");
    }

    #[test]
    fn begin_create_refuses_a_plain_mutation() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending(&conn, 5);

        match begin_create(&mut conn, 5, "2026-05-19T00:00:00Z").unwrap_err() {
            PersistMutationOutcomeError::OperationShapeMismatch {
                sequence: 5,
                mutation_type: MutationType::UpdateTicket,
            } => {}
            other => panic!("expected OperationShapeMismatch, got {other:?}"),
        }

        let state: String = conn
            .query_row("select state from mutations where sequence = 5", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending");
    }

    // ---- mark_mutation_skipped ------------------------------------------

    #[test]
    fn mark_skipped_transitions_failed_to_skipped() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "skipped");
        assert!(failure.contains("rejected"), "audit trail preserved");
    }

    #[test]
    fn mark_skipped_refuses_non_failed() {
        let mut conn = open_seeded();
        seed_pending(&conn, 1);
        let workflow = RemoteWorkflowGuard::for_test();
        match mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::MutationNotFailed(1) => {}
            other => panic!("expected MutationNotFailed, got {other:?}"),
        }
    }

    #[test]
    fn mark_skipped_missing_row_returns_not_found() {
        let mut conn = open_seeded();
        let workflow = RemoteWorkflowGuard::for_test();
        match mark_mutation_skipped(&mut conn, &workflow, 42, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::MutationNotFound(42) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }

    #[test]
    fn mark_skipped_reports_promotion_before_state() {
        // Both Promotion Mutation Types gate the same way: skipping either
        // would strand whatever the invocation queued behind it with no
        // backend identity to resolve against (ADR-0036 Pending Promotion).
        for (mutation_type, item_class) in [("promote_ticket", "ticket"), ("promote_epic", "epic")]
        {
            for prior_state in ["failed", "applying"] {
                let mut conn = open_seeded();
                insert_fixture_item(
                    &conn,
                    FixtureItem {
                        id: "t1",
                        display: "tk-1",
                        item_class,
                        ticket_kind: (item_class == "ticket").then_some("task"),
                        selection_state: (item_class == "ticket").then_some("accepted"),
                        priority: (item_class == "ticket").then_some("P2"),
                        title: "Local work",
                        created_seq: 1,
                        ..FixtureItem::default()
                    },
                )
                .unwrap();
                insert_fixture_mutation(
                    &conn,
                    FixtureMutation {
                        sequence: 1,
                        mutation_type,
                        item_id: "t1",
                        item_class,
                        payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                        state: prior_state,
                        failure_json: Some(r#"{"detail":"boom"}"#),
                        ..FixtureMutation::default()
                    },
                )
                .unwrap();

                let workflow = RemoteWorkflowGuard::for_test();
                match mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z")
                    .unwrap_err()
                {
                    MarkSkippedError::CannotSkipPromotion(1) => {}
                    other => panic!(
                        "expected CannotSkipPromotion for {prior_state} {mutation_type}, got {other:?}"
                    ),
                }

                let stored_state: String = conn
                    .query_row("select state from mutations where sequence = 1", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(stored_state, prior_state);
            }
        }
    }

    // ---- get_remote / count ---------------------------------------------

    #[test]
    fn get_remote_returns_none_when_unconfigured() {
        let conn = open_seeded();
        assert_eq!(get_remote(&conn).unwrap(), None);
    }

    #[test]
    fn get_remote_returns_configured_row_with_cursor() {
        let conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: r#"{"repo":"o/r"}"#,
                last_applied_sequence: 9,
                ..FixtureRemote::default()
            },
        )
        .unwrap();

        let row = get_remote(&conn).unwrap().unwrap();
        assert_eq!(row.backend_kind, "github");
        assert_eq!(row.config_json, r#"{"repo":"o/r"}"#);
        assert_eq!(row.last_applied_sequence, 9);
    }

    #[test]
    fn pending_or_failed_count_counts_only_in_flight() {
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        for (seq, state) in [
            (1, "pending"),
            (2, "failed"),
            (3, "applied"),
            (4, "skipped"),
        ] {
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: seq,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"X","body":""}"#,
                    state,
                    failure_json: if state == "failed" {
                        Some(r#"{"detail":"x"}"#)
                    } else {
                        None
                    },
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
        }
        assert_eq!(pending_or_failed_mutation_count(&conn).unwrap(), 2);
    }

    // ---- set_remote / clear_remote --------------------------------------

    #[test]
    fn set_remote_creates_row_then_no_ops() {
        let mut conn = open_seeded();
        let outcome =
            set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();
        assert_eq!(outcome, SetRemoteOutcome::Created);

        let row = get_remote(&conn).unwrap().unwrap();
        assert_eq!(row.backend_kind, "github");
        assert_eq!(row.config_json, "{}");
        assert_eq!(row.last_applied_sequence, 0);

        // A second set is an idempotent no-op (ADR-0033): no replace, one row.
        let again =
            set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-18T00:00:00Z").unwrap();
        assert_eq!(again, SetRemoteOutcome::Unchanged);
        let count: i64 = conn
            .query_row("select count(*) from remotes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn set_remote_restores_the_kind_retained_by_backend_items() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        clear_remote(&mut conn).unwrap();

        let outcome =
            set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-18T00:00:00Z").unwrap();

        assert_eq!(outcome, SetRemoteOutcome::Created);
        assert_eq!(get_remote(&conn).unwrap().unwrap().backend_kind, "github");
    }

    #[test]
    fn set_remote_rejects_a_kind_incompatible_with_retained_items() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        clear_remote(&mut conn).unwrap();

        match set_remote(&mut conn, BackendKind::Jira, "{}", "2026-06-18T00:00:00Z").unwrap_err() {
            SetRemoteError::BackendKindConflict {
                requested: BackendKind::Jira,
                retained: BackendKind::Github,
            } => {}
            other => panic!("expected BackendKindConflict, got {other:?}"),
        }
        assert_eq!(get_remote(&conn).unwrap(), None);
    }

    #[test]
    fn set_remote_rejects_mixed_retained_backend_kinds() {
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "tk-2",
                title: "Local",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t2",
                payload_json: r#"{"title":"Local","body":"","backend_kind":"jira"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-18T00:00:00Z").unwrap_err()
        {
            SetRemoteError::BackendCohort(BackendCohortError::MultipleBackendKinds) => {}
            other => panic!("expected mixed Backend cohort, got {other:?}"),
        }
        assert_eq!(get_remote(&conn).unwrap(), None);
    }

    #[test]
    fn set_remote_reports_unknown_retained_backend_kind() {
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local","body":"","backend_kind":"gitlab"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-18T00:00:00Z").unwrap_err()
        {
            SetRemoteError::BackendCohort(BackendCohortError::UnknownBackendKind(kind)) => {
                assert_eq!(kind, "gitlab");
            }
            other => panic!("expected unknown Backend kind, got {other:?}"),
        }
        assert_eq!(get_remote(&conn).unwrap(), None);
    }

    #[test]
    fn set_remote_rejects_replacing_an_existing_remote_kind() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();

        match set_remote(&mut conn, BackendKind::Jira, "{}", "2026-06-18T00:00:00Z").unwrap_err() {
            SetRemoteError::RemoteKindConflict {
                requested: BackendKind::Jira,
                existing: BackendKind::Github,
            } => {}
            other => panic!("expected RemoteKindConflict, got {other:?}"),
        }
        assert_eq!(get_remote(&conn).unwrap().unwrap().backend_kind, "github");
    }

    #[test]
    fn clear_remote_removes_remote_and_cursor_when_clean() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();

        clear_remote(&mut conn).unwrap();

        assert_eq!(get_remote(&conn).unwrap(), None);
        let cursors: i64 = conn
            .query_row("select count(*) from sync_cursors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            cursors, 0,
            "the child sync_cursors row is deleted before the restrict FK"
        );
    }

    #[test]
    fn clear_remote_refuses_when_no_remote() {
        let mut conn = open_seeded();
        match clear_remote(&mut conn).unwrap_err() {
            ClearRemoteError::NotConfigured => {}
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn clear_remote_refuses_when_pending_or_failed_would_orphan() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"x"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match clear_remote(&mut conn).unwrap_err() {
            ClearRemoteError::WouldOrphan(1) => {}
            other => panic!("expected WouldOrphan(1), got {other:?}"),
        }
        // The Remote survives a refused clear.
        assert!(get_remote(&conn).unwrap().is_some());
    }

    #[test]
    fn clear_remote_refuses_an_applying_creation() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        assert!(matches!(
            clear_remote(&mut conn),
            Err(ClearRemoteError::ApplyingMutation(3))
        ));
        assert!(get_remote(&conn).unwrap().is_some());
    }

    // ---- log read -------------------------------------------------------

    fn seed_log_fixture(conn: &Connection) {
        backend_ticket(conn, "t1", "gh-1", "1", 1);
        backend_ticket(conn, "t2", "gh-2", "2", 2);
        backend_ticket(conn, "t3", "gh-3", "3", 3);
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t4",
                display: "tk-4",
                title: "Local work",
                created_seq: 4,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"A","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "promote_ticket",
                item_id: "t4",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                failure_json: Some(r#"{"detail":"unknown effect"}"#),
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "set_item_status",
                item_id: "t2",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"HTTP 422: rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "update_ticket",
                item_id: "t3",
                payload_json: r#"{"title":"C","body":""}"#,
                state: "skipped",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn list_default_returns_nonterminal_and_skipped_rows_with_failure_detail() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        let rows = list_mutation_log(&conn, LogListFilter::Default).unwrap();
        let seqs: Vec<i64> = rows.iter().map(|r| r.sequence).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
        assert_eq!(rows[0].failure_detail, None);
        assert_eq!(
            rows[1].failure_detail.as_deref(),
            Some("HTTP 422: rejected")
        );
        assert_eq!(rows[1].target_display_id, "gh-2");
        assert_eq!(rows[3].state, MutationState::Applying);
        assert_eq!(rows[3].failure_detail.as_deref(), Some("unknown effect"));
    }

    #[test]
    fn list_filters_by_state() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Pending)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Failed)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_mutation_log(&conn, LogListFilter::Skipped)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn show_returns_detail_with_decoded_failure() {
        let conn = open_seeded();
        seed_log_fixture(&conn);

        let detail = show_mutation_log(&conn, 2).unwrap();
        assert_eq!(detail.sequence, 2);
        assert_eq!(detail.state, MutationState::Failed);
        assert_eq!(detail.mutation_type, MutationType::SetItemStatus);
        assert_eq!(detail.target_display_id, "gh-2");
        assert_eq!(detail.item_class, ItemClass::Ticket);
        assert_eq!(detail.payload_json, r#"{"status":"done"}"#);
        assert_eq!(detail.failure_detail.as_deref(), Some("HTTP 422: rejected"));
    }

    #[test]
    fn show_missing_returns_not_found() {
        let conn = open_seeded();
        match show_mutation_log(&conn, 999).unwrap_err() {
            LogError::MutationNotFound(999) => {}
            other => panic!("expected MutationNotFound, got {other:?}"),
        }
    }

    #[test]
    fn begin_create_refuses_a_promotion_whose_item_is_already_backend_bound() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        seed_pending_promotion(&conn, 4);
        conn.execute(
            "update items set origin = 'backend', backend_kind = 'github', backend_key = '42' where id = 't1'",
            [],
        )
        .unwrap();
        assert!(matches!(
            begin_create(&mut conn, 4, "2026-05-19T00:00:00Z"),
            Err(PersistMutationOutcomeError::TargetNotLocal {
                sequence: 4,
                ref item_id,
            }) if item_id == "t1"
        ));
    }

    #[test]
    fn sync_cursor_never_regresses() {
        let conn = open_seeded();
        seed_remote(&conn);
        mutations::mark_applied(&conn, 9, "2026-05-19T00:00:00Z").unwrap();
        mutations::mark_applied(&conn, 4, "2026-05-20T00:00:00Z").unwrap();
        let cursor: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 9);
    }
}
