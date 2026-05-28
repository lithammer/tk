//! Backend Adapter seam.
//!
//! The [`adapter::Adapter`] trait is the single boundary the sync engine
//! ([`crate::sync`]) and the `tk sync` command talk to; it is backend-blind,
//! so neither imports a concrete `github` / `jira` adapter. Real adapters
//! plug in behind the trait (tk-40); until then the only implementations are
//! the [`factory`] stub (returns `NotImplemented` for a configured Remote)
//! and the test-only [`fake::FakeAdapter`] the engine's tests substitute.

pub mod adapter;
pub mod factory;

#[cfg(test)]
pub mod fake;
