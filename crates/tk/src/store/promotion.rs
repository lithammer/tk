//! Store-side Promotion helpers: the preflight graph read at the start of
//! `tk promote`, the outbox commit that follows planning, and receipt
//! application at the end of it (ADR-0035, ADR-0036).
//!
//! [`read_graph`] gathers the facts
//! [`crate::domain::promotion_graph::PromotionGraph`] carries; the planner that
//! reasons over them owns every decision.
//!
//! [`commit_plan`] writes the planner's ordered
//! [`crate::domain::promotion_plan::PromotionPlan`] to the Mutation Log outbox
//! as one Promotion Operation, in the single local transaction ADR-0035
//! requires before any Backend call.
//!
//! [`apply_receipt`] runs inside the caller's open transaction — it
//! takes a borrowed connection and neither begins nor commits one — so the
//! conversion commits together with the Mutation Log state transition that
//! records the Promotion as applied. No window exists in which a Mutation is
//! `applied` while its Item is still Local.
//!
//! [`earliest_applicable_mutation`] and [`unresolved_in_operation`]
//! close the loop: after the sync that follows the commit, they are how the
//! command tells a Promotion queued behind an older Mutation from one whose
//! own Mutations did not land.
//!
//! Promotion recovery also lives here (ADR-0037, ADR-0038, ADR-0039):
//! [`recoverable_promotion`] and [`capture_recovery_mappings`] locate what a
//! recovery acts on, [`reconcile_promotion`] and [`retry_promotion`] each
//! resolve one nonterminal Promotion in a single transaction,
//! [`cancel_promotion`] withdraws a whole Promotion Operation, and
//! [`abandoned_promotions`] warns when an earlier withdrawal left a Backend
//! object tk cannot address.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::BackendItemIdentity;
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, MutationPayload, Promotion, TitleBody,
};
use crate::domain::mutation_state::MutationState;
use crate::domain::mutation_type::{AddressedCounterpart, MutationType};
use crate::domain::origin::Origin;
use crate::domain::promotion_graph::{GraphDependency, GraphItem, PromotionGraph};
use crate::domain::promotion_plan::PromotionPlan;
use crate::domain::ticket_kind::TicketKind;
use crate::store::mutations;
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::repository::create::generate_internal_id;

/// Error returned by [`read_graph`].
#[derive(Debug, Error)]
pub enum ReadGraphError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
}

/// Snapshot the Items and Dependency edges a `tk promote` of `target_id`
/// preflights over. `children_requested` mirrors `--children`.
///
/// Membership, in order of how it is built:
///
/// - the target;
/// - every Item the target directly contains, whether or not children were
///   requested. `--children` decides which contained Tickets are Promotion
///   Children, but promoting an Epic also snapshots membership for the
///   Tickets it already contains on the same Backend, so the planner needs
///   them either way. Only an Epic can contain Items, so a Ticket target
///   contributes none, and Origin is not filtered here because both
///   questions are the planner's;
/// - the target's containing Epic, when it has one — Epic membership is a
///   facet the operation has to decide about;
/// - both endpoints of every Dependency edge that touches any of the above.
///
/// Edges are collected from that pre-expansion set alone: Promotion inspects
/// one hop out to judge the resulting graph but never follows Dependencies to
/// discover further items (ADR-0035). That bound is what keeps both endpoints
/// of every returned edge present in `items`.
///
/// Edges whose Blocking Item is `done` are retained — a done Blocking Item
/// resolves readiness without removing its Dependency (ADR-0035), so the
/// resulting backend graph still needs it. The `tk show` dependency reads
/// filter `status <> 'done'` because they answer a readiness question; this
/// one does not.
///
/// A `target_id` no `items` row matches is a caller fault (the command
/// resolves the Display ID first) and surfaces as
/// [`ReadGraphError::Storage`].
pub fn read_graph(
    conn: &Connection,
    target_id: &str,
    children_requested: bool,
) -> Result<PromotionGraph, ReadGraphError> {
    let mut core: BTreeSet<String> = BTreeSet::new();
    core.insert(target_id.to_owned());

    let container_id: Option<String> = conn.query_row(
        "select container_id from items where id = ?1",
        params![target_id],
        |r| r.get(0),
    )?;
    if let Some(container_id) = container_id {
        core.insert(container_id);
    }

    {
        let mut stmt = conn.prepare("select id from items where container_id = ?1")?;
        let rows = stmt.query_map(params![target_id], |r| r.get::<_, String>(0))?;
        for id in rows {
            core.insert(id?);
        }
    }

    // An edge with both endpoints in `core` matches this query twice; the set
    // collapses it, and orders the result deterministically for a caller that
    // does not re-sort.
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    let mut stmt = conn.prepare(
        "select blocked_id, blocking_id from dependencies \
          where blocked_id = ?1 or blocking_id = ?1",
    )?;
    for id in &core {
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for edge in rows {
            edges.insert(edge?);
        }
    }

    let mut item_ids = core;
    for (blocked_id, blocking_id) in &edges {
        item_ids.insert(blocked_id.clone());
        item_ids.insert(blocking_id.clone());
    }

    let mut items = Vec::with_capacity(item_ids.len());
    for id in &item_ids {
        items.push(read_graph_item(conn, id)?);
    }
    items.sort_by_key(|item| item.created_seq);

    Ok(PromotionGraph {
        target_id: target_id.to_owned(),
        children_requested,
        items,
        dependencies: edges
            .into_iter()
            .map(|(blocked_id, blocking_id)| GraphDependency {
                blocked_id,
                blocking_id,
            })
            .collect(),
    })
}

fn read_graph_item(conn: &Connection, id: &str) -> Result<GraphItem, ReadGraphError> {
    let backend_binding = mutations::resolve_backend_binding(conn, id)?;
    let item = conn.query_row(
        "select display_value, item_class, ticket_kind, selection_state, status, \
                title, body, created_seq, container_id \
           from items where id = ?1",
        params![id],
        |r| {
            Ok(GraphItem {
                id: id.to_owned(),
                display_id: r.get(0)?,
                item_class: r.get(1)?,
                ticket_kind: r.get(2)?,
                selection_state: r.get(3)?,
                status: r.get(4)?,
                title: r.get(5)?,
                body: r.get(6)?,
                created_seq: r.get(7)?,
                container_id: r.get(8)?,
                backend_binding,
            })
        },
    )?;
    Ok(item)
}

/// Error returned by [`commit_plan`].
#[derive(Debug, Error)]
pub enum CommitPlanError {
    /// Underlying SQLite error opening or committing the transaction.
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// Underlying error from appending one draft to the outbox.
    #[error(transparent)]
    Append(#[from] mutations::AppendError),
    #[error(transparent)]
    BackendCohort(#[from] crate::store::sync::BackendCohortError),
    /// The configured Remote changed after Promotion preflight.
    #[error("Remote changed while preparing the Promotion; retry the command")]
    RemoteChanged {
        expected: BackendKind,
        actual: Option<BackendKind>,
    },
}

/// Commit `plan` to the Mutation Log outbox as one Promotion Operation, in
/// the single local transaction ADR-0035 requires before any Backend call: a
/// process failure part-way through must leave no partial Promotion.
///
/// Mints one Promotion Operation identity for the invocation with
/// [`generate_internal_id`] — the same opaque-ID scheme `items.id` uses,
/// reused rather than inventing a second one — and stamps every appended
/// Mutation with it (ADR-0036), so a caller can later ask whether every
/// Mutation belonging to it resolved (CONTEXT.md Promotion Operation).
/// Drafts are appended in plan order, which is what makes the resulting
/// ascending Mutation Sequence order match the plan's contract order: `append`
/// allocates sequences as it goes.
///
/// An empty plan appends nothing and returns `None` rather than minting an
/// identity that would own zero Mutations. The workflow guard proves the
/// caller owns the Remote workflow while appending behind any unresolved
/// creation; ordered sync remains blocked at that earlier Mutation.
pub fn commit_plan<R: Rng + ?Sized>(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    plan: &PromotionPlan,
    expected_kind: BackendKind,
    rng: &mut R,
    now: &str,
) -> Result<Option<String>, CommitPlanError> {
    if plan.is_empty() {
        return Ok(None);
    }
    let tx = crate::store::write_transaction(conn)?;

    let operation_id = generate_internal_id(rng);
    let actual = crate::store::sync::configured_remote_kind(&tx)?;
    if actual != Some(expected_kind) {
        return Err(CommitPlanError::RemoteChanged {
            expected: expected_kind,
            actual,
        });
    }
    crate::store::sync::ensure_backend_cohort(&tx, expected_kind)?;
    for draft in &plan.mutations {
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type: draft.mutation_type,
                item_id: &draft.item_id,
                item_class: draft.item_class,
                payload: &draft.payload,
                promotion_operation_id: Some(&operation_id),
                now_iso: now,
            },
        )?;
    }
    tx.commit()?;
    Ok(Some(operation_id))
}

/// Error returned by [`apply_receipt`].
#[derive(Debug, Error)]
pub enum ApplyReceiptError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error("Promotion Mutation targets Item {0}, which no longer exists")]
    ItemNotFound(String),
    #[error("Promotion Mutation targets Item {0}, which is not Local")]
    TargetNotLocal(String),
}

/// Convert the Local Item `item_id` into a Backend Item using the identity its
/// Promotion receipt carries: store `backend_kind` / `receipt.backend_key`,
/// replace the Display ID with `receipt.display_id`, and keep the outgoing
/// Display ID resolvable as an Alias (CONTEXT.md Promotion).
///
/// Selection State, Priority, status, title, and body are Local Fields that
/// survive Promotion untouched; leaving `status` out of the `items` update also
/// keeps the `items_no_escape_from_done` trigger (which fires `before update of
/// status`) out of this path.
///
/// A Display ID the receipt names but another Item already claims fails the
/// `item_ids` insert; the error propagates so the caller's transaction rolls
/// back with the Mutation still applicable (ADR-0036 Consequences).
pub fn apply_receipt(
    conn: &Connection,
    item_id: &str,
    backend_kind: &str,
    receipt: &BackendItemIdentity,
    now: &str,
) -> Result<(), ApplyReceiptError> {
    let origin: Option<Origin> = conn
        .query_row(
            "select origin from items where id = ?1",
            params![item_id],
            |r| r.get(0),
        )
        .optional()?;
    match origin {
        Some(Origin::Local) => {}
        Some(Origin::Backend) => return Err(ApplyReceiptError::TargetNotLocal(item_id.into())),
        None => return Err(ApplyReceiptError::ItemNotFound(item_id.into())),
    }
    // Statement order is load-bearing. `item_ids_one_display_per_item` is a
    // plain partial unique index on (item_id) where source = 'display' and is
    // not deferrable, so the outgoing row must be demoted to an Alias *before*
    // the receipt's Display ID is inserted; the reverse order fails with
    // `UNIQUE constraint failed: item_ids.item_id`. The composite foreign key
    // from `items.display_value` into `item_ids` is deferred, which is what
    // tolerates the window where `items` still points at a row that has just
    // become an Alias.
    conn.execute(
        "update item_ids set source = 'alias' where item_id = ?1 and source = 'display'",
        params![item_id],
    )?;
    conn.execute(
        "insert into item_ids(value, source, item_id, created_at) \
         values (?1, 'display', ?2, ?3)",
        params![receipt.display_id, item_id, now],
    )?;
    // The `items` Origin CHECK requires backend_kind and backend_key to be
    // non-null exactly when origin = 'backend', so all three move together.
    conn.execute(
        "update items \
            set origin = ?2, backend_kind = ?3, backend_key = ?4, \
                display_value = ?5, updated_at = ?6 \
          where id = ?1",
        params![
            item_id,
            Origin::Backend.text(),
            backend_kind,
            receipt.backend_key,
            receipt.display_id,
            now,
        ],
    )?;
    Ok(())
}

