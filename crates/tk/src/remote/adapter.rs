//! Backend Adapter trait.
//!
//! Commands and the sync engine consume this trait so tests can substitute a
//! [`crate::remote::fake::FakeAdapter`] without spawning real backend CLIs.
//! The contract data types it exchanges — [`BackendItemSnapshot`],
//! [`MutationView`], [`Outcome`] — are pure domain data under
//! [`crate::domain`].
//!
//! The trait takes `&mut self` because the only stateful implementation today
//! is the test fake (a consumed script queue); real adapters reach the backend
//! through an injected [`crate::proc::ProcRunner`] and hold no per-call mutable
//! state, so `&mut self` costs them nothing.

use crate::domain::backend_item_snapshot::BackendItemSnapshot;
use crate::domain::mutation_view::MutationView;
use crate::domain::outcome::Outcome;
use crate::proc::ProcError;
use thiserror::Error;

/// Error returned by [`Adapter::apply_mutation`].
///
/// Exactly the subprocess runner's error set: every v1 adapter (GitHub via
/// `gh`, Jira via `acli`) reaches the backend through the injectable runner,
/// so the environment-failure vocabulary is the runner's. Mutation-level
/// rejection — non-zero exit, refused write, validation failure — does NOT
/// flow here; it rides the [`Outcome::Failure`] arm so the engine can persist
/// the failure detail to `mutations.failure_json` without conflating it with
/// adapter unavailability (the ADR-0009 sync failure taxonomy).
pub type ApplyError = ProcError;

/// Error returned by [`Adapter::pull_backend_items`].
///
/// Extends the runner's environment failures with [`PullError::Failed`] for
/// adapter-level rejection (the CLI ran but exited non-zero). `Failed` carries
/// the CLI stderr the adapter captured — this is where the Zig `?*Diagnostic`
/// out-param collapses into the typed error (ADR-0018). The engine renders the
/// detail and stops the sync: Pull is all-or-nothing in v1 because the
/// snapshot model assumes a consistent backend view.
#[derive(Debug, Error)]
pub enum PullError {
    /// Adapter unavailable — backend CLI missing on PATH or spawn failed.
    #[error(transparent)]
    Env(#[from] ProcError),
    /// The backend CLI ran but rejected the Pull; the payload is the captured
    /// stderr the engine renders beneath its failure frame.
    #[error("{0}")]
    Failed(String),
}

/// Type-erased Backend Adapter.
pub trait Adapter {
    /// Fetch the current snapshot of backend-owned Items.
    ///
    /// On [`PullError::Failed`] the payload is the CLI stderr the adapter
    /// captured; the engine renders it and stops the sync.
    fn pull_backend_items(&mut self) -> Result<Vec<BackendItemSnapshot>, PullError>;

    /// Apply one pending Mutation Log entry to the backend.
    ///
    /// Returns [`Outcome::Success`] (a `Receipt`) or [`Outcome::Failure`] (a
    /// `Failure` carrying the rejection detail). Environment failures arrive
    /// through the [`ApplyError`] error arm. `now` is the engine's injected
    /// timestamp for adapters that stamp their backend writes.
    fn apply_mutation(&mut self, view: &MutationView, now: &str) -> Result<Outcome, ApplyError>;
}
