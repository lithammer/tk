//! Promotion engine: the local half of `tk promote` (ADR-0035, ADR-0036).
//!
//! Sibling of [`crate::sync`]. `sync` drains the Mutation Log against a
//! Backend Adapter; `promotion` decides what one `tk promote` invocation
//! commits to that log in the first place — the whole operation is preflighted
//! before anything is written, so a rejected Promotion leaves the outbox empty.
//!
//! [`crate::domain::promotion_graph`] holds the Repository Store snapshot that
//! reasoning runs over — plain data with no SQLite dependency, so the planner
//! is testable without a database, and it lives in `domain/` because `store/`
//! produces it (ARCHITECTURE.md: infrastructure-free contract types shared by
//! `store/` and an engine). [`crate::store::promotion`] owns the SQL.
//!
//! [`plan`] is the reasoning itself: snapshot in, ordered Mutations or
//! collected findings out.

pub mod plan;