/// Immutable Store data needed to recover one nonterminal Promotion.
///
/// A value read outside a transaction is only a candidate: every transition
/// re-reads the Promotion under its own write transaction and acts on that
/// row. Fields the command layer does not need stay module-private so the
/// authoritative copy is the one the transition reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPromotion {
    /// Mutation Sequence that fixes recovery ordering.
    pub sequence: i64,
    /// Local Display ID to retain as an Alias when reconciliation succeeds.
    pub outgoing_display_id: String,
    /// Original Promotion snapshot and retained Backend payload.
    pub promotion: Promotion,
    /// Typed Backend kind decoded from the retained Promotion payload.
    pub backend_kind: BackendKind,
    /// Current Ticket Kind from the Repository Store; `None` for Epics.
    pub ticket_kind: Option<TicketKind>,
    /// Mutation state as of this read.
    state: MutationState,
    /// Stable internal Item ID, unaffected by receipt Display ID replacement.
    item_id: String,
    /// Item Class as of this read.
    pub item_class: ItemClass,
    /// Promotion Operation grouping as of this read.
    operation_id: String,
}

/// One pre-sync display mapping candidate captured from the complete Promotion
/// queue, rather than only from the graph of the recovery target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPromotionMapping {
    /// Promotion Mutation Sequence order.
    pub sequence: i64,
    /// Stable internal Item ID used to resolve the post-sync Display ID.
    pub item_id: String,
    /// Display ID before a receipt replaces it with the Backend identity.
    pub outgoing_display_id: String,
    /// Item Class for command rendering and recovery decisions.
    pub item_class: ItemClass,
}

