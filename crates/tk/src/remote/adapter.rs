//! Backend Adapter trait.
//!
//! Commands and the sync engine consume this trait so tests can substitute a
//! [`crate::remote::fake::FakeAdapter`] without spawning real backend CLIs.
//! The contract data types it exchanges — [`AdoptedItem`], [`BackendItemRefresh`],
//! [`MutationView`], [`ApplyOutcome`] — are pure domain data under
//! [`crate::domain`].
//!
//! Operational calls take `&mut self` so an Adapter may consume per-call
//! state. Backend-kind and capability declarations are immutable properties
//! and take `&self`.

use crate::domain::apply_outcome::ApplyOutcome;
use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{AdoptedItem, BackendItemRefresh};
use crate::domain::mutation_view::MutationView;
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::proc::ProcError;
use thiserror::Error;

/// Error returned by [`Adapter::apply_mutation`].
///
/// Exactly the subprocess runner's error set: an Adapter reaches the Backend
/// through the injectable runner, so the environment-failure vocabulary is
/// the runner's. Mutation-level
/// rejection — non-zero exit, refused write, validation failure — does NOT
/// flow here; it rides the [`ApplyOutcome::Rejected`] arm so the engine can persist
/// the failure detail to `mutations.failure_json` without conflating it with
/// adapter unavailability (the ADR-0009 sync failure taxonomy).
pub type ApplyError = ProcError;

/// Error returned by Backend Adapter read operations.
///
/// Extends the runner's environment failures with [`AdapterReadError::Failed`]
/// for adapter-level rejection after the CLI ran. The payload carries the
/// Adapter's diagnostic across the typed boundary; Adopt renders it directly,
/// while Backend Pull stops before merging any collected refreshes.
#[derive(Debug, Error)]
pub enum AdapterReadError {
    /// Adapter unavailable — backend CLI missing on PATH or spawn failed.
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

    /// Apply one pending Mutation Log entry to the backend.
    ///
    /// Returns [`ApplyOutcome::Accepted`] (a `Receipt`) or [`ApplyOutcome::Rejected`] (a
    /// `Failure` carrying the rejection detail). Environment failures arrive
    /// through the [`ApplyError`] error arm. `now` is the engine's injected
    /// timestamp for adapters that stamp their backend writes.
    fn apply_mutation(
        &mut self,
        view: &MutationView,
        now: &str,
    ) -> Result<ApplyOutcome, ApplyError>;

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
