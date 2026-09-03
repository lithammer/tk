//! Repository Store operations for Remote configuration, retained Backend
//! cohort validation, canonical Adopt insertion, Re-Adopt, Pull refresh, and
//! Mutation Log replay and inspection.
//!
//! Every operation here is SQL on the `items` / `mutations` / `item_ids` /
//! `remotes` / `sync_cursors` tables, so it lives under [`crate::store`]. The
//! Adopt, Promote, Remote, and Sync flows compose these transactions with
//! the backend-blind [`crate::remote::adapter::Adapter`] boundary.
//!
//! Write helpers open their own transaction and take `&mut Connection`; read
//! helpers take `&Connection`.

use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemAddress, BackendItemRefresh,
    BackendOperation,
};
use crate::domain::backend_outcome::{
    BackendCreateOutcome, BackendEditOutcome, Failure, FailureClass,
};
use crate::domain::binding_display_provenance::BindingDisplayProvenance;
use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, LifecycleChange, MutationPayload, Promotion, TitleBody,
};
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::MutationType;
use crate::domain::origin::Origin;
use crate::domain::priority::Priority;
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::domain::relationship_plan::{self, RelationshipFinding};
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::domain::work_state::WorkState;
use crate::store::mutations;
use crate::store::promotion::ReadGraphError;
use crate::store::repository::create::generate_internal_id;
use crate::store::repository::{self, RemoteWorkflowGuard};
use crate::store::sequences::{self, Counter, SequenceError};

// ──────────────────────────────────────────────────────────────────────────
// Canonical Adopt insertion and Backend cohort errors
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
    #[error(transparent)]
    Append(#[from] mutations::AppendError),
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
    /// The Item owning the Former Backend Identity already holds a different
    /// active Binding. An Item has at most one (ADR-0047).
    #[error("{backend_display_id} belongs to the Item now bound as {bound_display_id}")]
    ReadoptBoundElsewhere {
        backend_display_id: String,
        bound_display_id: String,
    },
    /// The Item owning the Former Backend Identity carries durable Promotion
    /// intent. Rebinding it would leave that Promotion's receipt with no Local
    /// Item to bind (ADR-0036).
    #[error("{backend_display_id} belongs to {local_display_id}, which has a Pending Promotion")]
    ReadoptPendingPromotion {
        backend_display_id: String,
        local_display_id: String,
    },
    /// Every ordered relationship finding that stops Re-Adopt before it writes
    /// the restored Binding (ADR-0035, ADR-0047).
    #[error("relationship preflight refused Re-Adopt of {backend_display_id}")]
    ReadoptRelationships {
        item_id: String,
        backend_display_id: String,
        backend_kind: BackendKind,
        findings: Vec<RelationshipFinding>,
    },
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
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

/// Result of canonical Backend Ticket intake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptOutcome {
    Inserted(BackendItemRow),
    AlreadyExists(BackendItemRow),
    /// The canonical identity was a Former Backend Identity, so intake
    /// restored its original Item instead of creating a second one
    /// (ADR-0047).
    Readopted(ReadoptReport),
}

/// One Item restored to the canonical Backend identity its Former Backend
/// Identity history already reserved to it (ADR-0047).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadoptReport {
    /// Backend Display ID made current by Re-Adopt.
    pub backend_display_id: String,
    /// Local Display ID demoted to an Alias, which a later Detach restores.
    pub local_display_id: String,
    /// Canonical identity restored, spelled as Former Backend Identity
    /// history holds it.
    pub backend_key: String,
    /// Title imported from the Backend snapshot.
    pub title: String,
    /// Ticket Kind imported for a Ticket; Epics retain no Ticket-only Kind.
    pub ticket_kind: Option<TicketKind>,
    /// Item Status derived from the imported Lifecycle and the Work State
    /// that survived it (ADR-0043).
    pub status: ItemStatus,
    /// Fresh relationship intent appended by the Re-Adopt transaction, in
    /// Mutation Sequence order (ADR-0047).
    pub queued_relationships: Vec<QueuedRelationshipMutation>,
}

impl ReadoptReport {
    /// Derive the preserved Item Class from the Ticket-only Kind enforced by
    /// the Repository Store schema.
    #[must_use]
    pub fn item_class(&self) -> ItemClass {
        match self.ticket_kind {
            Some(_) => ItemClass::Ticket,
            None => ItemClass::Epic,
        }
    }
}

/// One relationship Mutation queued while restoring a Former Backend
/// Identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRelationshipMutation {
    /// Fresh Mutation Sequence allocated by the Re-Adopt transaction.
    pub sequence: i64,
    /// Relationship operation the Mutation Log will apply.
    pub mutation_type: MutationType,
    /// Current Display ID of the Item the Mutation targets.
    pub target_display_id: String,
}

/// One canonical Backend identity an Item detached from, as
/// `former_backend_identities` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FormerIdentity {
    /// Stable internal Item ID that owns the identity for its lifetime.
    item_id: String,
    /// Canonical key exactly as history stores it. Re-Adopt restores this
    /// spelling rather than the Adapter's, so one Backend object never appears
    /// as both an active and a former identity under two spellings.
    backend_key: String,
    /// Backend Display ID this identity carried while it was current.
    backend_display_id: String,
}