/// Errors returned while locating or changing a recoverable Promotion.
#[derive(Debug, Error)]
pub enum RecoveryPromotionError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error("Item {0} has no recoverable Promotion")]
    NoRecoverablePromotion(String),
    #[error("Item {item_id} has terminal Promotion Mutation {sequence} in state {state}")]
    TerminalPromotion {
        item_id: String,
        sequence: i64,
        state: MutationState,
    },
    #[error("Item {item_id} has multiple nonterminal Promotion Mutations ({first} and {second})")]
    MultipleNonterminalPromotions {
        item_id: String,
        first: i64,
        second: i64,
    },
    #[error("Promotion Mutation {sequence} has malformed payload_json: {source}")]
    MalformedPayload {
        sequence: i64,
        #[source]
        source: serde_json::Error,
    },
    #[error("Promotion Mutation {sequence} has malformed Backend kind '{backend_kind}'")]
    MalformedBackendKind { sequence: i64, backend_kind: String },
    #[error("Promotion Mutation {0} has no Promotion Operation ID")]
    MissingOperationId(i64),
    #[error(
        "Promotion Mutation {sequence} has invalid type/class pair {mutation_type}/{item_class}"
    )]
    WrongMutationShape {
        sequence: i64,
        mutation_type: MutationType,
        item_class: ItemClass,
    },
    #[error("Promotion Mutation {sequence} no longer targets a Local Item")]
    TargetNotLocal { sequence: i64 },
    /// The confirmed Backend identity already belongs to another Item, so
    /// attaching it would break the Repository Store's Backend Item and
    /// Display ID uniqueness. Reported before any write so the operator sees
    /// the collision rather than a SQL constraint.
    #[error("Backend object {display_id} is already tracked by tk")]
    BackendIdentityTaken { display_id: String },
    /// `tk promote retry` exists to resolve an indeterminate creation. A
    /// `failed` Promotion is already retried by ordinary sync, so routing it
    /// through recovery would claim a duplicate-creation risk the operator
    /// does not need to accept (ADR-0037).
    #[error("Promotion Mutation {sequence} is in state {state}, not applying")]
    RetryNotApplying { sequence: i64, state: MutationState },
    #[error("Remote changed while recovering the Promotion; retry the command")]
    RemoteChanged {
        expected: BackendKind,
        actual: Option<BackendKind>,
    },
    #[error(transparent)]
    BackendCohort(#[from] crate::store::sync::BackendCohortError),
    #[error("Mutation {sequence} in state {state} must resolve before this Promotion")]
    EarlierNonterminal { sequence: i64, state: MutationState },
    #[error(transparent)]
    Receipt(#[from] ApplyReceiptError),
    #[error(transparent)]
    Append(#[from] mutations::AppendError),
    /// The Mutation state edge recovery asked for is not in the transition
    /// table. Each transition narrows the row to a nonterminal Promotion first,
    /// so this names a Store-layer contract break.
    #[error(transparent)]
    Transition(#[from] mutations::IllegalTransition),
}

impl From<mutations::TransitionError> for RecoveryPromotionError {
    fn from(error: mutations::TransitionError) -> Self {
        match error {
            mutations::TransitionError::Storage(error) => Self::Storage(error),
            mutations::TransitionError::Illegal(error) => Self::Transition(error),
        }
    }
}

#[derive(Debug)]
struct RecoveryPromotionRow {
    sequence: i64,
    state: MutationState,
    mutation_type: MutationType,
    item_class: ItemClass,
    payload_json: String,
    operation_id: Option<String>,
    display: String,
    ticket_kind: Option<TicketKind>,
}

/// Load the unique nonterminal Promotion targeting `item_id` by stable
/// internal Item ID. Display IDs deliberately remain a command concern because
/// receipt application replaces them with Backend-owned identities.
pub fn recoverable_promotion(
    conn: &Connection,
    item_id: &str,
) -> Result<RecoveryPromotion, RecoveryPromotionError> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.state, m.mutation_type, m.item_class, m.payload_json, \
                m.promotion_operation_id, i.display_value, i.ticket_kind \
           from mutations m join items i on i.id = m.item_id \
          where m.item_id = ?1 and m.mutation_type in ('promote_ticket', 'promote_epic') \
            and m.state in ('pending', 'failed', 'applying') \
          order by m.sequence",
    )?;
    let rows = stmt
        .query_map(params![item_id], |r| {
            Ok(RecoveryPromotionRow {
                sequence: r.get(0)?,
                state: r.get(1)?,
                mutation_type: r.get(2)?,
                item_class: r.get(3)?,
                payload_json: r.get(4)?,
                operation_id: r.get(5)?,
                display: r.get(6)?,
                ticket_kind: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > 1 {
        return Err(RecoveryPromotionError::MultipleNonterminalPromotions {
            item_id: item_id.into(),
            first: rows[0].sequence,
            second: rows[1].sequence,
        });
    }
    let Some(row) = rows.into_iter().next() else {
        let terminal = conn
            .query_row(
                "select sequence, state from mutations where item_id = ?1 \
              and mutation_type in ('promote_ticket', 'promote_epic') order by sequence limit 1",
                params![item_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, MutationState>(1)?)),
            )
            .optional()?;
        return match terminal {
            Some((sequence, state)) => Err(RecoveryPromotionError::TerminalPromotion {
                item_id: item_id.into(),
                sequence,
                state,
            }),
            None => Err(RecoveryPromotionError::NoRecoverablePromotion(
                item_id.into(),
            )),
        };
    };
    if !matches!(
        (row.mutation_type, row.item_class),
        (MutationType::PromoteTicket, ItemClass::Ticket)
            | (MutationType::PromoteEpic, ItemClass::Epic)
    ) {
        return Err(RecoveryPromotionError::WrongMutationShape {
            sequence: row.sequence,
            mutation_type: row.mutation_type,
            item_class: row.item_class,
        });
    }
    let promotion: Promotion = serde_json::from_str(&row.payload_json).map_err(|source| {
        RecoveryPromotionError::MalformedPayload {
            sequence: row.sequence,
            source,
        }
    })?;
    let backend_kind = BackendKind::from_str(&promotion.backend_kind).map_err(|_| {
        RecoveryPromotionError::MalformedBackendKind {
            sequence: row.sequence,
            backend_kind: promotion.backend_kind.clone(),
        }
    })?;
    let operation_id = row
        .operation_id
        .ok_or(RecoveryPromotionError::MissingOperationId(row.sequence))?;
    Ok(RecoveryPromotion {
        sequence: row.sequence,
        state: row.state,
        item_id: item_id.into(),
        outgoing_display_id: row.display,
        item_class: row.item_class,
        promotion,
        backend_kind,
        ticket_kind: row.ticket_kind,
        operation_id,
    })
}

/// Capture every nonterminal Promotion before a recovery workflow runs nested
/// sync, preserving the outgoing Display IDs needed to render old-to-new maps.
pub fn capture_recovery_mappings(
    conn: &Connection,
) -> Result<Vec<RecoveryPromotionMapping>, RecoveryPromotionError> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.item_id, i.display_value, m.item_class \
           from mutations m join items i on i.id = m.item_id \
          where m.mutation_type in ('promote_ticket', 'promote_epic') \
            and m.state in ('pending', 'failed', 'applying') order by m.sequence",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RecoveryPromotionMapping {
                sequence: r.get(0)?,
                item_id: r.get(1)?,
                outgoing_display_id: r.get(2)?,
                item_class: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = BTreeMap::new();
    for mapping in &rows {
        if let Some(first) = seen.insert(mapping.item_id.as_str(), mapping.sequence) {
            return Err(RecoveryPromotionError::MultipleNonterminalPromotions {
                item_id: mapping.item_id.clone(),
                first,
                second: mapping.sequence,
            });
        }
    }
    Ok(rows)
}

/// Reconcile a confirmed Backend identity into the Repository Store atomically.
///
/// Re-reads the Promotion under its own write transaction and refuses unless it
/// is still the nonterminal Promotion `target` named, its Item is still Local,
/// the Remote and the retained Backend cohort still match, no earlier Mutation
/// is nonterminal, and no other Item holds `identity`. Success converts the
/// Local Item into a Backend Item, marks the Promotion applied, and advances the
/// Sync Cursor in that one transaction, so a refusal changes nothing
/// (ADR-0037).
///
/// `force_convergence` additionally appends an update Mutation carrying the
/// Item's current title and body into the same Promotion Operation, so the
/// Backend object durably converges on current local content (ADR-0037). The
/// caller sets it when the candidate's content no longer matched the retained
/// Promotion snapshot and the operator supplied `--force`; it relaxes none of
/// the refusals above.
pub fn reconcile_promotion(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    target: &RecoveryPromotion,
    identity: &BackendItemIdentity,
    force_convergence: bool,
    now: &str,
) -> Result<(), RecoveryPromotionError> {
    let tx = crate::store::write_transaction(conn)?;
    let current = recoverable_promotion(&tx, &target.item_id)?;
    if current.sequence != target.sequence {
        return Err(RecoveryPromotionError::NoRecoverablePromotion(
            target.item_id.clone(),
        ));
    }
    let origin: Origin = tx.query_row(
        "select origin from items where id = ?1",
        params![&target.item_id],
        |r| r.get(0),
    )?;
    if origin != Origin::Local {
        return Err(RecoveryPromotionError::TargetNotLocal {
            sequence: target.sequence,
        });
    }
    let actual = crate::store::sync::configured_remote_kind(&tx)?;
    if actual != Some(current.backend_kind) {
        return Err(RecoveryPromotionError::RemoteChanged {
            expected: current.backend_kind,
            actual,
        });
    }
    crate::store::sync::ensure_backend_cohort(&tx, current.backend_kind)?;
    ensure_no_earlier_nonterminal(&tx, current.sequence)?;
    ensure_identity_unclaimed(&tx, &target.item_id, current.backend_kind, identity)?;
    apply_receipt(
        &tx,
        &target.item_id,
        current.backend_kind.text(),
        identity,
        now,
    )?;
    if force_convergence {
        // Convergence intent is current local title and body (CONTEXT.md
        // Promotion Reconciliation), and Promotion leaves both untouched, so
        // the read is the same either side of the receipt.
        let (title, body): (String, String) = tx.query_row(
            "select title, body from items where id = ?1",
            params![&target.item_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mutation_type = current.item_class.update_mutation_type();
        mutations::append(
            &tx,
            mutations::AppendRequest {
                mutation_type,
                item_id: &current.item_id,
                item_class: current.item_class,
                payload: &MutationPayload::UpdateTitleBody(TitleBody { title, body }),
                promotion_operation_id: Some(&current.operation_id),
                now_iso: now,
            },
        )?;
    }
    mutations::mark_applied(&tx, current.sequence, current.state, now)?;
    tx.commit()?;
    Ok(())
}

/// Refuse recovery that would resolve a Promotion ahead of older unresolved
/// intent, which global Mutation Sequence order forbids (ADR-0037).
///
/// A pure local read, so a command can reject the invocation before spending a
/// Backend round trip. Every transition repeats it inside its own transaction,
/// where the answer is authoritative.
pub fn ensure_no_earlier_nonterminal(
    conn: &Connection,
    sequence: i64,
) -> Result<(), RecoveryPromotionError> {
    let earlier = conn
        .query_row(
            "select sequence, state from mutations where sequence < ?1 \
          and state in ('pending', 'failed', 'applying') order by sequence limit 1",
            params![sequence],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, MutationState>(1)?)),
        )
        .optional()?;
    match earlier {
        Some((sequence, state)) => {
            Err(RecoveryPromotionError::EarlierNonterminal { sequence, state })
        }
        None => Ok(()),
    }
}

/// Refuse a confirmed Backend identity that another Item already holds.
///
/// `apply_receipt` would otherwise hit `items_backend_unique` or the
/// `item_ids.value` primary key and surface a SQL constraint to the operator.
/// A mistyped Backend key and an object already imported by Adopt both land
/// here.
fn ensure_identity_unclaimed(
    conn: &Connection,
    item_id: &str,
    backend_kind: BackendKind,
    identity: &BackendItemIdentity,
) -> Result<(), RecoveryPromotionError> {
    let backend_taken = conn
        .query_row(
            "select 1 from items \
              where backend_kind = ?1 and backend_key = ?2 and id <> ?3",
            params![backend_kind.text(), &identity.backend_key, item_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    // `item_ids.value` is the nocase primary key, so this covers an existing
    // Display ID and a retained Alias alike.
    let display_taken = conn
        .query_row(
            "select 1 from item_ids where value = ?1 and item_id <> ?2",
            params![&identity.display_id, item_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if backend_taken || display_taken {
        return Err(RecoveryPromotionError::BackendIdentityTaken {
            display_id: identity.display_id.clone(),
        });
    }
    Ok(())
}

/// Return an indeterminate Promotion to pending so the normal ordered sync may
/// attempt it again. Retrying never records an identity or advances the cursor.
///
/// The Promotion snapshot replays as committed, never rewritten: it is
/// Promotion Reconciliation's default identity proof for the object the first
/// attempt may have created (ADR-0037). A Promotion the Backend refuses on its
/// content exits through Promotion Cancellation and a fresh Promotion instead
/// (ADR-0039).
pub fn retry_promotion(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    target: &RecoveryPromotion,
    now: &str,
) -> Result<(), RecoveryPromotionError> {
    let tx = crate::store::write_transaction(conn)?;
    let current = recoverable_promotion(&tx, &target.item_id)?;
    if current.sequence != target.sequence {
        return Err(RecoveryPromotionError::NoRecoverablePromotion(
            target.item_id.clone(),
        ));
    }
    ensure_no_earlier_nonterminal(&tx, current.sequence)?;
    match current.state {
        MutationState::Applying => {
            mutations::transition(
                &tx,
                mutations::TransitionRequest {
                    sequence: current.sequence,
                    from: MutationState::Applying,
                    to: MutationState::Pending,
                    failure: None,
                    now,
                },
            )?;
        }
        // Already pending: the command stays idempotent and just resumes sync.
        MutationState::Pending => {}
        state => {
            return Err(RecoveryPromotionError::RetryNotApplying {
                sequence: current.sequence,
                state,
            });
        }
    }
    tx.commit()?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Promotion Cancellation (ADR-0038, ADR-0039)
// ──────────────────────────────────────────────────────────────────────────

/// One Promotion of the operation a cancellation reports on. Which group of the
/// report it lands in says what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedPromotion {
    pub sequence: i64,
    pub display_id: String,
    pub item_class: ItemClass,
}

/// One Mutation the withdrawal took with the Promotions, other than the
/// Promotions themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawnMutation {
    pub sequence: i64,
    pub mutation_type: MutationType,
    pub target_display_id: String,
    /// Whether the target is itself one of the cancelled items. When it is
    /// not, this is intent lost for an object that really exists upstream —
    /// the distinction a report leads with (ADR-0038).
    pub target_cancelled: bool,
}

/// What one Promotion Cancellation withdrew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationReport {
    /// Promotions the withdrawal resolved, in Mutation Sequence order. Their
    /// items are back to Local Backend Binding, and nothing they would have
    /// created exists on the Backend.
    pub cancelled_promotions: Vec<ReportedPromotion>,
    /// Promotions withdrawn while their Backend creation outcome was
    /// indeterminate. Their items are Local too, but a Backend object may
    /// exist that tk holds no identity for (ADR-0039).
    pub abandoned_promotions: Vec<ReportedPromotion>,
    /// Promotions the Backend already accepted. tk never compensates by
    /// deleting a Backend object, so these are reported, not undone.
    pub applied_promotions: Vec<ReportedPromotion>,
    /// Everything else the withdrawal took, in Mutation Sequence order.
    pub withdrawn: Vec<WithdrawnMutation>,
}

/// A Dependency edge a withdrawal would leave unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrepresentableDependency {
    pub blocked_display_id: String,
    pub blocking_display_id: String,
    pub rejection: DependencyRejection,
}

/// Errors returned by [`cancel_promotion`].
#[derive(Debug, Error)]
pub enum CancelPromotionError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    #[error(transparent)]
    Recovery(Box<RecoveryPromotionError>),
    /// The withdrawal would leave a backend-bound Blocked Item waiting on a
    /// Local Blocking Item — the graph ADR-0035 refuses to create by
    /// Promotion, reached from the other direction.
    #[error("{} Dependency edge(s) would become unrepresentable", .0.len())]
    UnrepresentableDependencies(Vec<UnrepresentableDependency>),
    /// Every Promotion of the operation has already resolved, so there is no
    /// untried or certified-rejected intent left to withdraw.
    #[error("Promotion Operation {0} has no Promotion left to withdraw")]
    NothingToWithdraw(String),
    /// A Mutation's `payload_json` did not decode into the counterpart its
    /// Mutation Type addresses. Repository Store corruption.
    #[error("malformed payload_json on mutation {sequence}: {source}")]
    MalformedPayload {
        sequence: i64,
        source: serde_json::Error,
    },
    #[error(transparent)]
    BackendBinding(#[from] mutations::BackendBindingError),
    #[error(transparent)]
    Transition(#[from] mutations::IllegalTransition),
}

impl From<RecoveryPromotionError> for CancelPromotionError {
    fn from(error: RecoveryPromotionError) -> Self {
        Self::Recovery(Box::new(error))
    }
}

impl From<mutations::TransitionError> for CancelPromotionError {
    fn from(error: mutations::TransitionError) -> Self {
        match error {
            mutations::TransitionError::Storage(error) => Self::Storage(error),
            mutations::TransitionError::Illegal(error) => Self::Transition(error),
        }
    }
}

/// Withdraw the Promotion Operation the Promotion of `item_id` belongs to.
///
/// A `pending` or `failed` Promotion is cancelled; an `applying` one is
/// abandoned, because tk recorded no Backend identity for its creation
/// (ADR-0039). Both return their item to Local, so both contribute the item
/// whose collateral is withdrawn alongside them.
///
/// Plans and commits in one transaction under the caller's Remote workflow
/// guard: the whole decision reads Mutation Log state a concurrent sync could
/// be draining, and no part of it reaches a Backend Adapter (ADR-0038). That is
/// also why ADR-0037's "earliest nonterminal Mutation" rule does not apply —
/// it exists to keep *Backend* effects ordered.
///
/// A refusal drops the transaction, so nothing is withdrawn.
pub fn cancel_promotion(
    conn: &mut Connection,
    _workflow: &RemoteWorkflowGuard,
    item_id: &str,
    now: &str,
) -> Result<CancellationReport, CancelPromotionError> {
    let tx = crate::store::write_transaction(conn)?;
    let operation_id = cancellable_operation_id(&tx, item_id)?;
    let promotions = operation_promotions(&tx, &operation_id)?;
    let mut cancelled_promotions = Vec::new();
    let mut abandoned_promotions = Vec::new();
    let mut applied_promotions = Vec::new();
    for promotion in promotions {
        match promotion.state {
            MutationState::Pending | MutationState::Failed => {
                cancelled_promotions.push(promotion);
            }
            MutationState::Applying => abandoned_promotions.push(promotion),
            MutationState::Applied => applied_promotions.push(promotion),
            // A Promotion cannot be `skipped` (the `mutations` CHECK), and a
            // `cancelled` or `abandoned` one is already withdrawn by an earlier
            // invocation.
            MutationState::Skipped | MutationState::Cancelled | MutationState::Abandoned => {}
        }
    }

    if cancelled_promotions.is_empty() && abandoned_promotions.is_empty() {
        return Err(CancelPromotionError::NothingToWithdraw(operation_id));
    }

    // Every withdrawn Promotion's item loses its prospective identity, whatever
    // state the withdrawal reaches, so the Dependency and collateral queries
    // judge against one set.
    let cancelled_items: BTreeSet<String> = cancelled_promotions
        .iter()
        .chain(&abandoned_promotions)
        .map(|promotion| promotion.item_id.clone())
        .collect();
    ensure_dependencies_representable(&tx, &cancelled_items)?;

    for promotion in &cancelled_promotions {
        mutations::transition(
            &tx,
            mutations::TransitionRequest {
                sequence: promotion.sequence,
                from: promotion.state,
                to: MutationState::Cancelled,
                failure: None,
                now,
            },
        )?;
    }
    for promotion in &abandoned_promotions {
        mutations::transition(
            &tx,
            mutations::TransitionRequest {
                sequence: promotion.sequence,
                from: MutationState::Applying,
                to: MutationState::Abandoned,
                failure: None,
                now,
            },
        )?;
    }
    let withdrawn = withdrawn_mutations(&tx, &cancelled_items)?;
    for row in &withdrawn {
        mutations::transition(
            &tx,
            mutations::TransitionRequest {
                sequence: row.sequence,
                from: row.state,
                to: MutationState::Cancelled,
                failure: None,
                now,
            },
        )?;
    }
    tx.commit()?;

    Ok(CancellationReport {
        cancelled_promotions: cancelled_promotions.into_iter().map(Into::into).collect(),
        abandoned_promotions: abandoned_promotions.into_iter().map(Into::into).collect(),
        applied_promotions: applied_promotions.into_iter().map(Into::into).collect(),
        withdrawn: withdrawn
            .into_iter()
            .map(|row| WithdrawnMutation {
                target_cancelled: cancelled_items.contains(&row.item_id),
                sequence: row.sequence,
                mutation_type: row.mutation_type,
                target_display_id: row.display_id,
            })
            .collect(),
    })
}

/// The Promotion Operation `tk promote cancel <item_id>` names.
///
/// Usually the item's own nonterminal Promotion, which also rules out the
/// ambiguity of an item carrying two of them. A resolved Promotion still
/// identifies its operation, though, and has to: cancelling a `--children`
/// Epic whose own Promotion applied while a child's was rejected is exactly
/// the half-applied operation ADR-0038 says the withdrawal still covers, and
/// the Epic is the item the operator typed at promote time. The item's latest
/// Promotion wins, so a re-promoted item resolves to its current operation
/// rather than a withdrawn one.
fn cancellable_operation_id(
    conn: &Connection,
    item_id: &str,
) -> Result<String, CancelPromotionError> {
    match recoverable_promotion(conn, item_id) {
        Ok(promotion) => Ok(promotion.operation_id),
        Err(RecoveryPromotionError::TerminalPromotion { sequence, .. }) => conn
            .query_row(
                "select promotion_operation_id from mutations \
                  where item_id = ?1 and mutation_type in ('promote_ticket', 'promote_epic') \
                  order by sequence desc limit 1",
                params![item_id],
                |r| r.get::<_, Option<String>>(0),
            )?
            .ok_or_else(|| RecoveryPromotionError::MissingOperationId(sequence).into()),
        Err(err) => Err(err.into()),
    }
}

/// One Promotion of the operation being cancelled, with the state and Item
/// identity the decision needs.
#[derive(Debug, Clone)]
struct OperationPromotion {
    sequence: i64,
    state: MutationState,
    item_id: String,
    display_id: String,
    item_class: ItemClass,
}

impl From<OperationPromotion> for ReportedPromotion {
    fn from(promotion: OperationPromotion) -> Self {
        Self {
            sequence: promotion.sequence,
            display_id: promotion.display_id,
            item_class: promotion.item_class,
        }
    }
}

fn operation_promotions(
    conn: &Connection,
    operation_id: &str,
) -> rusqlite::Result<Vec<OperationPromotion>> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.state, m.item_id, i.display_value, m.item_class \
           from mutations m join items i on i.id = m.item_id \
          where m.promotion_operation_id = ?1 \
            and m.mutation_type in ('promote_ticket', 'promote_epic') \
          order by m.sequence asc",
    )?;
    stmt.query_map(params![operation_id], |r| {
        Ok(OperationPromotion {
            sequence: r.get(0)?,
            state: r.get(1)?,
            item_id: r.get(2)?,
            display_id: r.get(3)?,
            item_class: r.get(4)?,
        })
    })?
    .collect()
}

/// One nonterminal Mutation the withdrawal takes with it.
#[derive(Debug, Clone)]
struct WithdrawnRow {
    sequence: i64,
    state: MutationState,
    mutation_type: MutationType,
    item_id: String,
    display_id: String,
}

/// The collateral of a withdrawal: every non-Promotion Mutation that cannot
/// resolve once `cancelled_items` lose their prospective Backend identity, in
/// Mutation Sequence order.
///
/// One hop and never transitive: only the cancelled items lose an identity, so
/// no third item's Mutations become unresolvable (ADR-0038). Only `pending` and
/// `failed` rows can qualify — global Mutation Sequence order holds every later
/// Mutation behind a nonterminal Promotion, so collateral was never attempted.
/// This skips the Promotions themselves: a withdrawal reaches two states,
/// `cancelled` and `abandoned`, and the caller picks which one per Promotion
/// (ADR-0039).
fn withdrawn_mutations(
    conn: &Connection,
    cancelled_items: &BTreeSet<String>,
) -> Result<Vec<WithdrawnRow>, CancelPromotionError> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.state, m.mutation_type, m.item_id, i.display_value, m.payload_json \
           from mutations m join items i on i.id = m.item_id \
          where m.state in ('pending', 'failed') \
            and m.mutation_type not in ('promote_ticket', 'promote_epic') \
          order by m.sequence asc",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                WithdrawnRow {
                    sequence: r.get(0)?,
                    state: r.get(1)?,
                    mutation_type: r.get(2)?,
                    item_id: r.get(3)?,
                    display_id: r.get(4)?,
                },
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for (row, payload_json) in rows {
        let counterpart = addressed_counterpart_id(row.sequence, row.mutation_type, &payload_json)?;
        let withdrawn = cancelled_items.contains(&row.item_id)
            || counterpart.is_some_and(|id| cancelled_items.contains(&id));
        if withdrawn {
            out.push(row);
        }
    }
    Ok(out)
}

/// The internal Item ID of the counterpart `mutation_type` addresses, decoded
/// from the row's payload.
fn addressed_counterpart_id(
    sequence: i64,
    mutation_type: MutationType,
    payload_json: &str,
) -> Result<Option<String>, CancelPromotionError> {
    let malformed = |source| CancelPromotionError::MalformedPayload { sequence, source };
    Ok(match mutation_type.addressed_counterpart() {
        AddressedCounterpart::None => None,
        AddressedCounterpart::Epic => Some(
            serde_json::from_str::<EpicRef>(payload_json)
                .map_err(malformed)?
                .epic_id,
        ),
        AddressedCounterpart::BlockingItem => Some(
            serde_json::from_str::<DependencyRef>(payload_json)
                .map_err(malformed)?
                .blocking_id,
        ),
    })
}

/// Refuse a withdrawal that would leave a Dependency the Backend graph cannot
/// represent.
///
/// Cancellation is the third caller of ADR-0035's Dependency classification,
/// judging each affected edge against the Backend Binding the withdrawal will
/// produce exactly as Promotion preflight judges against the Binding a
/// Promotion will produce. Epic Membership degrades to local instead, as
/// ADR-0035 already decided.
fn ensure_dependencies_representable(
    conn: &Connection,
    cancelled_items: &BTreeSet<String>,
) -> Result<(), CancelPromotionError> {
    let mut stmt = conn.prepare(
        "select blocked_id, blocking_id from dependencies \
          where blocked_id = ?1 or blocking_id = ?1",
    )?;
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    for item_id in cancelled_items {
        for edge in stmt.query_map(params![item_id], |r| Ok((r.get(0)?, r.get(1)?)))? {
            edges.insert(edge?);
        }
    }

    // Every edge here has at least one endpoint returning to Local, and a
    // Backend-kind mismatch needs two bound endpoints, so only
    // `BackendBlockedLocalBlocking` is reachable today. The rejection travels
    // with the edge anyway, so a change to the classification cannot leave
    // this path rendering the wrong remedy.
    let mut rejected = Vec::new();
    for (blocked_id, blocking_id) in edges {
        let blocked = post_cancellation_binding(conn, &blocked_id, cancelled_items)?;
        let blocking = post_cancellation_binding(conn, &blocking_id, cancelled_items)?;
        if let DependencyClassification::Rejected(rejection) =
            dependency_rule::classify(&blocked, &blocking)
        {
            rejected.push(UnrepresentableDependency {
                blocked_display_id: display_id(conn, &blocked_id)?,
                blocking_display_id: display_id(conn, &blocking_id)?,
                rejection,
            });
        }
    }
    if rejected.is_empty() {
        return Ok(());
    }
    Err(CancelPromotionError::UnrepresentableDependencies(rejected))
}

/// The Backend Binding `item_id` will have once the withdrawal commits.
///
/// A cancelled item is Local: its only Promotion is in the withdrawn set. Every
/// other item keeps the Binding it has now, because a Promotion joins the
/// withdrawn set only through its own item.
fn post_cancellation_binding(
    conn: &Connection,
    item_id: &str,
    cancelled_items: &BTreeSet<String>,
) -> Result<BackendBinding, CancelPromotionError> {
    if cancelled_items.contains(item_id) {
        return Ok(BackendBinding::Local);
    }
    Ok(mutations::resolve_backend_binding(conn, item_id)?)
}

fn display_id(conn: &Connection, item_id: &str) -> rusqlite::Result<String> {
    conn.query_row(
        "select display_value from items where id = ?1",
        params![item_id],
        |r| r.get(0),
    )
}

/// A Promotion withdrawn while its Backend creation outcome was unobserved, so
/// a Backend object may exist that tk holds no identity for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedPromotion {
    pub sequence: i64,
    pub display_id: String,
}

