//! Backend Adapter trait.
//!
//! Commands and the sync engine consume this trait so tests can substitute a
//! [`crate::remote::fake::FakeAdapter`] without spawning real backend CLIs.
//! The contract data types it exchanges — [`BackendItemSnapshot`],
//! [`MutationView`], [`ApplyOutcome`] — are pure domain data under
//! [`crate::domain`].
//!
//! The Pull and Apply methods take `&mut self` because the only stateful
//! implementation today is the test fake (a consumed script queue); real
//! adapters reach the backend through an injected
//! [`crate::proc::ProcRunner`] and hold no per-call mutable state, so
//! `&mut self` costs them nothing. Capability declaration is static data and
//! takes `&self`.

use crate::domain::apply_outcome::ApplyOutcome;
use crate::domain::backend_item_snapshot::BackendItemSnapshot;
use crate::domain::mutation_view::MutationView;
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::proc::ProcError;
use thiserror::Error;

/// Error returned by [`Adapter::apply_mutation`].
///
/// Exactly the subprocess runner's error set: every v1 adapter (GitHub via
/// `gh`, Jira via `acli`) reaches the backend through the injectable runner,
/// so the environment-failure vocabulary is the runner's. Mutation-level
/// rejection — non-zero exit, refused write, validation failure — does NOT
/// flow here; it rides the [`ApplyOutcome::Rejected`] arm so the engine can persist
/// the failure detail to `mutations.failure_json` without conflating it with
/// adapter unavailability (the ADR-0009 sync failure taxonomy).
pub type ApplyError = ProcError;

/// Error returned by [`Adapter::fetch_snapshots`].
///
/// Extends the runner's environment failures with [`PullError::Failed`] for
/// adapter-level rejection (the CLI ran but exited non-zero). `Failed` carries
/// the CLI stderr the adapter captured — ADR-0018 carries this diagnostic on
/// the typed error itself. The engine renders the
/// detail and stops the sync: Pull is all-or-nothing in v1, so one bad key
/// (e.g. a deleted Adopted issue) fails the whole refresh.
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
    /// Fetch snapshots of the given backend `keys` — the Adopted working set's
    /// active items, per ADR-0034. The sync engine derives the key set; the
    /// adapter fetches exactly those and neither lists nor discovers. An empty
    /// `keys` slice yields an empty result with no backend call.
    ///
    /// On [`PullError::Failed`] the payload is the CLI stderr the adapter
    /// captured; the engine renders it and stops the sync.
    fn fetch_snapshots(&mut self, keys: &[&str]) -> Result<Vec<BackendItemSnapshot>, PullError>;

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