/// Take canonical Adapter intake data into the Repository Store: ordinary
/// intake inserts a Backend Ticket, while Re-Adopt restores the Ticket or Epic
/// that already owns the identity (ADR-0047).
///
/// The Repository Store is the serialization point for canonical identity, and
/// that ownership spans active and former history: duplicate
/// `(BackendKind, backend_key)` input returns the stored row without changing
/// it, and a Former Backend Identity restores its original Item rather than
/// creating a second representation of one Backend object. The transaction
/// rechecks the Remote before writing, closing the Adapter-read to Store-write
/// configuration race.
pub fn adopt_backend_ticket(
    conn: &mut Connection,
    expected_kind: BackendKind,
    rng: &mut dyn rand::Rng,
    adopted: &AdoptedItem,
    capabilities: PromotionCapabilities,
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
    // Active identity first: an Item restored by an earlier Re-Adopt keeps the
    // matching row in history, so only a miss above makes this a former one.
    if let Some(former) =
        find_former_backend_identity(&tx, expected_kind, &adopted.backend_key, legacy_backend_key)?
    {
        let report = readopt_backend_item(&tx, expected_kind, &former, adopted, capabilities, now)?;
        tx.commit()?;
        return Ok(AdoptOutcome::Readopted(report));
    }
    let id = generate_internal_id(rng);
    let created_seq = sequences::next(&tx, Counter::ItemCreated)?;
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

    // Dropping `tx` on an early return rolls back the orphaned `items` insert.
    claim_display_id(&tx, &adopted.display_id, &id, now)?;
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

/// Derive the Backend facets an exact Re-Adopt needs after Adapter
/// canonicalization. Ordinary Adopt returns `None` because it restores no
/// relationship intent.
pub fn readopt_requirements(
    conn: &Connection,
    backend_kind: BackendKind,
    adopted: &AdoptedItem,
) -> Result<Option<PromotionRequirements>, AdoptStoreError> {
    let legacy_backend_key = legacy_adopt_backend_key(backend_kind, adopted);
    if find_adopted_ticket_by_identity(
        conn,
        backend_kind,
        &adopted.backend_key,
        legacy_backend_key,
    )?
    .is_some()
    {
        return Ok(None);
    }
    let Some(former) =
        find_former_backend_identity(conn, backend_kind, &adopted.backend_key, legacy_backend_key)?
    else {
        return Ok(None);
    };
    ensure_backend_cohort(conn, backend_kind)?;
    let graph = readopt_graph(conn, &former.item_id)?;
    let bound_ids = HashSet::from([former.item_id.as_str()]);
    Ok(Some(relationship_plan::requirements(
        &graph.items,
        &graph.dependencies,
        &bound_ids,
        backend_kind,
    )))
}

/// Rebind the Item that owns `former` to that canonical identity, inside the
/// caller's Adopt intake transaction (ADR-0047).
///
/// The Backend snapshot replaces title, body, and Lifecycle. It also replaces
/// Ticket Kind for a Ticket, while Item Class, the stable internal Item ID,
/// Local Fields, and relationships stay as the Item held them locally.
///
/// Detach's withdrawals remain terminal. Dependencies and Epic Membership
/// that become Backend intent append fresh ordered Mutations; an invalid
/// Dependency refuses before any Store write, and mixed-Origin membership
/// stays local. The caller commits the rebind and outbox together.
fn readopt_backend_item(
    conn: &Connection,
    backend_kind: BackendKind,
    former: &FormerIdentity,
    adopted: &AdoptedItem,
    capabilities: PromotionCapabilities,
    now: &str,
) -> Result<ReadoptReport, AdoptStoreError> {
    let (item_class, display_id, work_state, closing_reason): (
        ItemClass,
        String,
        WorkState,
        Option<String>,
    ) = conn.query_row(
        "select item_class, display_value, work_state, closing_reason \
           from items where id = ?1",
        params![&former.item_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    // One question decides both remaining refusals, so the match asks it once
    // and covers every answer: an Item holds at most one active Binding, and a
    // Pending Promotion is still owed a Backend identity of its own that this
    // rebind would leave no Local Item to bind (ADR-0036).
    match mutations::resolve_backend_binding(conn, &former.item_id)? {
        BackendBinding::Backend { .. } => {
            return Err(AdoptStoreError::ReadoptBoundElsewhere {
                backend_display_id: former.backend_display_id.clone(),
                bound_display_id: display_id,
            });
        }
        BackendBinding::PendingPromotion { .. } => {
            return Err(AdoptStoreError::ReadoptPendingPromotion {
                backend_display_id: former.backend_display_id.clone(),
                local_display_id: display_id,
            });
        }
        BackendBinding::Local => {}
    }
    let graph = readopt_graph(conn, &former.item_id)?;
    let bound_ids = HashSet::from([former.item_id.as_str()]);
    let relationship_drafts = relationship_plan::plan(
        &graph.items,
        &graph.dependencies,
        &bound_ids,
        capabilities,
        backend_kind,
    )
    .map_err(|findings| AdoptStoreError::ReadoptRelationships {
        item_id: former.item_id.clone(),
        backend_display_id: former.backend_display_id.clone(),
        backend_kind,
        findings,
    })?;

    // The imported Lifecycle decides the two local fields it can invalidate:
    // `done` retires Work State, and `open` cannot carry a Closing Reason the
    // column CHECK confines to `done` (ADR-0043, ADR-0047).
    let (work_state, closing_reason) = match adopted.status {
        Lifecycle::Open => (work_state, None),
        Lifecycle::Done => (WorkState::Idle, closing_reason),
    };

    // The Binding records the local Display ID it displaced, which is what a
    // later Detach restores. The two CHECK-constrained columns come from one
    // domain value so they cannot be written out of agreement.
    let provenance = BindingDisplayProvenance::Known(display_id.clone());
    let (stored_provenance, displaced_display_id) = provenance.stored_values();

    // Statement order is load-bearing for the same reason as
    // `promotion::apply_receipt`: `item_ids_one_display_per_item` is a
    // non-deferrable partial unique index, so the outgoing Display ID has to
    // become an Alias before the Backend Display ID is inserted.
    let ticket_kind = (item_class == ItemClass::Ticket).then_some(adopted.ticket_kind);
    conn.execute(
        "update item_ids set source = 'alias' where item_id = ?1 and source = 'display'",
        params![&former.item_id],
    )?;
    claim_display_id(conn, &former.backend_display_id, &former.item_id, now)?;
    // One statement, because it is also what authorizes the reopen: migration
    // 015 admits `done` -> `open` only while a single update moves a Local
    // Item onto a canonical identity its own history reserves (ADR-0006).
    conn.execute(
        "update items \
            set origin = 'backend', backend_kind = ?2, backend_key = ?3, display_value = ?4, \
                title = ?5, body = ?6, ticket_kind = ?7, status = ?8, work_state = ?9, \
                closing_reason = ?10, updated_at = ?11, \
                binding_display_provenance = ?12, binding_local_display_value = ?13 \
          where id = ?1",
        params![
            &former.item_id,
            backend_kind.text(),
            &former.backend_key,
            &former.backend_display_id,
            adopted.title,
            adopted.body,
            ticket_kind.map(TicketKind::text),
            adopted.status.text(),
            work_state.text(),
            closing_reason,
            now,
            stored_provenance,
            displaced_display_id,
        ],
    )?;

    let display_ids: HashMap<&str, &str> = graph
        .items
        .iter()
        .map(|item| (item.id.as_str(), item.display_id.as_str()))
        .collect();
    let mut queued_relationships = Vec::with_capacity(relationship_drafts.len());
    for draft in relationship_drafts {
        let sequence = mutations::append(
            conn,
            mutations::AppendRequest {
                mutation_type: draft.mutation_type,
                item_id: &draft.item_id,
                item_class: draft.item_class,
                payload: &draft.payload,
                promotion_operation_id: None,
                now_iso: now,
            },
        )?;
        let target_display_id = if draft.item_id == former.item_id {
            former.backend_display_id.clone()
        } else {
            display_ids
                .get(draft.item_id.as_str())
                .expect("relationship target belongs to the closed graph")
                .to_string()
        };
        queued_relationships.push(QueuedRelationshipMutation {
            sequence,
            mutation_type: draft.mutation_type,
            target_display_id,
        });
    }

    Ok(ReadoptReport {
        backend_display_id: former.backend_display_id.clone(),
        local_display_id: display_id,
        backend_key: former.backend_key.clone(),
        title: adopted.title.clone(),
        ticket_kind,
        status: ItemStatus::of(adopted.status, work_state),
        queued_relationships,
    })
}

/// Read only the relationship graph that Re-Adopt can change.
fn readopt_graph(
    conn: &Connection,
    item_id: &str,
) -> Result<crate::domain::promotion_graph::PromotionGraph, AdoptStoreError> {
    match crate::store::promotion::read_graph(conn, item_id, false) {
        Ok(mut graph) => {
            // Promotion's read also gathers edges that touch the target's
            // parent or children. Re-Adopt changes only its target, so those
            // wider edges are outside Re-Adopt's relationship check.
            graph
                .dependencies
                .retain(|edge| edge.blocked_id == item_id || edge.blocking_id == item_id);
            Ok(graph)
        }
        Err(ReadGraphError::Storage(error)) => Err(error.into()),
        Err(ReadGraphError::BackendBinding(error)) => Err(error.into()),
    }
}

/// Look up the Former Backend Identity that owns a canonical Backend key.
///
/// Shares Adopt's exact-key predicate, legacy spelling included: Detach copies
/// `items.backend_key` verbatim into history, so an Item adopted before the
/// Adapter canonicalized keys leaves a bare issue number behind. Missing it
/// would let ordinary Adopt insert a second Item for a Backend object tk
/// already owns.
///
/// Two rows can satisfy that predicate, because the ownership invariant
/// compares key strings and the two spellings are not equal: history may hold
/// a legacy number for one Item and the canonical URL for another. The
/// canonical spelling is the stronger evidence — a bare number is only
/// exact-matchable within one repository — so it wins, and the other Item
/// keeps its reservation rather than being displaced by scan order.
fn find_former_backend_identity(
    conn: &Connection,
    backend_kind: BackendKind,
    backend_key: &str,
    legacy_backend_key: Option<&str>,
) -> rusqlite::Result<Option<FormerIdentity>> {
    conn.query_row(
        "select item_id, backend_key, backend_display_value \
           from former_backend_identities \
          where backend_kind = ?1 \
            and (backend_key = ?2 or (?3 is not null and backend_key = ?3)) \
          order by backend_key = ?2 desc \
          limit 1",
        params![backend_kind.text(), backend_key, legacy_backend_key],
        |row| {
            Ok(FormerIdentity {
                item_id: row.get(0)?,
                backend_key: row.get(1)?,
                backend_display_id: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Claim `display_id` as the Item's current Display ID.
///
/// The resolver row itself belongs to [`repository`]; what this adds is the
/// classification. A unique/primary-key violation means another Item already
/// claims the value as a Display ID or Alias, which is
/// [`AdoptStoreError::DisplayIdCollision`] rather than a storage fault, and
/// both Adopt intake paths mint an Adapter-owned Display ID.
fn claim_display_id(
    conn: &Connection,
    display_id: &str,
    item_id: &str,
    now: &str,
) -> Result<(), AdoptStoreError> {
    match repository::insert_display_resolver(conn, display_id, item_id, now) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
        {
            Err(AdoptStoreError::DisplayIdCollision(display_id.to_owned()))
        }
        Err(other) => Err(other.into()),
    }
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
    ///
    /// Names the row, because the decode is batched: one undecodable payload
    /// fails the whole run, and the operator's next move is `tk sync log
    /// <sequence>` on the row that caused it.
    #[error("mutation {sequence} ({mutation_type}) has malformed payload_json: {source}")]
    PayloadJson {
        sequence: i64,
        mutation_type: MutationType,
        #[source]
        source: serde_json::Error,
    },
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
    #[error("mutation {mutation_type} requires Ticket Kind for Item {item_id}")]
    MissingTicketKind {
        mutation_type: MutationType,
        item_id: String,
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
        let payload = decode_mutation_payload(sequence, mutation_type, &payload_text)?;

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
        (MutationType::SetItemStatus, MutationPayload::Lifecycle(change)) => {
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
            let ticket_kind = conn
                .query_row(
                    "select ticket_kind from items where id = ?1",
                    params![item_id],
                    |row| row.get::<_, Option<TicketKind>>(0),
                )?
                .ok_or_else(|| LoadApplicableError::MissingTicketKind {
                    mutation_type,
                    item_id: item_id.clone(),
                })?;
            BackendOperation::Create(BackendCreate::Ticket {
                snapshot: TitleBody { title, body },
                ticket_kind,
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
    sequence: i64,
    mutation_type: MutationType,
    payload_text: &str,
) -> Result<MutationPayload, LoadApplicableError> {
    use MutationType as Mt;

    // Built here rather than through a `From` impl: the row identity is what
    // makes the failure actionable, and only this scope holds it.
    let malformed = |source: serde_json::Error| LoadApplicableError::PayloadJson {
        sequence,
        mutation_type,
        source,
    };
    Ok(match mutation_type {
        Mt::UpdateTicket | Mt::UpdateEpic => MutationPayload::UpdateTitleBody(
            serde_json::from_str::<TitleBody>(payload_text).map_err(malformed)?,
        ),
        Mt::AddTicketToEpic | Mt::RemoveTicketFromEpic => MutationPayload::EpicRef(
            serde_json::from_str::<EpicRef>(payload_text).map_err(malformed)?,
        ),
        Mt::SetItemStatus => MutationPayload::Lifecycle(
            serde_json::from_str::<LifecycleChange>(payload_text).map_err(malformed)?,
        ),
        Mt::AddDependency | Mt::RemoveDependency => MutationPayload::DependencyRef(
            serde_json::from_str::<DependencyRef>(payload_text).map_err(malformed)?,
        ),
        Mt::PromoteTicket | Mt::PromoteEpic => MutationPayload::Promotion(
            serde_json::from_str::<Promotion>(payload_text).map_err(malformed)?,
        ),
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
    /// [`LoadApplicableError::PayloadJson`] names on the load side, and named
    /// the same way: every sibling variant here carries its sequence, and the
    /// operator's next move is `tk sync log <sequence>`.
    #[error("mutation {sequence} has malformed payload_json: {source}")]
    PayloadJson {
        sequence: i64,
        #[source]
        source: serde_json::Error,
    },
    /// The Mutation state edge this outcome implies is not in the transition
    /// table. Every outcome path narrows the row to an applicable state before
    /// transitioning, so this names a Store-layer contract break.
    #[error(transparent)]
    Transition(#[from] mutations::IllegalTransition),
}

impl From<mutations::TransitionError> for PersistMutationOutcomeError {
    fn from(error: mutations::TransitionError) -> Self {
        match error {
            mutations::TransitionError::Storage(error) => Self::Storage(error),
            mutations::TransitionError::Illegal(error) => Self::Transition(error),
        }
    }
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
    let (prior, mutation_type) = applicable_outcome_row(&tx, sequence)?;
    if mutation_type.is_promotion() {
        return Err(PersistMutationOutcomeError::OperationShapeMismatch {
            sequence,
            mutation_type,
        });
    }

    match outcome {
        BackendEditOutcome::Acknowledged => mutations::mark_applied(&tx, sequence, prior, now)?,
        BackendEditOutcome::Rejected(failure) => {
            persist_failed(&tx, sequence, prior, failure, now)?;
        }
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
    // The non-Local precondition belongs to receipt application, which owns
    // it typed. Checking it here as well would refuse a Rejected or
    // Indeterminate outcome before its failure evidence is durable, leaving
    // the Mutation `applying` with nothing recorded against it.
    match outcome {
        BackendCreateOutcome::Created(identity) => {
            let payload: Promotion = serde_json::from_str(&payload_json)
                .map_err(|source| PersistMutationOutcomeError::PayloadJson { sequence, source })?;
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
            mutations::mark_applied(&tx, sequence, prior, now)?;
        }
        BackendCreateOutcome::Rejected(failure) => {
            persist_failed(&tx, sequence, prior, failure, now)?;
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
    mutations::transition(
        &tx,
        mutations::TransitionRequest {
            sequence,
            from: state,
            to: MutationState::Applying,
            failure: None,
            now,
        },
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
    prior: MutationState,
    failure: &Failure,
    now: &str,
) -> Result<(), mutations::TransitionError> {
    mutations::transition(
        conn,
        mutations::TransitionRequest {
            sequence,
            from: prior,
            to: MutationState::Failed,
            failure: Some(failure),
            now,
        },
    )
}

/// Record why a creation's effect stayed unknown without resolving the doubt:
/// the row keeps the `applying` barrier that only reconcile or retry lifts.
fn persist_applying_failure(
    conn: &Connection,
    sequence: i64,
    failure: &Failure,
    now: &str,
) -> Result<(), mutations::TransitionError> {
    mutations::transition(
        conn,
        mutations::TransitionRequest {
            sequence,
            from: MutationState::Applying,
            to: MutationState::Applying,
            failure: Some(failure),
            now,
        },
    )
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

/// Local consequence of skipping one failed Mutation, returned by
/// [`mark_mutation_skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipOutcome {
    /// Every Mutation Type but the one exception below: skipping changes
    /// only the Mutation's own state, to `skipped`. The Item is untouched.
    Bypassed,
    /// The skipped Mutation was a failed `set_item_status` targeting `done`.
    /// The Item was restored to `open` in the same transaction that moved
    /// the Mutation to `skipped` (ADR-0046). Carries the Item's Display ID
    /// so the command layer can name what it reopened.
    RelinquishedClose { display_id: String },
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
    /// Withdrawing a Promotion is operation-wide and spans those Mutations, so
    /// it is Promotion Cancellation's job, not Sync Skip's (ADR-0038). The
    /// refusal names the command that does it.
    #[error("mutation {0} is a Promotion and cannot be skipped")]
    CannotSkipPromotion(i64),
    /// The ADR-0046 reopen matched no row. `close_item` never appends a
    /// closing Mutation until its own transaction has already landed `done`,
    /// so a `failed` `set_item_status` row targeting `done` implies its Item
    /// is `done` too; this gate does not re-read the Item to confirm it. The
    /// `and status = 'done'` guard turns a broken instance of that invariant
    /// into a silent zero-row update rather than an impossible state, so it
    /// needs its own Repository Store invariant break instead of committing a
    /// Mutation the Store never actually relinquished.
    #[error("mutation {0}'s reopen matched no done Item")]
    ReopenMatchedNothing(i64),
    /// `items_no_escape_from_done` (migration 016) refused the reopen despite
    /// the failed closing Mutation this same transaction's gate confirmed
    /// exists — the one case its second exception (ADR-0046) exists to admit.
    #[error("mutation {0}'s reopen was refused by the done-terminal trigger")]
    ReopenRefusedByTrigger(i64),
    /// Sync Skip refuses any row that is not `failed`, which is the one legal
    /// `skipped` edge, so this names a Store-layer contract break.
    #[error(transparent)]
    Transition(#[from] mutations::IllegalTransition),
}

impl From<mutations::TransitionError> for MarkSkippedError {
    fn from(error: mutations::TransitionError) -> Self {
        match error {
            mutations::TransitionError::Storage(error) => Self::Storage(error),
            mutations::TransitionError::Illegal(error) => Self::Transition(error),
        }
    }
}

/// Restore an Item to `open` as Sync Skip relinquishes its failed close, and
/// report the Display ID the command layer names (ADR-0046).
///
/// Runs while that closing Mutation is still `failed`: the row is what
/// authorizes migration 016's trigger exception, so moving it to `skipped`
/// first would drop the authorization before the reopen ran. `work_state` is
/// written rather than assumed — ADR-0046 says the Item stays idle, and
/// ADR-0043's discipline is that the writer keeps the pair coherent instead
/// of inheriting it from `close_item` having cleared the other axis.
fn relinquish_close(
    tx: &Connection,
    item_id: &str,
    sequence: i64,
    now: &str,
) -> Result<String, MarkSkippedError> {
    let reopened = tx
        .execute(
            "update items \
                set status = ?2, work_state = ?3, closing_reason = null, updated_at = ?4 \
              where id = ?1 and status = ?5",
            params![
                item_id,
                Lifecycle::Open.text(),
                WorkState::Idle.text(),
                now,
                Lifecycle::Done.text(),
            ],
        )
        .map_err(|err| match err {
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER =>
            {
                MarkSkippedError::ReopenRefusedByTrigger(sequence)
            }
            other => other.into(),
        })?;
    // A failed closing Mutation implies its Item is `done` (see
    // `MarkSkippedError::ReopenMatchedNothing`), so zero rows is not a race.
    // It has to abort rather than let the caller's transition commit the very
    // divergence gh-53 exists to close while reporting success.
    if reopened == 0 {
        return Err(MarkSkippedError::ReopenMatchedNothing(sequence));
    }
    Ok(repository::current_display_id(tx, item_id)?)
}

/// Transition a `failed` Mutation Log entry into `skipped`, inside its own
/// transaction. Refuses a Mutation that is not `failed`, or whose Mutation
/// Type is `promote_ticket` / `promote_epic` ([`MarkSkippedError::CannotSkipPromotion`]).
/// The edge preserves `failure_json`, so `tk sync log` can still show why the
/// Mutation was bypassed.
///
/// Skipping a failed `set_item_status` Mutation whose target is `done`
/// additionally restores the Item to `open` in this same transaction
/// (ADR-0046): retaining local `done` after the operator relinquishes the
/// close would leave the Backend open forever, since `done` Items sit outside
/// Backend Pull.
pub fn mark_mutation_skipped(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    sequence: i64,
    now: &str,
) -> Result<SkipOutcome, MarkSkippedError> {
    let tx = crate::store::write_transaction(conn)?;

    // The gate reads the payload's target through SQL, not a Rust decode, so
    // it still resolves a `set_item_status` payload that no longer decodes as
    // `Lifecycle` (ADR-0043's amendment). That keeps Skip available for such a
    // row once it is `failed`. A `pending` one never reaches `failed`, because
    // `load_applicable_mutations` decodes every applicable row before Apply
    // can fail any of them, and the state gate below admits only `failed`; its
    // recovery is Promotion Cancellation or Detach, which withdraw a
    // `set_item_status` row without decoding its payload.
    let row: Option<(MutationState, MutationType, Option<String>, String)> = tx
        .query_row(
            "select state, mutation_type, json_extract(payload_json, '$.status'), item_id \
               from mutations where sequence = ?1",
            params![sequence],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (prior, mutation_type, target_status, item_id) =
        row.ok_or(MarkSkippedError::MutationNotFound(sequence))?;
    if mutation_type.is_promotion() {
        return Err(MarkSkippedError::CannotSkipPromotion(sequence));
    }
    if prior != MutationState::Failed {
        return Err(MarkSkippedError::MutationNotFailed(sequence));
    }

    // Exhaustive by name, following `MutationType::is_promotion`: a Mutation
    // kind added later has to answer what abandoning it does to local state
    // instead of silently reading as `Bypassed`.
    //
    // The target comes from the row named by `sequence`, not from an `exists`
    // over the Item the way migration 016's trigger reads it. The trigger's
    // clause names no particular row, so it stays open while any failed
    // closing Mutation sits on the Item; an Item can hold both a spared
    // non-closing `set_item_status` failure (migration 011) and a real failed
    // close, and skipping the former must neither reopen the Item nor report
    // a relinquishment that never happened.
    let outcome = match mutation_type {
        MutationType::SetItemStatus
            if target_status.as_deref() == Some(Lifecycle::Done.text()) =>
        {
            SkipOutcome::RelinquishedClose {
                display_id: relinquish_close(&tx, &item_id, sequence, now)?,
            }
        }
        MutationType::SetItemStatus
        | MutationType::UpdateTicket
        | MutationType::UpdateEpic
        | MutationType::AddTicketToEpic
        | MutationType::RemoveTicketFromEpic
        | MutationType::AddDependency
        | MutationType::RemoveDependency
        | MutationType::AddExternalBlocker
        | MutationType::ResolveExternalBlocker
        // Unreachable past the Promotion refusal above; named so the match
        // stays exhaustive rather than resting on that early return.
        | MutationType::PromoteTicket
        | MutationType::PromoteEpic => SkipOutcome::Bypassed,
    };

    mutations::transition(
        &tx,
        mutations::TransitionRequest {
            sequence,
            from: prior,
            to: MutationState::Skipped,
            failure: None,
            now,
        },
    )?;

    tx.commit()?;
    Ok(outcome)
}

// ──────────────────────────────────────────────────────────────────────────
// Pending/failed count
// ──────────────────────────────────────────────────────────────────────────

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
            "select item_class, display_value, ticket_kind, priority, status, title, \
                  work_state \
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
                        // Item Status is derived, not stored (ADR-0043).
                        status: ItemStatus::of(row.get(4)?, row.get(6)?),
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

/// Return the Adopted working set's Backend keys after checking that the
/// Store's Backend kind matches the Adapter.
pub fn working_set_keys(
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
            and status = 'open' \
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
    let local_content_targets = local_content_targets(&tx)?;
    for (key, refresh) in refreshes {
        let existing: Option<(String, ItemClass)> = tx
            .query_row(
                "select id, item_class from items \
                  where backend_kind = ?1 and backend_key = ?2 and status = 'open'",
                params![expected_kind.text(), key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((item_id, item_class)) = existing else {
            continue;
        };
        let content_authority = if local_content_targets.get(item_id.as_str()) == Some(&item_class)
        {
            ContentAuthority::Local
        } else {
            ContentAuthority::Backend
        };
        let BackendItemRefresh {
            title,
            body,
            status,
            ticket_kind,
        } = refresh;
        let (title_write, body_write) = if content_authority == ContentAuthority::Local {
            (None, None)
        } else {
            (Some(title.as_str()), Some(body.as_str()))
        };
        let work_state_write: Option<WorkState> = match status {
            Lifecycle::Open => None,
            Lifecycle::Done => Some(WorkState::Idle),
        };
        // Pull never imports Work State. A Backend close still clears it as a
        // local consequence, while an open refresh must not undo `tk start`
        // (ADR-0043, ADR-0021). Title/body shielding does not affect Lifecycle
        // or Ticket Kind (ADR-0044).
        tx.execute(
            "update items set title = coalesce(?2, title), body = coalesce(?3, body), updated_at = max(updated_at, ?5), \
              ticket_kind = case when item_class = 'ticket' then coalesce(?6, ticket_kind) else ticket_kind end, \
              status = ?4, \
              work_state = coalesce(?7, work_state) \
              where id = ?1",
            params![item_id, title_write, body_write, status.text(), now,
                ticket_kind.map(TicketKind::text), work_state_write],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Whether unresolved local content intent shields Backend Pull's title/body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentAuthority {
    Backend,
    Local,
}

impl ContentAuthority {
    fn for_mutation(mutation_type: MutationType) -> Self {
        match mutation_type {
            MutationType::UpdateTicket | MutationType::UpdateEpic => Self::Local,
            MutationType::SetItemStatus
            | MutationType::AddTicketToEpic
            | MutationType::RemoveTicketFromEpic
            | MutationType::AddDependency
            | MutationType::RemoveDependency
            | MutationType::AddExternalBlocker
            | MutationType::ResolveExternalBlocker
            | MutationType::PromoteTicket
            | MutationType::PromoteEpic => Self::Backend,
        }
    }
}

/// Collect exact Items whose unresolved content Mutations shield title/body.
///
/// Decoding every unresolved Mutation row before the first write exposes
/// unknown Mutation Type or Item Class spellings. The map only adds targets,
/// so non-content Mutations cannot remove a shield added by an earlier row.
fn local_content_targets(conn: &Connection) -> rusqlite::Result<HashMap<String, ItemClass>> {
    let mut stmt = conn.prepare(
        "select item_id, item_class, mutation_type from mutations \
          where state in ('pending', 'failed')",
    )?;
    let mut rows = stmt.query([])?;
    let mut targets = HashMap::new();
    while let Some(row) = rows.next()? {
        let item_id: String = row.get(0)?;
        let item_class: ItemClass = row.get(1)?;
        let mutation_type: MutationType = row.get(2)?;
        if ContentAuthority::for_mutation(mutation_type) == ContentAuthority::Local {
            targets.insert(item_id, item_class);
        }
    }
    Ok(targets)
}

/// The single Backend kind retained backend-bound state belongs to, or `None`
/// when no such state exists. Errors when that state spans more than one kind.
fn retained_backend_kind(conn: &Connection) -> Result<Option<BackendKind>, BackendCohortError> {
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
    if let Some(retained) = retained_backend_kind(conn)?
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

/// The kind of the v1 singleton Remote, or `None` when none is configured.
/// The only Remote read in the store: `remotes.config_json` carries nothing a
/// reader needs under ADR-0033 (the repository is resolved by `gh` from the
/// command cwd), and the Sync Cursor belongs to the Mutation Log views.
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

    if let Some(retained) = retained_backend_kind(&tx)? {
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
        "{0} pending or failed Mutation(s) would be orphaned; resolve them before clearing the Remote. Run 'tk sync' to apply them, or 'tk sync --skip <mutation-id>' to bypass a failed one"
    )]
    WouldOrphan(i64),
    /// The same refusal with a Promotion among the in-flight rows. Sync Skip
    /// refuses a Promotion, so the guidance names the operation that can
    /// actually clear it (ADR-0038).
    #[error(
        "{count} pending or failed Mutation(s) would be orphaned, including Promotion Mutation {promotion}; resolve them before clearing the Remote. Run 'tk promote cancel <id>' to withdraw a Promotion Operation the Backend will never accept"
    )]
    WouldOrphanPromotion { count: i64, promotion: i64 },
    /// A creation whose outcome tk never observed is unresolved intent against
    /// this Remote, so tk refuses to clear it (CONTEXT.md). Carries the Mutation
    /// Sequence for the verbatim diagnostic.
    #[error(
        "Mutation {0} has an indeterminate Backend creation outcome; resolve it before clearing the Remote. Run 'tk promote reconcile <id> <backend-key>' if the Backend object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked"
    )]
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
        return Err(match earliest_in_flight_promotion(&tx)? {
            Some(promotion) => ClearRemoteError::WouldOrphanPromotion {
                count: in_flight,
                promotion,
            },
            None => ClearRemoteError::WouldOrphan(in_flight),
        });
    }

    tx.execute("delete from sync_cursors where remote_name = 'primary'", [])?;
    tx.execute("delete from remotes where name = 'primary'", [])?;

    tx.commit()?;
    Ok(())
}

/// The lowest Mutation Sequence of an in-flight Promotion, if the outbox holds
/// one. Names the Promotion whose Promotion Operation the clear refusal points
/// the operator at.
fn earliest_in_flight_promotion(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "select sequence from mutations \
          where state in ('pending','failed') \
            and mutation_type in ('promote_ticket','promote_epic') \
          order by sequence limit 1",
        [],
        |r| r.get(0),
    )
    .optional()
}

// ──────────────────────────────────────────────────────────────────────────
// Mutation Log read (tk sync log)
// ──────────────────────────────────────────────────────────────────────────

/// Filter for the `tk sync log` list view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogListFilter {
    /// Every state but `applied` — a withdrawal is visible without a flag.
    Default,
    Pending,
    Failed,
    Skipped,
    Cancelled,
    /// Only Promotions withdrawn while their outcome was unobserved — the only
    /// rows that mean tk may have left a Backend object behind (ADR-0039).
    Abandoned,
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

/// Whether the Mutation Log holds no rows at all.
///
/// [`LogListFilter::Default`] omits `applied` Mutations, so an empty list from
/// it does not mean an empty log. `tk sync log` asks this to tell a store that
/// never recorded a Mutation from one whose Mutations have all applied.
pub fn mutation_log_is_empty(conn: &Connection) -> Result<bool, LogError> {
    let any: Option<i64> = conn
        .query_row("select 1 from mutations limit 1", [], |row| row.get(0))
        .optional()?;
    Ok(any.is_none())
}

/// Return Mutation Log rows matching `filter` in ascending sequence order.
pub fn list_mutation_log(
    conn: &Connection,
    filter: LogListFilter,
) -> Result<Vec<LogListRow>, LogError> {
    let where_clause = match filter {
        LogListFilter::Default => {
            "where m.state in ('pending', 'failed', 'applying', 'skipped', 'cancelled', 'abandoned')"
        }
        LogListFilter::Pending => "where m.state = 'pending'",
        LogListFilter::Failed => "where m.state = 'failed'",
        LogListFilter::Skipped => "where m.state = 'skipped'",
        LogListFilter::Cancelled => "where m.state = 'cancelled'",
        LogListFilter::Abandoned => "where m.state = 'abandoned'",
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
    // Decoding happens after the query: a rusqlite row closure can only
    // surface `rusqlite::Error`, not `LogError`.
    let (mut detail, raw_failure) = conn
        .query_row(
            "select m.sequence, m.state, m.mutation_type, i.display_value, \
                    m.item_class, m.payload_json, m.failure_json, \
                    m.created_at, m.state_changed_at \
               from mutations m \
               join items i on i.id = m.item_id and i.item_class = m.item_class \
              where m.sequence = ?1",
            params![sequence],
            |r| {
                let raw_failure: Option<String> = r.get(6)?;
                Ok((
                    LogDetailRow {
                        sequence: r.get(0)?,
                        state: r.get(1)?,
                        mutation_type: r.get(2)?,
                        target_display_id: r.get(3)?,
                        item_class: r.get(4)?,
                        payload_json: r.get(5)?,
                        failure_detail: None,
                        failure_class: None,
                        created_at: r.get(7)?,
                        state_changed_at: r.get(8)?,
                    },
                    raw_failure,
                ))
            },
        )
        .optional()?
        .ok_or(LogError::MutationNotFound(sequence))?;

    if let Some(raw) = raw_failure {
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
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::domain::lifecycle::Lifecycle;
    use crate::domain::ticket_kind::TicketKind;
    use crate::domain::work_state::WorkState;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureFormerIdentity, FixtureItem, FixtureMutation, FixtureRemote, insert_dependency,
        insert_fixture_former_identity, insert_fixture_item, insert_fixture_mutation,
        insert_fixture_remote, item_axes, item_count,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    const NOW: &str = "2026-08-09T00:00:00Z";
    /// Canonical GitHub key for issue 42, whose legacy spelling is `42`.
    const URL_42: &str = "https://github.com/o/r/issues/42";

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    fn adopted(backend_key: &str, display_id: &str, title: &str, status: Lifecycle) -> AdoptedItem {
        AdoptedItem {
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            ticket_kind: TicketKind::Task,
            title: title.into(),
            body: "Body".into(),
            status,
        }
    }

    fn adopt_with_all_capabilities(
        conn: &mut Connection,
        backend_kind: BackendKind,
        rng: &mut dyn rand::Rng,
        item: &AdoptedItem,
        now: &str,
    ) -> Result<AdoptOutcome, AdoptStoreError> {
        adopt_backend_ticket(
            conn,
            backend_kind,
            rng,
            item,
            PromotionCapabilities::all(),
            now,
        )
    }

    fn refresh(title: &str, status: Lifecycle) -> BackendItemRefresh {
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
        let outcome = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("1", "gh-1", "First", Lifecycle::Open),
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
    fn content_authority_is_local_only_for_content_mutations() {
        for mutation_type in MutationType::ALL {
            let expected = matches!(
                mutation_type,
                MutationType::UpdateTicket | MutationType::UpdateEpic
            );
            assert_eq!(
                ContentAuthority::for_mutation(mutation_type) == ContentAuthority::Local,
                expected,
                "unexpected content authority for {mutation_type}"
            );
        }
    }

    #[test]
    fn refresh_preserves_content_for_pending_and_failed_content_mutations() {
        for state in ["pending", "failed"] {
            let mut conn = open_seeded();
            seed_remote(&conn);
            backend_ticket(&conn, "t1", "gh-1", "1", 1);
            conn.execute(
                "update items set ticket_kind = 'bug', status = 'open', work_state = 'active' \
                 where id = 't1'",
                [],
            )
            .unwrap();
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: 1,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"Local title","body":"Local body"}"#,
                    state,
                    failure_json: (state == "failed").then_some(r#"{"detail":"prior"}"#),
                    ..FixtureMutation::default()
                },
            )
            .unwrap();
            conn.execute(
                "update items set title = 'Local title', body = 'Local body' where id = 't1'",
                [],
            )
            .unwrap();

            merge_backend_refreshes(
                &mut conn,
                BackendKind::Github,
                &[(
                    "1".into(),
                    BackendItemRefresh {
                        title: "Stale Backend title".into(),
                        body: "Stale Backend body".into(),
                        status: Lifecycle::Done,
                        ticket_kind: Some(TicketKind::Task),
                    },
                )],
                "2026-05-20T00:00:00Z",
            )
            .unwrap();

            let (title, body, kind, updated): (String, String, Option<String>, String) = conn
                .query_row(
                    "select title, body, ticket_kind, updated_at from items where id = 't1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!((title, body), ("Local title".into(), "Local body".into()));
            assert_eq!(kind.as_deref(), Some("task"));
            assert_eq!(updated, "2026-05-20T00:00:00Z");
            assert_eq!(
                item_axes(&conn, "t1").unwrap(),
                (Lifecycle::Done, WorkState::Idle)
            );
        }
    }

    #[test]
    fn refresh_content_shield_is_monotone_across_all_unresolved_mutations() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_dependency",
                item_id: "t1",
                payload_json: r#"{"blocking_id":"other"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local title","body":"Local body"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "add_dependency",
                item_id: "t1",
                payload_json: r#"{"blocking_id":"later"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set title = 'Local title', body = 'Local body' where id = 't1'",
            [],
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend title", Lifecycle::Done))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, body): (String, String) = conn
            .query_row("select title, body from items where id = 't1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title, body), ("Local title".into(), "Local body".into()));
        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn refresh_keeps_newer_timestamp_from_concurrent_content_edit() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        assert_eq!(
            working_set_keys(&conn, BackendKind::Github).unwrap(),
            vec!["1".to_owned()]
        );

        // A local edit can commit while sync waits for the Backend. Pull must
        // preserve both its content and its newer Repository Store timestamp.
        conn.execute(
            "update items set title = 'Local title', body = 'Local body', \
             updated_at = '2026-05-21T00:00:00.000Z' where id = 't1'",
            [],
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local title","body":"Local body"}"#,
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Stale title", Lifecycle::Done))],
            "2026-05-20T00:00:00.000Z",
        )
        .unwrap();

        let (title, body, updated): (String, String, String) = conn
            .query_row(
                "select title, body, updated_at from items where id = 't1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((title, body), ("Local title".into(), "Local body".into()));
        assert_eq!(updated, "2026-05-21T00:00:00.000Z");
        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn refresh_content_shield_is_scoped_to_its_exact_item() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        conn.execute(
            "update items set title = 'Local title', body = 'Local body', ticket_kind = 'bug' \
             where id = 't1'",
            [],
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local title","body":"Local body"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[
                (
                    "1".into(),
                    BackendItemRefresh {
                        title: "Stale title".into(),
                        body: "Stale body".into(),
                        status: Lifecycle::Done,
                        ticket_kind: Some(TicketKind::Task),
                    },
                ),
                (
                    "2".into(),
                    BackendItemRefresh {
                        title: "Backend title".into(),
                        body: "Backend body".into(),
                        status: Lifecycle::Done,
                        ticket_kind: Some(TicketKind::Bug),
                    },
                ),
            ],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let rows = conn
            .prepare("select title, body, ticket_kind, status from items order by created_seq")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "Local title".into(),
                    "Local body".into(),
                    "task".into(),
                    "done".into()
                ),
                (
                    "Backend title".into(),
                    "Backend body".into(),
                    "bug".into(),
                    "done".into()
                )
            ]
        );
    }

    #[test]
    fn refresh_admits_content_for_non_content_mutations_only() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "add_dependency",
                item_id: "t1",
                payload_json: r#"{"blocking_id":"other"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend title", Lifecycle::Open))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Backend title");
    }

    #[test]
    fn refresh_admits_content_after_content_mutation_is_terminal() {
        for state in ["applied", "skipped"] {
            let mut conn = open_seeded();
            seed_remote(&conn);
            backend_ticket(&conn, "t1", "gh-1", "1", 1);
            conn.execute(
                "update items set title = 'Local title', body = 'Local body' where id = 't1'",
                [],
            )
            .unwrap();
            insert_fixture_mutation(
                &conn,
                FixtureMutation {
                    sequence: 1,
                    mutation_type: "update_ticket",
                    item_id: "t1",
                    payload_json: r#"{"title":"Local title","body":"Local body"}"#,
                    state,
                    ..FixtureMutation::default()
                },
            )
            .unwrap();

            merge_backend_refreshes(
                &mut conn,
                BackendKind::Github,
                &[("1".into(), refresh("Backend title", Lifecycle::Open))],
                "2026-05-20T00:00:00Z",
            )
            .unwrap();

            let (title, body): (String, String) = conn
                .query_row("select title, body from items where id = 't1'", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!((title, body), ("Backend title".into(), "Body".into()));
        }
    }

    #[test]
    fn refresh_preserves_ticket_kind_when_backend_omits_it() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        conn.execute(
            "update items set ticket_kind = 'bug', title = 'Local title' where id = 't1'",
            [],
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local title","body":""}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[(
                "1".into(),
                BackendItemRefresh {
                    title: "Backend title".into(),
                    body: "Body".into(),
                    status: Lifecycle::Done,
                    ticket_kind: None,
                },
            )],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, kind, updated): (String, String, String) = conn
            .query_row(
                "select title, ticket_kind, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Local title");
        assert_eq!(kind, "bug");
        assert_eq!(updated, "2026-05-20T00:00:00Z");
        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn refresh_preserves_epic_content_and_ticket_kind_null_with_content_mutation() {
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
                title: "Local epic title",
                body: "Local epic body",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "update_epic",
                item_id: "e1",
                item_class: "epic",
                payload_json: r#"{"title":"Local epic title","body":"Local epic body"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[(
                "9".into(),
                BackendItemRefresh {
                    title: "Backend epic title".into(),
                    body: "Backend epic body".into(),
                    status: Lifecycle::Done,
                    ticket_kind: Some(TicketKind::Task),
                },
            )],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, body, kind, updated): (String, String, Option<String>, String) = conn
            .query_row(
                "select title, body, ticket_kind, updated_at from items where id = 'e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (title, body),
            ("Local epic title".into(), "Local epic body".into())
        );
        assert_eq!(kind, None);
        assert_eq!(updated, "2026-05-20T00:00:00Z");
        assert_eq!(
            item_axes(&conn, "e1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn refresh_skips_item_that_left_the_working_set_during_pull() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        backend_ticket(&conn, "t2", "gh-2", "2", 2);
        assert_eq!(
            working_set_keys(&conn, BackendKind::Github).unwrap(),
            vec!["1".to_owned(), "2".to_owned()]
        );

        // A local close can commit while sync waits for the Backend. The
        // stale refresh must not reopen the Item or abort the batch.
        conn.execute(
            "update items set status = 'done', work_state = 'idle' where id = 't1'",
            [],
        )
        .unwrap();
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

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[
                ("1".into(), refresh("Stale open", Lifecycle::Open)),
                ("2".into(), refresh("Closed upstream", Lifecycle::Done)),
            ],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let rows = conn
            .prepare("select title, status from items order by created_seq")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("Old".into(), "done".into()),
                ("Closed upstream".into(), "done".into())
            ]
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
            &[("1".into(), refresh("Backend Wins", Lifecycle::Done))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let (title, updated): (String, String) = conn
            .query_row(
                "select title, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "Backend Wins");
        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
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
                ("1".into(), refresh("New one", Lifecycle::Done)),
                ("2".into(), refresh("New two", Lifecycle::Open)),
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
                ("1".into(), refresh("New one", Lifecycle::Done)),
                ("2".into(), refresh("New two", Lifecycle::Open)),
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
            &[("404".into(), refresh("Unknown", Lifecycle::Open))],
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
            &[("9".into(), refresh("Fresh Epic", Lifecycle::Open))],
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
        let err = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("99", "gh-1", "Backend", Lifecycle::Open),
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
        // it back, so a Pull merging title/status must leave a local `parked`
        // state and its Priority untouched. Regression lock against a future
        // edit adding Selection State or Priority to the refresh write. Its
        // sibling below draws the other side of the same boundary: Work State
        // is equally local, and Pull writes it only as the consequence of a
        // Lifecycle transition Pull is authoritative for (ADR-0043).
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
            &[("1".into(), refresh("Backend Title", Lifecycle::Open))],
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
    fn refresh_clears_work_state_on_close() {
        // The one Work State write Backend Pull is allowed (ADR-0043): not an
        // imported value — the Adapter reads no in-progress state — but the
        // local consequence of landing a closed Lifecycle, since a closed issue
        // means the work is over. `tk done` clears it the same way. Without
        // this the merge would leave a `(done, active)` row, which nothing else
        // in the store can produce.
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
                status: "active",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Closed Upstream", Lifecycle::Done))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn refresh_keeps_active_when_the_backend_reports_open() {
        // Work State is a Local Field Backend Pull never reads (ADR-0043), so
        // an imported OPEN must not reset a start. A failure here loses
        // `tk start` on a Backend Ticket at the next sync (tk-108). The test
        // pins the merge rather than a whole sync because in the field the
        // reset lands on the *second* one: the first skips this item while its
        // own status Mutation is still queued.
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
                status: "active",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Backend Title", Lifecycle::Open))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let title: String = conn
            .query_row("select title from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Backend Title", "non-status fields still merge");
        assert_eq!(
            item_axes(&conn, "t1").unwrap(),
            (Lifecycle::Open, WorkState::Active),
            "an incoming open must not stop the work"
        );
    }

    #[test]
    fn refresh_closes_an_active_ticket() {
        // The other half of the two-state axis: CLOSED is a real state change,
        // so keeping `active` must not swallow an incoming `done`.
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
                status: "active",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        merge_backend_refreshes(
            &mut conn,
            BackendKind::Github,
            &[("1".into(), refresh("Closed Upstream", Lifecycle::Done))],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        let status: String = conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "done", "a Backend close still lands");
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
            MutationPayload::Lifecycle(s) => assert_eq!(s.status, Lifecycle::Done),
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
    fn resolve_operation_turns_a_pending_promotion_into_a_backend_creation() {
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
        let BackendCreate::Ticket {
            snapshot,
            ticket_kind,
        } = create
        else {
            panic!("expected Ticket creation")
        };
        assert_eq!(snapshot.title, "T");
        assert_eq!(ticket_kind, TicketKind::Task);
    }

    #[test]
    fn resolve_operation_reads_the_current_bug_kind_for_creation() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local",
                ticket_kind: Some("bug"),
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

        let row = load_applicable_mutations(&conn).unwrap().remove(0);
        let BackendOperation::Create(BackendCreate::Ticket { ticket_kind, .. }) =
            resolve_backend_operation(&conn, row).unwrap().operation
        else {
            panic!("Promotion must resolve as Ticket creation")
        };
        assert_eq!(ticket_kind, TicketKind::Bug);
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
    fn load_applicable_rejects_a_pre_split_active_status_target() {
        // Lifecycle has no `active` variant — Work State split out of Item
        // Status in ADR-0043, and the amendment recorded there
        // documents this: a `set_item_status` row that predates the split and
        // still names `active` now fails to decode here, before any Backend
        // Adapter call, rather than failing at Apply.
        let conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-1", "1", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"active"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        match load_applicable_mutations(&conn).unwrap_err() {
            err @ LoadApplicableError::PayloadJson { sequence: 1, .. } => {
                assert_eq!(
                    err.to_string(),
                    "mutation 1 (set_item_status) has malformed payload_json: \
                     unknown variant `active`, expected `open` or `done` at line 1 column 18"
                );
            }
            other => panic!("expected PayloadJson, got {other:?}"),
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
        let outcome =
            mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();
        assert_eq!(outcome, SkipOutcome::Bypassed);

        let (state, failure): (String, String) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "skipped");
        assert!(failure.contains("rejected"), "audit trail preserved");

        let status: String = conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "open", "Bypassed leaves the Item untouched");
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

    /// Seed a `done`, backend-bound Item at id `t1` with a failed closing
    /// `set_item_status` Mutation at sequence 1 targeting `done` — the one
    /// shape ADR-0046's reopen exception covers.
    fn seed_closed_with_failed_close(conn: &Connection, item_class: &str, display: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t1",
                display,
                item_class,
                ticket_kind: (item_class == "ticket").then_some("task"),
                priority: (item_class == "ticket").then_some("P2"),
                title: "Needs its close relinquished",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("53"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // `FixtureItem` has no Closing Reason field; set one directly so the
        // test can assert Sync Skip clears it.
        conn.execute(
            "update items set closing_reason = 'Not planned' where id = 't1'",
            [],
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                item_class,
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn mark_skipped_relinquishes_a_failed_close_on_a_ticket() {
        let mut conn = open_seeded();
        seed_closed_with_failed_close(&conn, "ticket", "gh-53");

        let workflow = RemoteWorkflowGuard::for_test();
        let outcome =
            mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();
        assert_eq!(
            outcome,
            SkipOutcome::RelinquishedClose {
                display_id: "gh-53".to_string()
            }
        );

        let (status, work_state, closing_reason, updated_at): (
            String,
            String,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "select status, work_state, closing_reason, updated_at from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "open");
        assert_eq!(work_state, "idle");
        assert_eq!(closing_reason, None, "Closing Reason cleared");
        assert_eq!(updated_at, "2026-05-19T00:00:00Z");

        let mutation_state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mutation_state, "skipped");
    }

    #[test]
    fn mark_skipped_relinquishes_a_failed_close_on_an_epic() {
        // ADR-0046: "The rule is identical for Tickets and Epics."
        let mut conn = open_seeded();
        seed_closed_with_failed_close(&conn, "epic", "gh-53");

        let workflow = RemoteWorkflowGuard::for_test();
        let outcome =
            mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();
        assert_eq!(
            outcome,
            SkipOutcome::RelinquishedClose {
                display_id: "gh-53".to_string()
            }
        );

        let (status, work_state, closing_reason): (String, String, Option<String>) = conn
            .query_row(
                "select status, work_state, closing_reason from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "open");
        assert_eq!(work_state, "idle");
        assert_eq!(closing_reason, None);
    }

    #[test]
    fn mark_skipped_relinquish_preserves_selection_state_and_priority() {
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-53",
                item_class: "ticket",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("53"),
                title: "Parked with a Priority",
                selection_state: Some("parked"),
                priority: Some("P1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        let outcome =
            mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();
        assert_eq!(
            outcome,
            SkipOutcome::RelinquishedClose {
                display_id: "gh-53".to_string()
            }
        );

        let (selection_state, priority): (String, String) = conn
            .query_row(
                "select selection_state, priority from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(selection_state, "parked");
        assert_eq!(priority, "P1");

        // ADR-0046's first Consequence, asserted rather than composed: the
        // reopen restores Lifecycle, so a parked Ticket must still be held out
        // of selection. Reading it through `next_ready_ticket` means a column
        // added to that predicate later cannot quietly falsify the claim.
        let store = crate::store::repository::Store::for_test(conn);
        assert!(
            repository::next::next_ready_ticket(&store, repository::next::NextOptions::default())
                .unwrap()
                .is_none(),
            "a parked Ticket stays out of tk next after its close is relinquished"
        );
    }

    #[test]
    fn mark_skipped_relinquish_returns_an_accepted_ticket_to_tk_next() {
        // The other half of the same Consequence: an accepted, idle Ticket
        // becomes eligible again. `tk next` reads exactly the three columns
        // the reopen writes or preserves (status, work_state,
        // selection_state), so this is the claim's only direct assertion.
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-53",
                item_class: "ticket",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("53"),
                title: "Accepted, closed, close failed",
                selection_state: Some("accepted"),
                priority: Some("P1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();

        let store = crate::store::repository::Store::for_test(conn);
        let next =
            repository::next::next_ready_ticket(&store, repository::next::NextOptions::default())
                .unwrap()
                .expect("the reopened accepted Ticket is selectable again (ADR-0046)");
        assert_eq!(next.display_id, "gh-53");
    }

    #[test]
    fn mark_skipped_gate_reads_the_named_row_not_any_row_on_the_item() {
        // The regression an `exists`-shaped gate would cause: an Item holding
        // both a spared non-closing `set_item_status` failure (migration 011)
        // and a real failed close. Skipping the non-closing row must not
        // reopen the Item or report a relinquishment that never happened —
        // the real close stays `failed`, free to retry (ADR-0046).
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-53",
                item_class: "ticket",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("53"),
                title: "Closed with a stale non-closing failure",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"active"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"stale, non-closing"}"#),
                ..FixtureMutation::default()
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
                state: "failed",
                failure_json: Some(r#"{"detail":"the real close"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        let outcome =
            mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap();
        assert_eq!(outcome, SkipOutcome::Bypassed);

        let status: String = conn
            .query_row("select status from items where id = 't1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "done", "the real close still authorizes a retry");

        let (skipped_state, closing_state): (String, String) = conn
            .query_row(
                "select \
                    (select state from mutations where sequence = 1), \
                    (select state from mutations where sequence = 2)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(skipped_state, "skipped");
        assert_eq!(closing_state, "failed");
    }

    #[test]
    fn mark_skipped_aborts_when_the_reopen_matches_no_row() {
        // `close_item` never appends a closing Mutation until its own
        // transaction has already landed `done`, so a `failed`
        // `set_item_status` row targeting `done` should imply its Item is
        // `done` too. Seed a broken instance of that invariant — the Item is
        // still `open` — and confirm the zero-row guard aborts rather than
        // silently committing the Mutation to `skipped` regardless.
        let mut conn = open_seeded();
        backend_ticket(&conn, "t1", "gh-53", "53", 1);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        match mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::ReopenMatchedNothing(1) => {}
            other => panic!("expected ReopenMatchedNothing, got {other:?}"),
        }

        // The Mutation stayed `failed`, proving the transaction rolled back
        // rather than committing the transition without its reopen.
        let mutation_state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mutation_state, "failed");
    }

    #[test]
    fn mark_skipped_reports_a_trigger_refusal_as_its_own_invariant_break() {
        // Migrations 015 and 016 have each recreated `items_no_escape_from_done`
        // from scratch, and the object inventory an ADR-0028 rebuild is checked
        // against names triggers without comparing their bodies. A future
        // rebuild that copies an older body therefore drops the ADR-0046
        // conjunct silently. Stand in for that with the bare done-terminal
        // trigger migration 002 first wrote, carrying neither later exception,
        // and confirm Sync Skip names the Store invariant rather than
        // reporting a storage fault.
        let mut conn = open_seeded();
        conn.execute_batch(
            "drop trigger items_no_escape_from_done; \
             create trigger items_no_escape_from_done before update of status on items \
             for each row when old.status = 'done' and new.status != 'done' \
             begin select raise(abort, 'cannot leave done state'); end;",
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-53",
                title: "Closed locally",
                status: "done",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("53"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "set_item_status",
                item_id: "t1",
                payload_json: r#"{"status":"done"}"#,
                state: "failed",
                failure_json: Some(r#"{"detail":"rejected"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let workflow = RemoteWorkflowGuard::for_test();
        match mark_mutation_skipped(&mut conn, &workflow, 1, "2026-05-19T00:00:00Z").unwrap_err() {
            MarkSkippedError::ReopenRefusedByTrigger(1) => {}
            other => panic!("expected ReopenRefusedByTrigger, got {other:?}"),
        }

        let mutation_state: String = conn
            .query_row("select state from mutations where sequence = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mutation_state, "failed");
    }

    // ---- pending/failed count -------------------------------------------

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

        assert_eq!(
            configured_remote_kind(&conn).unwrap(),
            Some(BackendKind::Github)
        );
        let config_json: String = conn
            .query_row(
                "select config_json from remotes where name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(config_json, "{}");
        // `set_remote` seeds the Sync Cursor at 0 (ADR-0033); losing this
        // would let a new Remote start mid-log and skip Mutations.
        let last_applied: i64 = conn
            .query_row(
                "select last_applied_sequence from sync_cursors where remote_name = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_applied, 0);

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
        assert_eq!(
            configured_remote_kind(&conn).unwrap(),
            Some(BackendKind::Github)
        );
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
        assert_eq!(configured_remote_kind(&conn).unwrap(), None);
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
        assert_eq!(configured_remote_kind(&conn).unwrap(), None);
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
        assert_eq!(configured_remote_kind(&conn).unwrap(), None);
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
        assert_eq!(
            configured_remote_kind(&conn).unwrap(),
            Some(BackendKind::Github)
        );
    }

    #[test]
    fn clear_remote_removes_remote_and_cursor_when_clean() {
        let mut conn = open_seeded();
        set_remote(&mut conn, BackendKind::Github, "{}", "2026-06-17T00:00:00Z").unwrap();

        clear_remote(&mut conn).unwrap();

        assert_eq!(configured_remote_kind(&conn).unwrap(), None);
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
        assert!(configured_remote_kind(&conn).unwrap().is_some());
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
        assert!(configured_remote_kind(&conn).unwrap().is_some());
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
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "t5",
                display: "tk-5",
                title: "Withdrawn work",
                created_seq: 5,
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
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 5,
                mutation_type: "update_ticket",
                item_id: "t3",
                payload_json: r#"{"title":"D","body":""}"#,
                state: "cancelled",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 6,
                mutation_type: "promote_ticket",
                item_id: "t5",
                payload_json: r#"{"title":"Withdrawn work","body":"","backend_kind":"github"}"#,
                state: "abandoned",
                failure_json: Some(r#"{"detail":"unknown effect"}"#),
                promotion_operation_id: Some("op-2"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        // The one state the default list leaves out. Without a row in it, a
        // filter that started returning `applied` would still pass every
        // assertion below, and `tk sync log`'s drained-log line rests on that
        // exclusion.
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "update_ticket",
                item_id: "t3",
                payload_json: r#"{"title":"Landed","body":""}"#,
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn list_default_returns_every_state_but_applied_with_failure_detail() {
        // A withdrawal is visible without a flag (ADR-0038).
        let conn = open_seeded();
        seed_log_fixture(&conn);

        let rows = list_mutation_log(&conn, LogListFilter::Default).unwrap();
        let seqs: Vec<i64> = rows.iter().map(|r| r.sequence).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3, 4, 5, 6],
            "Mutation 7 is applied, the one state this list leaves out"
        );
        assert_eq!(rows[0].failure_detail, None);
        assert_eq!(
            rows[1].failure_detail.as_deref(),
            Some("HTTP 422: rejected")
        );
        assert_eq!(rows[1].target_display_id, "gh-2");
        assert_eq!(rows[3].state, MutationState::Applying);
        assert_eq!(rows[3].failure_detail.as_deref(), Some("unknown effect"));
        assert_eq!(rows[4].state, MutationState::Cancelled);
        assert_eq!(rows[5].state, MutationState::Abandoned);
        assert_eq!(
            rows[5].failure_detail.as_deref(),
            Some("unknown effect"),
            "the indeterminate diagnostic survives the withdrawal"
        );
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
        let cancelled = list_mutation_log(&conn, LogListFilter::Cancelled).unwrap();
        assert_eq!(
            cancelled.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![5],
            "a Cancelled Mutation is separable from a Skipped one"
        );
        let abandoned = list_mutation_log(&conn, LogListFilter::Abandoned).unwrap();
        assert_eq!(
            abandoned.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![6],
            "an Abandoned Mutation is the one state that means tk may have left an object behind"
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

    // ---- adopt ----------------------------------------------------------

    #[test]
    fn readopt_refuses_relationship_intent_the_backend_cannot_represent() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "stable",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        backend_ticket(&conn, "blocker", "gh-8", "8", 2);
        insert_dependency(&conn, "blocker", "stable").unwrap();
        insert_fixture_former_identity(
            &conn,
            FixtureFormerIdentity {
                item_id: "stable",
                backend_key: URL_42,
                backend_display_value: "gh-42",
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();
        let snapshot = adopted(URL_42, "gh-42", "Backend work", Lifecycle::Open);

        let requirements = readopt_requirements(&conn, BackendKind::Github, &snapshot)
            .unwrap()
            .unwrap();
        assert!(requirements.requires_dependencies());
        let error = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut StdRng::seed_from_u64(7),
            &snapshot,
            PromotionCapabilities::none(),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AdoptStoreError::ReadoptRelationships { ref findings, .. }
                if matches!(
                    findings.as_slice(),
                    [RelationshipFinding::DependencyNotRepresentable { blocked, blocking }]
                        if blocked.display_id == "tk-1" && blocking.display_id == "gh-8"
                )
        ));
        let origin: String = conn
            .query_row("select origin from items where id = 'stable'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(origin, "local");
        assert_eq!(pending_or_failed_mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn readopt_refuses_membership_the_backend_cannot_represent() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "parent",
                display: "gh-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                selection_state: None,
                title: "Backend Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "stable",
                display: "tk-1",
                title: "Local work",
                container_id: Some("parent"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_former_identity(
            &conn,
            FixtureFormerIdentity {
                item_id: "stable",
                backend_key: URL_42,
                backend_display_value: "gh-42",
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();
        let snapshot = adopted(URL_42, "gh-42", "Backend work", Lifecycle::Open);

        let error = adopt_backend_ticket(
            &mut conn,
            BackendKind::Github,
            &mut StdRng::seed_from_u64(7),
            &snapshot,
            PromotionCapabilities::none(),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AdoptStoreError::ReadoptRelationships { ref findings, .. }
                if matches!(
                    findings.as_slice(),
                    [RelationshipFinding::EpicMembershipNotRepresentable { ticket, epic }]
                        if ticket.display_id == "tk-1" && epic.display_id == "gh-9"
                )
        ));
        let origin: String = conn
            .query_row("select origin from items where id = 'stable'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(origin, "local");
        assert_eq!(pending_or_failed_mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn former_epic_requirements_include_membership_from_its_backend_ticket() {
        let conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "stable",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                selection_state: None,
                title: "Local Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_former_identity(
            &conn,
            FixtureFormerIdentity {
                item_id: "stable",
                backend_key: URL_42,
                backend_display_value: "gh-42",
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "child",
                display: "gh-8",
                title: "Backend child",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("8"),
                container_id: Some("stable"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let requirements = readopt_requirements(
            &conn,
            BackendKind::Github,
            &adopted(URL_42, "gh-42", "Backend Epic", Lifecycle::Open),
        )
        .unwrap()
        .unwrap();

        assert!(requirements.requires_epic_membership());
    }

    #[test]
    fn canonical_adopt_is_idempotent_without_overwriting_stored_fields() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let first = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42", "Original", Lifecycle::Open),
            NOW,
        )
        .unwrap();
        assert!(matches!(first, AdoptOutcome::Inserted(_)));

        let mut canonical = adopted("42", "gh-42", "Original", Lifecycle::Open);
        canonical.title = "Changed remotely".into();
        let second =
            adopt_with_all_capabilities(&mut conn, BackendKind::Github, &mut rng, &canonical, NOW)
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
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted(
                "https://github.com/one/repo/issues/42",
                "gh-42",
                "Original",
                Lifecycle::Open,
            ),
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
            adopt_with_all_capabilities(
                &mut conn,
                BackendKind::Github,
                &mut rng,
                &adopted("https://github.com/other/repo/issues/42", "gh-42", "Original", Lifecycle::Open),
                NOW,
            ),
            Err(AdoptStoreError::DisplayIdCollision(id)) if id == "gh-42"
        ));
    }

    #[test]
    fn a_former_identity_of_another_backend_does_not_capture_adopt() {
        let mut conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "jira",
                config_json: r#"{"site":"x","project":"P"}"#,
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "detached",
                display: "tk-1",
                title: "Former GitHub work",
                created_seq: 9,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_former_identity(
            &conn,
            FixtureFormerIdentity {
                backend_key: "42",
                item_id: "detached",
                backend_display_value: "gh-42",
                detached_at: NOW,
                ..FixtureFormerIdentity::default()
            },
        )
        .unwrap();

        // Canonical identity is the (Backend kind, key) pair: a key spelled
        // the same on another Backend names a different object, so Jira intake
        // inserts rather than rebinding the legacy GitHub history.
        let outcome = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Jira,
            &mut rand::rngs::StdRng::seed_from_u64(7),
            &adopted("42", "jira-42", "Jira work", Lifecycle::Open),
            NOW,
        )
        .unwrap();

        assert!(matches!(outcome, AdoptOutcome::Inserted(_)));
        assert_eq!(item_count(&conn).unwrap(), 2);
        let origin: String = conn
            .query_row(
                "select origin from items where id = 'detached'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(origin, "local");
    }

    #[test]
    fn the_canonical_spelling_wins_two_matching_history_rows() {
        let mut conn = open_seeded();
        seed_remote(&conn);
        for (id, display, created_seq) in [("legacy", "tk-1", 9), ("canonical", "tk-2", 10)] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title: "Detached work",
                    created_seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
        }
        // Both rows name issue 42. The ownership invariant compares key
        // strings, so the two spellings coexist and Re-Adopt has to choose.
        // The lookup's `order by` makes the canonical spelling win; this pins
        // that as the contract, so a rewritten predicate cannot hand the
        // identity to the other Item.
        for (backend_key, item_id, detached_seq) in [("42", "legacy", 1), (URL_42, "canonical", 2)]
        {
            insert_fixture_former_identity(
                &conn,
                FixtureFormerIdentity {
                    backend_key,
                    item_id,
                    backend_display_value: "gh-42",
                    detached_seq,
                    detached_at: NOW,
                    ..FixtureFormerIdentity::default()
                },
            )
            .unwrap();
        }

        let outcome = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rand::rngs::StdRng::seed_from_u64(7),
            &adopted(URL_42, "gh-42", "Fresh title", Lifecycle::Open),
            NOW,
        )
        .unwrap();

        let AdoptOutcome::Readopted(report) = outcome else {
            panic!("expected Re-Adopt, got {outcome:?}");
        };
        assert_eq!(report.local_display_id, "tk-2");
        assert_eq!(report.backend_key, URL_42);
        // The Item holding the weaker spelling keeps its reservation instead of
        // being displaced.
        let legacy_origin: String = conn
            .query_row("select origin from items where id = 'legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(legacy_origin, "local");
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
                adopt_with_all_capabilities(
                    &mut conn,
                    BackendKind::Github,
                    &mut rng,
                    &adopted("42", "gh-42", "Original", Lifecycle::Open),
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
        let mut conn = open_seeded();
        seed_remote(&conn);
        clear_remote(&mut conn).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);

        let error = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42", "Original", Lifecycle::Open),
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
        let mut conn = open_seeded();
        seed_remote(&conn);
        clear_remote(&mut conn).unwrap();
        set_remote(&mut conn, BackendKind::Jira, "{}", NOW).unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);

        let error = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42", "Original", Lifecycle::Open),
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
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("1", "gh-1", "Original", Lifecycle::Open),
            NOW,
        )
        .unwrap();
        conn.execute(
            "update remotes set backend_kind = 'jira' where name = 'primary'",
            [],
        )
        .unwrap();

        let error = adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Jira,
            &mut rng,
            &adopted("2", "jira-2", "Original", Lifecycle::Open),
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
        let mut conn = open_seeded();
        seed_remote(&conn);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        adopt_with_all_capabilities(
            &mut conn,
            BackendKind::Github,
            &mut rng,
            &adopted("42", "gh-42", "Original", Lifecycle::Open),
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
                    status: Lifecycle::Open,
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
