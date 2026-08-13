//! Backend Adapter trait.
//!
//! Commands and the sync engine consume this trait so tests can substitute a
//! [`crate::remote::fake::FakeAdapter`] without spawning real backend CLIs.
//! The contract data types it exchanges are pure domain data under
//! [`crate::domain`]. Edits and creation are separate calls because creation
//! has a stronger result-certainty contract.
//!
//! Operational calls take `&mut self` so an Adapter may consume per-call
//! state. Backend-kind and capability declarations are immutable properties
//! and take `&self`.

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemInspection, BackendItemRefresh,
};
use crate::domain::backend_outcome::{BackendCreateOutcome, BackendEditOutcome};
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::proc::ProcError;
use thiserror::Error;

/// Environment error returned by Backend Adapter edit operations.
///
/// Exactly the subprocess runner's error set: an Adapter reaches the Backend
/// through the injectable runner, so the environment-failure vocabulary is
/// the runner's. Mutation-level
/// rejection does not flow here; it rides the typed outcome so the engine can
/// persist it without conflating it with Adapter unavailability (ADR-0009).
pub type ApplyError = ProcError;

/// Error returned by Backend Adapter read operations.
///
/// Extends the runner's environment failures with [`AdapterReadError::Failed`]
/// for adapter-level rejection after the CLI ran. The payload carries the
/// Adapter's diagnostic across the typed boundary; Adopt renders it directly,
/// while Backend Pull stops before merging any collected refreshes.
#[derive(Debug, Error)]
pub enum AdapterReadError {
    /// Adapter unavailable — the Backend CLI did not start or its outcome
    /// could not be observed.
    #[error(transparent)]
    Env(#[from] ProcError),
    /// The backend CLI ran but rejected the read; the payload is the
    /// Adapter-owned diagnostic rendered by the calling command.
    #[error("{0}")]
    Failed(String),
}

/// Type-erased Backend Adapter.
pub trait Adapter {
    /// Typed kind of the Backend this Adapter reaches.
    fn backend_kind(&self) -> BackendKind;

    /// Canonicalize one Backend issue into full intake data for `tk adopt`.
    fn adopt_ticket(&mut self, input: &str) -> Result<AdoptedItem, AdapterReadError>;

    /// Refresh backend-owned fields of one existing Backend Item.
    ///
    /// The sync engine owns the working set and calls this once per key; a
    /// failure prevents every collected refresh from being merged.
    fn refresh_item(&mut self, key: &str) -> Result<BackendItemRefresh, AdapterReadError>;

    /// Inspect one Backend object for Promotion recovery.
    ///
    /// This narrow read returns only canonical identity and content. It does
    /// not classify the item as a Ticket Kind or map Backend lifecycle state.
    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError>;

    /// Apply one non-Promotion Mutation to an existing Backend object.
    ///
    /// Environment failures arrive through the [`ApplyError`] error arm.
    fn apply_edit(&mut self, edit: &BackendEdit) -> Result<BackendEditOutcome, ApplyError>;

    /// Apply one Promotion by creating a new Backend object.
    ///
    /// The result distinguishes a confirmed identity, certified no effect,
    /// and an indeterminate result. Creation exposes no error arm because the
    /// Adapter owns the effect-certainty classification, including whether a
    /// runner error proves the process never started.
    fn create_item(&mut self, create: &BackendCreate) -> BackendCreateOutcome;

    /// The Backend's static Promotion capability declaration (ADR-0036
    /// "Backend capability is declared per facet and staged"). Preflight
    /// reads this before any backend call to reject a Promotion the Adapter
    /// cannot represent.
    ///
    /// Declared data, not a backend call: the signature takes `&self` and
    /// returns the value directly rather than a `Result`, so nothing here
    /// can fail or spawn a subprocess.
    fn promotion_capabilities(&self) -> PromotionCapabilities;
}
