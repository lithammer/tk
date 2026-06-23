//! Backend Adapter seam.
//!
//! The [`adapter::Adapter`] trait is the single boundary the sync engine
//! ([`crate::sync`]) and the `tk sync` command talk to; it is backend-blind,
//! so neither imports a concrete adapter. The [`factory`] selects the concrete
//! adapter for a configured Remote: [`github::GithubAdapter`] for `github`,
//! `NotImplemented` for `jira` (tk-35). The test-only [`fake::FakeAdapter`] is
//! substituted directly by the engine's tests.

pub mod adapter;
pub mod factory;
pub mod github;

#[cfg(test)]
pub mod fake;