/// The latest abandoned Promotion of each of `item_ids`, for the items that
/// have one.
///
/// Asks for the latest *abandonment*, not the latest Promotion: every other
/// state a later Promotion could hold created nothing, so none of them clears
/// the risk this warns about. Only an `applied` Promotion would, and an Item the
/// Backend already accepted is never planned for Promotion again, so it never
/// reaches this query (ADR-0039).
pub fn abandoned_promotions(
    conn: &Connection,
    item_ids: &[String],
) -> rusqlite::Result<Vec<AbandonedPromotion>> {
    let mut stmt = conn.prepare(
        "select m.sequence, i.display_value \
           from mutations m join items i on i.id = m.item_id \
          where m.item_id = ?1 \
            and m.mutation_type in ('promote_ticket', 'promote_epic') \
            and m.state = 'abandoned' \
          order by m.sequence desc limit 1",
    )?;
    let mut out = Vec::new();
    for item_id in item_ids {
        let abandoned = stmt
            .query_row(params![item_id], |r| {
                Ok(AbandonedPromotion {
                    sequence: r.get(0)?,
                    display_id: r.get(1)?,
                })
            })
            .optional()?;
        out.extend(abandoned);
    }
    out.sort_by_key(|promotion| promotion.sequence);
    Ok(out)
}

/// Mutation Log fields needed by Promotion's post-sync report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationSummary {
    pub sequence: i64,
    pub state: MutationState,
    pub target_display_id: String,
}

/// The earliest nonterminal Mutation: the lowest Mutation Sequence in
/// `pending`, `failed`, or `applying` state, across the whole Mutation Log.
///
/// Deliberately not scoped to a Promotion Operation. A Promotion is appended
/// behind whatever the outbox already held, so the Mutation that stops the sync
/// may predate the operation and carry no Promotion Operation at all — an
/// operation-scoped query could never name it. It is also the only place the
/// answer exists when Apply hits an environment failure, which leaves the
/// in-flight row `pending` and writes no outcome.
pub fn earliest_applicable_mutation(
    conn: &Connection,
) -> rusqlite::Result<Option<MutationSummary>> {
    conn.query_row(
        "select m.sequence, m.state, i.display_value \
           from mutations m join items i on i.id = m.item_id \
          where m.state in ('pending','failed','applying') \
          order by m.sequence asc limit 1",
        [],
        |r| {
            Ok(MutationSummary {
                sequence: r.get(0)?,
                state: r.get(1)?,
                target_display_id: r.get(2)?,
            })
        },
    )
    .optional()
}

/// The Mutations of `operation_id` still awaiting an outcome, in Mutation
/// Sequence order.
///
/// An empty result is the success condition for one `tk promote`: overall
/// success requires every Mutation in the requested Promotion Operation to
/// resolve (CONTEXT.md Promotion Operation). Comparing a non-empty result
/// against [`earliest_applicable_mutation`] separates a Promotion queued behind
/// an older Mutation from one of the operation's own Mutations being rejected.
///
/// The question is the nonterminal set, not "not applied": a human-curated
/// terminal omission — Skipped or Cancelled — is a resolved outcome, not a
/// waiting one (ADR-0038).
pub fn unresolved_in_operation(
    conn: &Connection,
    operation_id: &str,
) -> rusqlite::Result<Vec<MutationSummary>> {
    let mut stmt = conn.prepare(
        "select m.sequence, m.state, i.display_value \
           from mutations m join items i on i.id = m.item_id \
          where m.promotion_operation_id = ?1 \
            and m.state in ('pending', 'failed', 'applying') \
          order by m.sequence asc",
    )?;
    let rows = stmt.query_map(params![operation_id], |r| {
        Ok(MutationSummary {
            sequence: r.get(0)?,
            state: r.get(1)?,
            target_display_id: r.get(2)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_binding::BackendBinding;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{MutationPayload, Promotion, TitleBody};
    use crate::domain::mutation_type::MutationType;
    use crate::domain::promotion_plan::MutationDraft;
    use crate::domain::status::ItemStatus;
    use crate::store::migrations;
    use crate::store::repository::resolve_item_ref;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, insert_dependency, insert_fixture_item,
        insert_fixture_mutation, mutation_count,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const NOW: &str = "2026-06-01T00:00:00Z";

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        crate::store::sync::set_remote(&mut conn, BackendKind::Github, "{}", NOW).unwrap();
        conn
    }

    fn local_ticket(conn: &Connection, id: &str, display: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Local work",
                selection_state: Some("parked"),
                priority: Some("P0"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn receipt(backend_key: &str, display_id: &str) -> BackendItemIdentity {
        BackendItemIdentity {
            backend_key: backend_key.into(),
            display_id: display_id.into(),
        }
    }

    /// Drive the receipt through a caller-owned transaction, the shape
    /// [`apply_receipt`] contracts for: the deferred `items` →
    /// `item_ids` foreign key is only tolerated mid-transaction, and a failure
    /// rolls the whole conversion back.
    fn promote(
        conn: &mut Connection,
        item_id: &str,
        receipt: &BackendItemIdentity,
    ) -> Result<(), ApplyReceiptError> {
        let tx = crate::store::write_transaction(conn)?;
        apply_receipt(&tx, item_id, "github", receipt, NOW)?;
        Ok(tx.commit()?)
    }

    #[test]
    fn receipt_makes_the_item_a_backend_item_and_replaces_the_display_id() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (display, origin, kind, key, updated): (String, String, String, String, String) = conn
            .query_row(
                "select display_value, origin, backend_kind, backend_key, updated_at \
                   from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-42");
        assert_eq!(origin, "backend");
        assert_eq!(kind, "github");
        assert_eq!(key, "42");
        assert_eq!(updated, NOW);
    }

    #[test]
    fn the_outgoing_display_id_still_resolves_as_an_alias() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        // Both identifiers reach the same Item; the local one is the Alias.
        assert_eq!(resolve_item_ref(&conn, "tk-1").unwrap().unwrap().id, "t1");
        assert_eq!(resolve_item_ref(&conn, "gh-42").unwrap().unwrap().id, "t1");

        let mut stmt = conn
            .prepare("select value, source from item_ids where item_id = 't1' order by value")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("gh-42".to_string(), "display".to_string()),
                ("tk-1".to_string(), "alias".to_string()),
            ],
            "exactly one display row and one alias row after the swap"
        );
    }

    #[test]
    fn local_fields_survive_promotion() {
        // Selection State and Priority are Local Fields (ADR-0027) that
        // Promotion preserves; status/title/body are not the receipt's to move.
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (selection, priority, status, title): (String, String, String, String) = conn
            .query_row(
                "select selection_state, priority, status, title from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(selection, "parked");
        assert_eq!(priority, "P0");
        assert_eq!(status, "open");
        assert_eq!(title, "Local work");
    }

    #[test]
    fn an_accepted_ticket_stays_accepted_through_promotion() {
        // Both Selection States a Ticket may carry into Promotion survive it
        // (CONTEXT.md Promotion); `parked` is covered above, and `accepted` is
        // the one the default path takes.
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                selection_state: Some("accepted"),
                priority: Some("P2"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap();

        let (selection, priority): (String, String) = conn
            .query_row(
                "select selection_state, priority from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(selection, "accepted");
        assert_eq!(priority, "P2");
    }

    #[test]
    fn a_display_id_another_item_claims_is_refused_and_rolls_back() {
        let mut conn = open_seeded();
        local_ticket(&conn, "t1", "tk-1");
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "gh-42",
                title: "Squatter",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let err = promote(&mut conn, "t1", &receipt("42", "gh-42")).unwrap_err();
        assert!(
            matches!(
                err,
                ApplyReceiptError::Storage(rusqlite::Error::SqliteFailure(e, _))
                    if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                        || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            ),
            "expected item_ids to refuse the already-claimed Display ID, got {err:?}"
        );

        // The demote is part of the same transaction, so the Item is untouched.
        let (display, origin): (String, String) = conn
            .query_row(
                "select display_value, origin from items where id = 't1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(display, "tk-1");
        assert_eq!(origin, "local");
        let source: String = conn
            .query_row(
                "select source from item_ids where value = 'tk-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "display", "the demote rolled back with the insert");
    }

    #[test]
    fn promoting_an_epic_keeps_its_null_ticket_only_columns() {
        let mut conn = open_seeded();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Local epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        promote(&mut conn, "e1", &receipt("7", "gh-7")).unwrap();

        let (display, origin, selection): (String, String, Option<String>) = conn
            .query_row(
                "select display_value, origin, selection_state from items where id = 'e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(display, "gh-7");
        assert_eq!(origin, "backend");
        assert_eq!(selection, None, "Epics stay outside Selection State");
    }

    // ── Preflight graph read ──────────────────────────────────────────────

    fn seed_ticket(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
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

    fn seed_epic(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn seed_child(conn: &Connection, id: &str, display: &str, epic_id: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: "Child",
                container_id: Some(epic_id),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn item_ids(graph: &PromotionGraph) -> Vec<&str> {
        graph.items.iter().map(|i| i.id.as_str()).collect()
    }

    fn edges(graph: &PromotionGraph) -> Vec<(&str, &str)> {
        graph
            .dependencies
            .iter()
            .map(|d| (d.blocked_id.as_str(), d.blocking_id.as_str()))
            .collect()
    }

    #[test]
    fn an_unrelated_target_is_the_whole_graph() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "elsewhere", "tk-2", 2);

        let graph = read_graph(&conn, "t1", false).unwrap();

        assert_eq!(graph.target_id, "t1");
        assert!(!graph.children_requested);
        assert_eq!(item_ids(&graph), vec!["t1"]);
        assert!(graph.dependencies.is_empty());

        let target = &graph.items[0];
        assert_eq!(target.display_id, "tk-1");
        assert_eq!(target.item_class, ItemClass::Ticket);
        assert_eq!(target.status, ItemStatus::Open);
        assert_eq!(target.created_seq, 1);
        assert_eq!(target.container_id, None);
    }

    #[test]
    fn requested_children_join_the_snapshot_whatever_their_origin() {
        // Origin is not filtered here: which contained Tickets are Promotion
        // Children is the planner's call, and it needs to see an already
        // backend-backed sibling to judge the resulting graph.
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "local-child", "tk-2", "epic", 2);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "backend-child",
                display: "gh-9",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                container_id: Some("epic"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_ticket(&conn, "outside", "tk-4", 4);

        let graph = read_graph(&conn, "epic", true).unwrap();

        assert!(graph.children_requested);
        assert_eq!(
            item_ids(&graph),
            vec!["epic", "local-child", "backend-child"]
        );
        assert_eq!(graph.items[1].container_id.as_deref(), Some("epic"));
    }

    #[test]
    fn contained_tickets_are_snapshotted_even_without_children() {
        // Promoting an Epic snapshots membership for the Tickets it already
        // contains on the same Backend, so the snapshot carries them whether
        // or not `--children` was passed; `children_requested` only decides
        // which of them the planner treats as Promotion Children.
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "epic", 2);

        let graph = read_graph(&conn, "epic", false).unwrap();

        assert_eq!(item_ids(&graph), vec!["epic", "child"]);
        assert!(!graph.children_requested);
    }

    #[test]
    fn a_ticket_target_pulls_in_its_containing_epic() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "epic", 2);
        seed_child(&conn, "sibling", "tk-3", "epic", 3);

        let graph = read_graph(&conn, "child", false).unwrap();

        // The Epic joins because Promotion has to decide about membership; the
        // sibling does not — `--children` descends from an Epic target only.
        assert_eq!(item_ids(&graph), vec!["epic", "child"]);
    }

    #[test]
    fn a_dependency_endpoint_outside_the_operation_joins_as_an_item() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "outside", "tk-2", 2);
        insert_dependency(&conn, "outside", "t1").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        // Preflight rejects a backend-backed Blocked Item left waiting on a
        // Local Blocking Item (ADR-0035), which it can only see if the
        // out-of-scope endpoint is in the snapshot.
        assert_eq!(item_ids(&graph), vec!["t1", "outside"]);
        assert_eq!(edges(&graph), vec![("t1", "outside")]);
    }

    #[test]
    fn a_done_blocking_items_edge_is_retained() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "finished",
                display: "tk-2",
                title: "Finished",
                status: "done",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "finished", "t1").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        // A done Blocking Item resolves readiness but keeps its Dependency
        // (ADR-0035), so the resulting backend graph still carries the edge.
        assert_eq!(edges(&graph), vec![("t1", "finished")]);
        assert_eq!(graph.items[1].status, ItemStatus::Done);
    }

    #[test]
    fn edges_are_captured_in_both_directions() {
        let conn = open_seeded();
        seed_ticket(&conn, "blocker", "tk-1", 1);
        seed_ticket(&conn, "t1", "tk-2", 2);
        seed_ticket(&conn, "blocked", "tk-3", 3);
        insert_dependency(&conn, "blocker", "t1").unwrap();
        insert_dependency(&conn, "t1", "blocked").unwrap();

        let graph = read_graph(&conn, "t1", false).unwrap();

        assert_eq!(item_ids(&graph), vec!["blocker", "t1", "blocked"]);
        let mut found = edges(&graph);
        found.sort_unstable();
        assert_eq!(found, vec![("blocked", "t1"), ("t1", "blocker")]);
    }

    #[test]
    fn a_dependency_between_two_children_is_captured_once() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "first", "tk-2", "epic", 2);
        seed_child(&conn, "second", "tk-3", "epic", 3);
        insert_dependency(&conn, "first", "second").unwrap();

        let graph = read_graph(&conn, "epic", true).unwrap();

        assert_eq!(edges(&graph), vec![("second", "first")]);
    }

    #[test]
    fn every_item_carries_its_backend_binding() {
        let conn = open_seeded();
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "plain-local", "tk-2", "epic", 2);
        seed_child(&conn, "pending", "tk-3", "epic", 3);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                mutation_type: "promote_ticket",
                item_id: "pending",
                payload_json: &MutationPayload::Promotion(Promotion {
                    title: "Child".into(),
                    body: String::new(),
                    backend_kind: "github".into(),
                })
                .to_json_string(),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "adopted",
                display: "gh-9",
                title: "Adopted",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("9"),
                container_id: Some("epic"),
                created_seq: 4,
                ..FixtureItem::default()
            },
        )
        .unwrap();

        let graph = read_graph(&conn, "epic", true).unwrap();

        let intents: Vec<&BackendBinding> =
            graph.items.iter().map(|i| &i.backend_binding).collect();
        assert_eq!(
            intents,
            vec![
                &BackendBinding::Local,
                &BackendBinding::Local,
                &BackendBinding::PendingPromotion {
                    backend_kind: "github".into()
                },
                &BackendBinding::Backend {
                    backend_kind: "github".into()
                },
            ]
        );
    }

    // ── Outbox commit ─────────────────────────────────────────────────────

    fn seeded_rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    fn commit_plan_for_test<R: Rng + ?Sized>(
        conn: &mut Connection,
        plan: &PromotionPlan,
        expected_kind: BackendKind,
        rng: &mut R,
        now: &str,
    ) -> Result<Option<String>, CommitPlanError> {
        let workflow = RemoteWorkflowGuard::for_test();
        commit_plan(conn, &workflow, plan, expected_kind, rng, now)
    }

    /// The `mutation_seq` counter's current value. It lives in `sequences`,
    /// not in `mutations`, so a rollback has to return it separately from the
    /// rows an aborted batch wrote.
    fn mutation_seq(conn: &Connection) -> rusqlite::Result<i64> {
        conn.query_row(
            "select value from sequences where name = 'mutation_seq'",
            [],
            |r| r.get(0),
        )
    }

    /// A `promote_ticket` draft naming `item_id`. The payload shape is not
    /// under test here — every draft reuses the same Promotion payload so
    /// the tests can focus on ordering, stamping, and atomicity.
    fn draft(item_id: &str) -> MutationDraft {
        MutationDraft {
            mutation_type: MutationType::PromoteTicket,
            item_id: item_id.to_owned(),
            item_class: ItemClass::Ticket,
            payload: MutationPayload::Promotion(Promotion {
                title: "T".into(),
                body: String::new(),
                backend_kind: "github".into(),
            }),
        }
    }

    fn seed_recovery(
        conn: &Connection,
        id: &str,
        display: &str,
        sequence: i64,
        state: &str,
        item_class: ItemClass,
        operation_id: Option<&str>,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                item_class: item_class.text(),
                ticket_kind: (item_class == ItemClass::Ticket).then_some("task"),
                priority: (item_class == ItemClass::Ticket).then_some("P2"),
                title: "Current title",
                body: "Current body",
                created_seq: sequence,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_promotion_mutation(conn, id, sequence, state, item_class, operation_id);
    }

    /// Append a Promotion Mutation without creating its Item.
    fn seed_promotion_mutation(
        conn: &Connection,
        item_id: &str,
        sequence: i64,
        state: &str,
        item_class: ItemClass,
        operation_id: Option<&str>,
    ) {
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: match item_class {
                    ItemClass::Ticket => "promote_ticket",
                    ItemClass::Epic => "promote_epic",
                },
                item_id,
                item_class: item_class.text(),
                payload_json: r#"{"title":"Original title","body":"Original body","backend_kind":"github"}"#,
                state,
                failure_json: (state == "failed" || state == "applying" || state == "abandoned")
                    .then_some(r#"{"detail":"prior"}"#),
                promotion_operation_id: operation_id,
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        conn.execute(
            "update sequences set value = max(value, ?1) where name = 'mutation_seq'",
            params![sequence],
        )
        .unwrap();
    }

    fn seed_update_mutation(conn: &Connection, state: &str) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id: "backend",
                display: "gh-1",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/1"),
                title: "Backend",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(conn, 1, "backend", state, None);
    }

    #[test]
    fn recoverable_promotion_loads_each_nonterminal_state() {
        for state in ["pending", "failed", "applying"] {
            let conn = open_seeded();
            seed_recovery(
                &conn,
                "t1",
                "tk-1",
                4,
                state,
                ItemClass::Ticket,
                Some("op-1"),
            );
            let target = recoverable_promotion(&conn, "t1").unwrap();
            assert_eq!(target.sequence, 4);
            assert_eq!(target.state.text(), state);
            assert_eq!(target.outgoing_display_id, "tk-1");
            assert_eq!(target.promotion.title, "Original title");
            assert_eq!(target.operation_id, "op-1");
        }
    }

    #[test]
    fn recoverable_promotion_rejects_missing_operation_and_duplicate_rows() {
        let conn = open_seeded();
        seed_recovery(&conn, "t1", "tk-1", 4, "applying", ItemClass::Ticket, None);
        assert!(matches!(
            recoverable_promotion(&conn, "t1"),
            Err(RecoveryPromotionError::MissingOperationId(4))
        ));
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 5,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"","backend_kind":"github"}"#,
                state: "pending",
                promotion_operation_id: Some("op-2"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        assert!(matches!(
            recoverable_promotion(&conn, "t1"),
            Err(RecoveryPromotionError::MultipleNonterminalPromotions {
                first: 4,
                second: 5,
                ..
            })
        ));
    }

    #[test]
    fn reconcile_promotes_atomically_and_forced_convergence_uses_the_operation() {
        for (class, update) in [
            (ItemClass::Ticket, "update_ticket"),
            (ItemClass::Epic, "update_epic"),
        ] {
            let mut conn = open_seeded();
            seed_recovery(&conn, "item", "tk-1", 4, "applying", class, Some("op-1"));
            let target = recoverable_promotion(&conn, "item").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                true,
                NOW,
            )
            .unwrap();
            let values: (String, String, String, String, Option<String>, i64) = conn
                .query_row(
                    "select i.display_value, i.origin, i.backend_key, m.state, m.failure_json, c.last_applied_sequence \
                     from items i join mutations m on m.sequence = 4 \
                     join sync_cursors c on c.remote_name = 'primary' where i.id = 'item'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .unwrap();
            assert_eq!(
                values,
                (
                    "gh-42".into(),
                    "backend".into(),
                    "42".into(),
                    "applied".into(),
                    None,
                    4
                )
            );
            let update_row: (String, String, String, String) = conn.query_row(
                "select mutation_type, payload_json, promotion_operation_id, state from mutations where sequence = 5",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            ).unwrap();
            assert_eq!(update_row.0, update);
            let payload: TitleBody = serde_json::from_str(&update_row.1).unwrap();
            assert_eq!(
                payload,
                TitleBody {
                    title: "Current title".into(),
                    body: "Current body".into(),
                }
            );
            assert_eq!(update_row.2, "op-1");
            assert_eq!(update_row.3, "pending");
        }
    }

    #[test]
    fn exact_reconciliation_does_not_append_convergence() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "item",
            "tk-1",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "item").unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        reconcile_promotion(
            &mut conn,
            &workflow,
            &target,
            &receipt("42", "gh-42"),
            false,
            NOW,
        )
        .unwrap();

        assert_eq!(mutation_count(&conn).unwrap(), 1);
        assert_eq!(mutation_seq(&conn).unwrap(), 4);
    }

    #[test]
    fn reconcile_refuses_an_earlier_nonterminal_but_allows_terminal_history() {
        for state in ["pending", "failed", "applying"] {
            let mut conn = open_seeded();
            seed_recovery(
                &conn,
                "earlier",
                "tk-1",
                1,
                state,
                ItemClass::Ticket,
                Some("old"),
            );
            seed_recovery(
                &conn,
                "target",
                "tk-2",
                4,
                "applying",
                ItemClass::Ticket,
                Some("op-1"),
            );
            let target = recoverable_promotion(&conn, "target").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();
            assert!(matches!(
                reconcile_promotion(
                    &mut conn,
                    &workflow,
                    &target,
                    &receipt("42", "gh-42"),
                    false,
                    NOW
                ),
                Err(RecoveryPromotionError::EarlierNonterminal { sequence: 1, .. })
            ));
        }
        // `skipped` is absent because the `mutations` CHECK forbids it for a
        // Promotion; cancellation is a Promotion's terminal omission.
        for state in ["applied", "cancelled"] {
            let mut conn = open_seeded();
            seed_recovery(
                &conn,
                "earlier",
                "tk-1",
                1,
                state,
                ItemClass::Ticket,
                Some("old"),
            );
            seed_recovery(
                &conn,
                "target",
                "tk-2",
                4,
                "applying",
                ItemClass::Ticket,
                Some("op-1"),
            );
            let target = recoverable_promotion(&conn, "target").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                false,
                NOW,
            )
            .unwrap();
        }
    }

    #[test]
    fn reconcile_cannot_jump_an_earlier_nonpromotion_mutation() {
        let mut conn = open_seeded();
        seed_update_mutation(&conn, "pending");
        seed_recovery(
            &conn,
            "target",
            "tk-2",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("https://github.com/o/r/issues/42", "gh-42"),
                false,
                NOW,
            ),
            Err(RecoveryPromotionError::EarlierNonterminal {
                sequence: 1,
                state: MutationState::Pending,
            })
        ));
        let unchanged: (String, String, Option<String>, String, i64) = conn
            .query_row(
                "select i.display_value, i.origin, i.backend_key, m.state, c.last_applied_sequence \
                   from items i join mutations m on m.item_id = i.id \
                   join sync_cursors c on c.remote_name = 'primary' \
                  where i.id = 'target' and m.sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            unchanged,
            ("tk-2".into(), "local".into(), None, "applying".into(), 0)
        );
    }

    #[test]
    fn retry_returns_a_target_to_pending_without_moving_the_cursor() {
        // `pending` is the idempotent re-run; `applying` is the state the
        // command exists for.
        for state in ["pending", "applying"] {
            let mut conn = open_seeded();
            seed_recovery(
                &conn,
                "target",
                "tk-1",
                4,
                state,
                ItemClass::Ticket,
                Some("op-1"),
            );
            let target = recoverable_promotion(&conn, "target").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();
            retry_promotion(&mut conn, &workflow, &target, NOW).unwrap();
            let row: (String, Option<String>, i64) = conn.query_row(
                "select m.state, m.failure_json, c.last_applied_sequence from mutations m join sync_cursors c on c.remote_name = 'primary' where m.sequence = 4",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).unwrap();
            assert_eq!(row, ("pending".into(), None, 0));
        }
    }

    /// Retry carries duplicate-creation risk, so it is reserved for an
    /// indeterminate outcome. A rejected Promotion has certified no effect and
    /// is already retried by ordinary sync (ADR-0037).
    #[test]
    fn retry_refuses_a_failed_promotion() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "target",
            "tk-1",
            4,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            retry_promotion(&mut conn, &workflow, &target, NOW),
            Err(RecoveryPromotionError::RetryNotApplying {
                sequence: 4,
                state: MutationState::Failed,
            })
        ));
        let row: (String, Option<String>) = conn
            .query_row(
                "select state, failure_json from mutations where sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("failed".into(), Some(r#"{"detail":"prior"}"#.into())),
            "the refusal leaves the recorded Mutation Failure intact"
        );
    }

    #[test]
    fn retry_refuses_every_earlier_nonterminal_state() {
        for state in ["pending", "failed", "applying"] {
            let mut conn = open_seeded();
            seed_recovery(
                &conn,
                "earlier",
                "tk-1",
                1,
                state,
                ItemClass::Ticket,
                Some("op-1"),
            );
            seed_recovery(
                &conn,
                "target",
                "tk-2",
                4,
                "applying",
                ItemClass::Ticket,
                Some("op-2"),
            );
            let target = recoverable_promotion(&conn, "target").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();

            assert!(matches!(
                retry_promotion(&mut conn, &workflow, &target, NOW),
                Err(RecoveryPromotionError::EarlierNonterminal {
                    sequence: 1,
                    state: actual
                }) if actual.text() == state
            ));
            let target_state: String = conn
                .query_row("select state from mutations where sequence = 4", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(target_state, "applying");
        }
    }

    #[test]
    fn retry_cannot_jump_an_earlier_nonpromotion_mutation() {
        let mut conn = open_seeded();
        seed_update_mutation(&conn, "failed");
        seed_recovery(
            &conn,
            "target",
            "tk-2",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            retry_promotion(&mut conn, &workflow, &target, NOW),
            Err(RecoveryPromotionError::EarlierNonterminal {
                sequence: 1,
                state: MutationState::Failed,
            })
        ));
        let unchanged: (String, i64) = conn
            .query_row(
                "select m.state, c.last_applied_sequence from mutations m \
                   join sync_cursors c on c.remote_name = 'primary' \
                  where m.sequence = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(unchanged, ("applying".into(), 0));
    }

    #[test]
    fn recovery_capture_and_retry_preserve_queue_identity() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "first",
            "tk-1",
            2,
            "pending",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "second",
            "tk-2",
            4,
            "failed",
            ItemClass::Ticket,
            Some("op-2"),
        );
        assert_eq!(
            capture_recovery_mappings(&conn).unwrap(),
            vec![
                RecoveryPromotionMapping {
                    sequence: 2,
                    item_id: "first".into(),
                    outgoing_display_id: "tk-1".into(),
                    item_class: ItemClass::Ticket
                },
                RecoveryPromotionMapping {
                    sequence: 4,
                    item_id: "second".into(),
                    outgoing_display_id: "tk-2".into(),
                    item_class: ItemClass::Ticket
                },
            ]
        );
        conn.execute(
            "update mutations set state = 'applying' where sequence = 2",
            [],
        )
        .unwrap();
        let target = recoverable_promotion(&conn, "second").unwrap();
        let workflow = RemoteWorkflowGuard::for_test();
        assert!(matches!(
            retry_promotion(&mut conn, &workflow, &target, NOW),
            Err(RecoveryPromotionError::EarlierNonterminal {
                sequence: 2,
                state: MutationState::Applying
            })
        ));
    }

    #[test]
    fn reconcile_rolls_back_on_remote_change_or_display_collision() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "target",
            "tk-1",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        conn.execute("update remotes set backend_kind = 'jira'", [])
            .unwrap();
        let workflow = RemoteWorkflowGuard::for_test();
        assert!(matches!(
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                false,
                NOW
            ),
            Err(RecoveryPromotionError::RemoteChanged { .. })
        ));
        assert_eq!(
            conn.query_row("select state from mutations where sequence = 4", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "applying"
        );

        conn.execute("update remotes set backend_kind = 'github'", [])
            .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "squatter",
                display: "gh-42",
                title: "taken",
                created_seq: 5,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        assert!(matches!(
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                false,
                NOW
            ),
            // A claimed identity is diagnosed before any write, so the
            // operator never sees the underlying uniqueness constraint.
            Err(RecoveryPromotionError::BackendIdentityTaken { ref display_id })
                if display_id == "gh-42"
        ));
        let row: (String, String) = conn.query_row(
            "select i.origin, m.state from items i join mutations m on m.sequence = 4 where i.id = 'target'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(row, ("local".into(), "applying".into()));
    }

    #[test]
    fn reconcile_refuses_an_identity_another_item_already_holds() {
        for (id, display, backend_key) in [
            // Adopt imported the same object under its own Display ID.
            ("adopted", "gh-9", Some("https://github.com/o/r/issues/42")),
            // A different object already owns the incoming Display ID.
            ("other", "gh-42", Some("https://github.com/o/r/issues/9")),
        ] {
            let mut conn = open_seeded();
            seed_recovery(
                &conn,
                "target",
                "tk-1",
                4,
                "applying",
                ItemClass::Ticket,
                Some("op-1"),
            );
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title: "Already tracked",
                    origin: "backend",
                    backend_kind: Some("github"),
                    backend_key,
                    created_seq: 5,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            let target = recoverable_promotion(&conn, "target").unwrap();
            let workflow = RemoteWorkflowGuard::for_test();

            let err = reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("https://github.com/o/r/issues/42", "gh-42"),
                // Force cannot buy past a claimed identity.
                true,
                NOW,
            )
            .unwrap_err();

            assert!(
                matches!(
                    err,
                    RecoveryPromotionError::BackendIdentityTaken { ref display_id }
                        if display_id == "gh-42"
                ),
                "{id} should claim the identity, got {err:?}"
            );
            let state: String = conn
                .query_row("select state from mutations where sequence = 4", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(state, "applying", "{id}: the Promotion is untouched");
        }
    }

    #[test]
    fn reconcile_refuses_a_target_that_is_no_longer_local() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "target",
            "tk-1",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        conn.execute(
            "update items set origin = 'backend', backend_kind = 'github', \
                    backend_key = 'existing' where id = 'target'",
            [],
        )
        .unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                false,
                NOW,
            ),
            Err(RecoveryPromotionError::TargetNotLocal { sequence: 4 })
        ));
        let state: String = conn
            .query_row("select state from mutations where sequence = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "applying");
    }

    #[test]
    fn forced_reconciliation_rolls_back_when_convergence_cannot_append() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "target",
            "tk-1",
            4,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        let target = recoverable_promotion(&conn, "target").unwrap();
        conn.execute("delete from sequences where name = 'mutation_seq'", [])
            .unwrap();
        let workflow = RemoteWorkflowGuard::for_test();

        assert!(matches!(
            reconcile_promotion(
                &mut conn,
                &workflow,
                &target,
                &receipt("42", "gh-42"),
                true,
                NOW,
            ),
            Err(RecoveryPromotionError::Append(
                mutations::AppendError::Sequence(_)
            ))
        ));
        let row: (String, String, String, i64, i64) = conn
            .query_row(
                "select i.display_value, i.origin, m.state, \
                        c.last_applied_sequence, \
                        (select count(*) from item_ids \
                          where item_id = 'target' and source = 'alias') \
                   from items i join mutations m on m.sequence = 4 \
                   join sync_cursors c on c.remote_name = 'primary' \
                  where i.id = 'target'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("tk-1".into(), "local".into(), "applying".into(), 0, 0)
        );
        assert_eq!(mutation_count(&conn).unwrap(), 1);
    }

    #[test]
    fn commit_promotion_plan_on_an_empty_plan_mints_no_operation() {
        let mut conn = open_seeded();

        let result = commit_plan_for_test(
            &mut conn,
            &PromotionPlan::default(),
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap();

        assert_eq!(
            result, None,
            "re-invoking promote on already-resolved work must not mint an operation that owns no Mutations"
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn commit_promotion_plan_refuses_when_the_remote_changed_after_planning() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        let plan = PromotionPlan {
            mutations: vec![draft("t1")],
        };
        crate::store::sync::clear_remote(&mut conn).unwrap();

        let error = commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CommitPlanError::RemoteChanged {
                expected: BackendKind::Github,
                actual: None,
            }
        ));
        assert_eq!(mutation_count(&conn).unwrap(), 0);
        assert_eq!(mutation_seq(&conn).unwrap(), 0);
    }

    #[test]
    fn commit_promotion_plan_refuses_a_different_retained_backend_kind() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"","backend_kind":"jira"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let plan = PromotionPlan {
            mutations: vec![draft("t2")],
        };

        let error = commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CommitPlanError::BackendCohort(
                crate::store::sync::BackendCohortError::BackendKindMismatch {
                    expected: BackendKind::Github,
                    retained: BackendKind::Jira,
                }
            )
        ));
        assert_eq!(mutation_count(&conn).unwrap(), 1);
        assert_eq!(mutation_seq(&conn).unwrap(), 0);
    }

    #[test]
    fn commit_promotion_plan_appends_drafts_in_plan_order() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_ticket(&conn, "t3", "tk-3", 3);
        // Deliberately out of created_seq order: the outbox must follow the
        // plan's order, not the Items' creation order.
        let plan = PromotionPlan {
            mutations: vec![draft("t3"), draft("t1"), draft("t2")],
        };

        commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap();

        let mut stmt = conn
            .prepare("select item_id from mutations order by sequence")
            .unwrap();
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec!["t3", "t1", "t2"]);
    }

    #[test]
    fn commit_promotion_plan_can_queue_behind_an_applying_creation() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 1,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"","backend_kind":"github"}"#,
                state: "applying",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        conn.execute(
            "update sequences set value = 1 where name = 'mutation_seq'",
            [],
        )
        .unwrap();

        let operation_id = commit_plan_for_test(
            &mut conn,
            &PromotionPlan {
                mutations: vec![draft("t2")],
            },
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap()
        .unwrap();

        let rows: Vec<(i64, String, String, String, Option<String>)> = conn
            .prepare(
                "select sequence, item_id, state, mutation_type, promotion_operation_id \
                   from mutations order by sequence",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    1,
                    "t1".into(),
                    "applying".into(),
                    "promote_ticket".into(),
                    Some("op-1".into()),
                ),
                (
                    2,
                    "t2".into(),
                    "pending".into(),
                    "promote_ticket".into(),
                    Some(operation_id),
                ),
            ]
        );
    }

    #[test]
    fn commit_promotion_plan_stamps_every_mutation_with_the_returned_operation() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        let plan = PromotionPlan {
            mutations: vec![draft("t1"), draft("t2")],
        };

        let operation_id = commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap()
        .expect("a non-empty plan mints an operation");

        let mut stmt = conn
            .prepare("select promotion_operation_id from mutations order by sequence")
            .unwrap();
        let stamped: Vec<Option<String>> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            stamped,
            vec![Some(operation_id.clone()), Some(operation_id)]
        );
    }

    #[test]
    fn commit_promotion_plan_rolls_back_the_whole_batch_on_a_mid_batch_failure() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        // "missing" names no `items` row, so its append violates the
        // `mutations` -> `items` composite foreign key after "t1" has
        // already been inserted earlier in the same transaction.
        let plan = PromotionPlan {
            mutations: vec![draft("t1"), draft("missing")],
        };

        let before = mutation_seq(&conn).unwrap();
        let err = commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap_err();

        assert!(
            matches!(
                err,
                CommitPlanError::Append(mutations::AppendError::Sqlite(_))
            ),
            "expected a foreign key violation on the second draft, got {err:?}"
        );
        assert_eq!(
            mutation_count(&conn).unwrap(),
            0,
            "the draft that appended before the failure must not survive the rollback"
        );
        // The first draft appended before the second failed, consuming a
        // Mutation Sequence from a different table than the row it wrote. An
        // empty log alone does not prove that allocation came back; without it
        // the next `tk promote` would open at a gap.
        assert_eq!(
            mutation_seq(&conn).unwrap(),
            before,
            "the rollback must return the Mutation Sequence counter too"
        );
    }

    #[test]
    fn commit_promotion_plan_leaves_pre_existing_mutations_undisturbed() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "existing", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);

        // Seed a Mutation the way earlier tk activity would have: through the
        // production outbox writer, consuming a real `mutation_seq` value
        // before the plan commit ever runs.
        let seed_tx = conn.unchecked_transaction().unwrap();
        mutations::append(
            &seed_tx,
            mutations::AppendRequest {
                mutation_type: MutationType::UpdateTicket,
                item_id: "existing",
                item_class: ItemClass::Ticket,
                payload: &MutationPayload::UpdateTitleBody(TitleBody {
                    title: "Prior".into(),
                    body: String::new(),
                }),
                promotion_operation_id: None,
                now_iso: NOW,
            },
        )
        .unwrap();
        seed_tx.commit().unwrap();

        let plan = PromotionPlan {
            mutations: vec![draft("t2")],
        };
        commit_plan_for_test(
            &mut conn,
            &plan,
            BackendKind::Github,
            &mut seeded_rng(),
            NOW,
        )
        .unwrap();

        let (sequence, promotion_operation_id, mutation_type): (i64, Option<String>, String) = conn
            .query_row(
                "select sequence, promotion_operation_id, mutation_type \
                   from mutations where item_id = 'existing'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            sequence, 1,
            "the pre-existing row keeps its original sequence"
        );
        assert_eq!(promotion_operation_id, None);
        assert_eq!(mutation_type, "update_ticket");

        let new_sequence: i64 = conn
            .query_row(
                "select sequence from mutations where item_id = 't2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            new_sequence, 2,
            "the new append continues the counter rather than restarting it"
        );
    }

    // ── Post-sync operation state ─────────────────────────────────────────

    /// Seed one Mutation Log row directly, so a test can place states and
    /// Promotion Operations the outbox writer alone could not produce.
    fn seed_mutation(
        conn: &Connection,
        sequence: i64,
        item_id: &str,
        state: &str,
        op: Option<&str>,
    ) {
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: "update_ticket",
                item_id,
                payload_json: r#"{"title":"T","body":""}"#,
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"boom"}"#),
                promotion_operation_id: op,
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn no_applicable_mutation_when_the_log_is_drained() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", None);
        seed_mutation(&conn, 2, "t1", "skipped", None);

        assert_eq!(earliest_applicable_mutation(&conn).unwrap(), None);
    }

    #[test]
    fn the_earliest_applicable_mutation_may_predate_the_promotion_operation() {
        // The whole reason this read is not operation-scoped: the row that
        // stops the sync carries no Promotion Operation.
        let conn = open_seeded();
        seed_ticket(&conn, "older", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_mutation(&conn, 1, "older", "failed", None);
        seed_mutation(&conn, 2, "t2", "pending", Some("op-1"));

        assert_eq!(
            earliest_applicable_mutation(&conn).unwrap(),
            Some(MutationSummary {
                sequence: 1,
                state: MutationState::Failed,
                target_display_id: "tk-1".to_owned(),
            })
        );
    }

    #[test]
    fn an_applying_mutation_is_the_earliest_applicable_blocker() {
        let conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            3,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );

        assert_eq!(
            earliest_applicable_mutation(&conn).unwrap(),
            Some(MutationSummary {
                sequence: 3,
                state: MutationState::Applying,
                target_display_id: "tk-1".into(),
            })
        );
    }

    #[test]
    fn an_applied_row_ahead_of_a_pending_one_is_not_the_blocker() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", Some("op-1"));
        seed_mutation(&conn, 2, "t1", "pending", Some("op-1"));

        assert_eq!(
            earliest_applicable_mutation(&conn)
                .unwrap()
                .unwrap()
                .sequence,
            2
        );
    }

    #[test]
    fn a_fully_applied_operation_has_no_unresolved_mutations() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_mutation(&conn, 1, "t1", "applied", Some("op-1"));
        seed_mutation(&conn, 2, "t1", "applied", Some("op-1"));

        assert!(
            unresolved_in_operation(&conn, "op-1").unwrap().is_empty(),
            "an operation whose every Mutation applied is resolved"
        );
    }

    #[test]
    fn unresolved_operation_mutations_ignore_other_operations() {
        let conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);
        seed_ticket(&conn, "t2", "tk-2", 2);
        seed_mutation(&conn, 1, "t1", "pending", None);
        seed_mutation(&conn, 2, "t2", "failed", Some("other"));
        seed_mutation(&conn, 3, "t1", "failed", Some("op-1"));
        seed_mutation(&conn, 4, "t1", "pending", Some("op-1"));

        assert_eq!(
            unresolved_in_operation(&conn, "op-1").unwrap(),
            vec![
                MutationSummary {
                    sequence: 3,
                    state: MutationState::Failed,
                    target_display_id: "tk-1".to_owned(),
                },
                MutationSummary {
                    sequence: 4,
                    state: MutationState::Pending,
                    target_display_id: "tk-1".to_owned(),
                },
            ]
        );
    }

    // ── Promotion Cancellation (ADR-0038) ─────────────────────────────────

    /// Seed one Mutation whose payload names a counterpart, so the withdrawn
    /// set can be exercised on the roles it is derived from.
    fn seed_counterpart_mutation(
        conn: &Connection,
        sequence: i64,
        mutation_type: MutationType,
        item_id: &str,
        counterpart_id: &str,
        op: Option<&str>,
    ) {
        // `remove_ticket_from_epic` addresses no counterpart yet still carries
        // an `EpicRef`, which is exactly the distinction under test.
        let payload = match mutation_type.addressed_counterpart() {
            AddressedCounterpart::BlockingItem => MutationPayload::DependencyRef(DependencyRef {
                blocking_id: counterpart_id.into(),
            }),
            AddressedCounterpart::Epic | AddressedCounterpart::None => {
                MutationPayload::EpicRef(EpicRef {
                    epic_id: counterpart_id.into(),
                })
            }
        }
        .to_json_string();
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type: mutation_type.text(),
                item_id,
                payload_json: &payload,
                promotion_operation_id: op,
                ..FixtureMutation::default()
            },
        )
        .unwrap();
    }

    fn state_of(conn: &Connection, sequence: i64) -> MutationState {
        conn.query_row(
            "select state from mutations where sequence = ?1",
            params![sequence],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn cancel(
        conn: &mut Connection,
        item_id: &str,
    ) -> Result<CancellationReport, CancelPromotionError> {
        let workflow = RemoteWorkflowGuard::for_test();
        cancel_promotion(conn, &workflow, item_id, NOW)
    }

    #[test]
    fn cancelling_withdraws_the_whole_operation_not_one_promotion() {
        // The unit is the invocation: a regretted `--children` Epic Promotion
        // must not leave its children to be created as bare Backend Tickets.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "e1",
            "tk-1",
            1,
            "failed",
            ItemClass::Epic,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "c1",
            "tk-2",
            2,
            "pending",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let report = cancel(&mut conn, "e1").unwrap();

        assert_eq!(
            report
                .cancelled_promotions
                .iter()
                .map(|p| p.display_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tk-1", "tk-2"]
        );
        assert_eq!(state_of(&conn, 1), MutationState::Cancelled);
        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
    }

    #[test]
    fn a_cancelled_item_returns_to_local_backend_binding() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );

        cancel(&mut conn, "t1").unwrap();

        assert_eq!(
            mutations::resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Local
        );
    }

    #[test]
    fn a_withdrawal_keeps_the_failure_evidence_that_motivated_it() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );

        cancel(&mut conn, "t1").unwrap();

        let failure: Option<String> = conn
            .query_row(
                "select failure_json from mutations where sequence = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failure.as_deref(), Some(r#"{"detail":"prior"}"#));
    }

    #[test]
    fn adding_membership_to_a_cancelled_epic_is_withdrawn_and_removing_it_is_not() {
        // The withdrawn set's defining asymmetry: a Backend Ticket's addition
        // needs the Epic's address, its removal clears a 0..1 slot.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "e1",
            "tk-1",
            1,
            "failed",
            ItemClass::Epic,
            Some("op-1"),
        );
        seed_ticket(&conn, "backend", "gh-9", 2);
        conn.execute(
            "update items set origin = 'backend', backend_kind = 'github', backend_key = '9' \
               where id = 'backend'",
            [],
        )
        .unwrap();
        seed_counterpart_mutation(
            &conn,
            2,
            MutationType::AddTicketToEpic,
            "backend",
            "e1",
            None,
        );
        seed_counterpart_mutation(
            &conn,
            3,
            MutationType::RemoveTicketFromEpic,
            "backend",
            "e1",
            None,
        );

        let report = cancel(&mut conn, "e1").unwrap();

        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
        assert_eq!(
            state_of(&conn, 3),
            MutationState::Pending,
            "clearing Epic Membership resolves without the Epic's address"
        );
        assert_eq!(
            report
                .withdrawn
                .iter()
                .map(|m| (m.sequence, m.target_display_id.as_str(), m.target_cancelled))
                .collect::<Vec<_>>(),
            vec![(2, "gh-9", false)],
            "intent lost for an object that really exists upstream is enumerated"
        );
    }

    #[test]
    fn a_dependency_naming_a_cancelled_blocking_item_is_withdrawn() {
        // `remove_dependency`, because a live `add_dependency` still has its
        // `dependencies` row and so meets the Dependency refusal instead.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "t2",
            "tk-2",
            2,
            "pending",
            ItemClass::Ticket,
            Some("op-2"),
        );
        seed_counterpart_mutation(
            &conn,
            3,
            MutationType::RemoveDependency,
            "t2",
            "t1",
            Some("op-2"),
        );

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 3), MutationState::Cancelled);
        assert_eq!(
            state_of(&conn, 2),
            MutationState::Pending,
            "cancellation is one hop: another item's Promotion survives"
        );
        assert_eq!(
            report
                .withdrawn
                .iter()
                .map(|m| (m.sequence, m.target_cancelled))
                .collect::<Vec<_>>(),
            vec![(3, false)]
        );
    }

    #[test]
    fn a_mutation_targeting_a_cancelled_item_is_counted_not_enumerated() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "pending",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_mutation(&conn, 2, "t1", "pending", Some("op-1"));

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
        assert_eq!(
            report
                .withdrawn
                .iter()
                .map(|m| (m.sequence, m.target_cancelled))
                .collect::<Vec<_>>(),
            vec![(2, true)]
        );
    }

    #[test]
    fn an_applying_promotion_is_abandoned_alongside_the_operations_cancellations() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "t2",
            "tk-2",
            2,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 1), MutationState::Cancelled);
        assert_eq!(
            state_of(&conn, 2),
            MutationState::Abandoned,
            "an unobserved creation is withdrawn into its own state"
        );
        assert_eq!(
            report
                .cancelled_promotions
                .iter()
                .map(|p| p.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            report
                .abandoned_promotions
                .iter()
                .map(|p| p.sequence)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn abandoning_the_only_promotion_returns_its_item_to_local() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 1), MutationState::Abandoned);
        assert!(report.cancelled_promotions.is_empty());
        assert_eq!(report.abandoned_promotions.len(), 1);
        assert_eq!(
            mutations::resolve_backend_binding(&conn, "t1").unwrap(),
            BackendBinding::Local,
            "abandoned is terminal, so the item is no longer Pending Promotion"
        );
    }

    #[test]
    fn a_later_promotion_warns_about_the_latest_abandonment() {
        let conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "abandoned",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_promotion_mutation(&conn, "t1", 2, "abandoned", ItemClass::Ticket, Some("op-2"));
        seed_recovery(
            &conn,
            "t2",
            "tk-2",
            3,
            "failed",
            ItemClass::Ticket,
            Some("op-3"),
        );

        let warned = abandoned_promotions(&conn, &["t1".to_owned(), "t2".to_owned()]).unwrap();

        assert_eq!(
            warned,
            vec![AbandonedPromotion {
                sequence: 2,
                display_id: "tk-1".to_owned(),
            }],
            "the newest unresolved risk is the one to name; a failed Promotion created nothing"
        );
    }

    #[test]
    fn a_later_cancelled_promotion_does_not_clear_an_abandonment() {
        // A cancelled Promotion created nothing, so it cannot clear an earlier
        // withdrawal whose object may still exist.
        let conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "abandoned",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_promotion_mutation(&conn, "t1", 2, "cancelled", ItemClass::Ticket, Some("op-2"));

        let warned = abandoned_promotions(&conn, &["t1".to_owned()]).unwrap();

        assert_eq!(
            warned,
            vec![AbandonedPromotion {
                sequence: 1,
                display_id: "tk-1".to_owned(),
            }],
            "the risk from Mutation 1 is still live"
        );
    }

    #[test]
    fn abandoning_a_promotion_withdraws_the_mutations_queued_behind_it() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "applying",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_mutation(&conn, 2, "t1", "pending", Some("op-1"));

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(
            state_of(&conn, 2),
            MutationState::Cancelled,
            "collateral was never attempted, so it is cancelled rather than abandoned"
        );
        assert_eq!(
            report
                .withdrawn
                .iter()
                .map(|m| (m.sequence, m.target_cancelled))
                .collect::<Vec<_>>(),
            vec![(2, true)]
        );
    }

    #[test]
    fn an_applied_promotion_is_reported_rather_than_undone() {
        // tk never compensates by deleting a Backend object.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "e1",
            "tk-1",
            1,
            "applied",
            ItemClass::Epic,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "c1",
            "tk-2",
            2,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let report = cancel(&mut conn, "c1").unwrap();

        assert_eq!(
            report
                .applied_promotions
                .iter()
                .map(|p| p.display_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tk-1"]
        );
        assert_eq!(state_of(&conn, 1), MutationState::Applied);
        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
    }

    #[test]
    fn a_backend_bound_blocked_item_may_not_be_left_waiting_on_a_cancelled_one() {
        // The graph ADR-0035 refuses to create by Promotion, reached from the
        // other direction: cancellation refuses it too.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_ticket(&conn, "backend", "gh-9", 2);
        conn.execute(
            "update items set origin = 'backend', backend_kind = 'github', backend_key = '9' \
               where id = 'backend'",
            [],
        )
        .unwrap();
        insert_dependency(&conn, "t1", "backend").unwrap();

        let err = cancel(&mut conn, "t1").unwrap_err();

        let CancelPromotionError::UnrepresentableDependencies(edges) = err else {
            panic!("expected a Dependency refusal, got {err:?}");
        };
        assert_eq!(
            edges,
            vec![UnrepresentableDependency {
                blocked_display_id: "gh-9".into(),
                blocking_display_id: "tk-1".into(),
                rejection: DependencyRejection::BackendBlockedLocalBlocking,
            }]
        );
        assert_eq!(state_of(&conn, 1), MutationState::Failed);
    }

    #[test]
    fn cancelling_both_endpoints_leaves_the_edge_local() {
        // An operation-wide withdrawal is self-consistent: neither endpoint
        // keeps a Backend identity, so the edge simply stays local.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "t2",
            "tk-2",
            2,
            "pending",
            ItemClass::Ticket,
            Some("op-1"),
        );
        insert_dependency(&conn, "t1", "t2").unwrap();

        cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 1), MutationState::Cancelled);
        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
    }

    #[test]
    fn cancellation_ignores_an_earlier_nonterminal_mutation() {
        // ADR-0037's ordering rule exists to keep Backend effects ordered, and
        // cancellation opens no Adapter, so it is exempt (ADR-0038).
        let mut conn = open_seeded();
        seed_update_mutation(&conn, "pending");
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            4,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );

        cancel(&mut conn, "t1").unwrap();

        assert_eq!(state_of(&conn, 4), MutationState::Cancelled);
        assert_eq!(
            state_of(&conn, 1),
            MutationState::Pending,
            "an unrelated older Mutation is untouched"
        );
    }

    #[test]
    fn an_applied_promotion_still_names_the_operation_it_belongs_to() {
        // The half-applied `--children` case: the Epic the operator typed at
        // promote time landed, its child was rejected, and the Epic is still
        // the natural handle on the operation to withdraw.
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "e1",
            "tk-1",
            1,
            "applied",
            ItemClass::Epic,
            Some("op-1"),
        );
        seed_recovery(
            &conn,
            "c1",
            "tk-2",
            2,
            "failed",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let report = cancel(&mut conn, "e1").unwrap();

        assert_eq!(
            report
                .cancelled_promotions
                .iter()
                .map(|p| p.display_id.as_str())
                .collect::<Vec<_>>(),
            vec!["tk-2"]
        );
        assert_eq!(state_of(&conn, 2), MutationState::Cancelled);
        assert_eq!(state_of(&conn, 1), MutationState::Applied);
    }

    #[test]
    fn an_operation_with_nothing_left_to_withdraw_is_refused() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "applied",
            ItemClass::Ticket,
            Some("op-1"),
        );

        let err = cancel(&mut conn, "t1").unwrap_err();

        assert!(
            matches!(err, CancelPromotionError::NothingToWithdraw(ref op) if op == "op-1"),
            "got {err:?}"
        );
    }

    #[test]
    fn a_re_promoted_item_resolves_to_its_current_operation() {
        let mut conn = open_seeded();
        seed_recovery(
            &conn,
            "t1",
            "tk-1",
            1,
            "cancelled",
            ItemClass::Ticket,
            Some("op-1"),
        );
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 2,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"T","body":"","backend_kind":"github"}"#,
                promotion_operation_id: Some("op-2"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let report = cancel(&mut conn, "t1").unwrap();

        assert_eq!(
            report
                .cancelled_promotions
                .iter()
                .map(|p| p.sequence)
                .collect::<Vec<_>>(),
            vec![2],
            "the withdrawn operation is the live one, not the already-cancelled one"
        );
    }

    #[test]
    fn cancelling_an_item_with_no_nonterminal_promotion_is_refused() {
        let mut conn = open_seeded();
        seed_ticket(&conn, "t1", "tk-1", 1);

        let err = cancel(&mut conn, "t1").unwrap_err();

        assert!(
            matches!(err, CancelPromotionError::Recovery(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn a_human_curated_terminal_omission_counts_as_resolved() {
        // The operation is no longer waiting on a Skipped or Cancelled
        // Mutation; a human already decided its outcome (ADR-0038).
        for state in ["skipped", "cancelled"] {
            let conn = open_seeded();
            seed_ticket(&conn, "t1", "tk-1", 1);
            seed_mutation(&conn, 1, "t1", state, Some("op-1"));

            assert!(
                unresolved_in_operation(&conn, "op-1").unwrap().is_empty(),
                "a {state} Mutation is a resolved outcome"
            );
        }
    }
}
