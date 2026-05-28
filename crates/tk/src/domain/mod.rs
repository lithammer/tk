//! Pure domain helpers — no SQLite, filesystem, Git, or subprocess access.
//!
//! Ports the schema-determined value types from the Zig `src/domain/` tree:
//! Priority, ItemStatus, TicketKind, ItemClass, Origin, MutationType,
//! MutationPayload, MutationView, and BackendItemSnapshot. Each one is pinned
//! by an existing SQL CHECK constraint or by an ADR — the shape exists
//! independently of any future Backend Adapter — so porting now lets the rest
//! of the codebase use typed values instead of raw strings at the store
//! boundary (see the "Porting from Zig" section in AGENTS.md).
//!
//! Three Zig-side types are deliberately not ported here because they are
//! evidence-determined: their shape depends on what a concrete Backend
//! Adapter observes, and porting them now would either ossify a speculative
//! API or ship a Zig-shaped placeholder.
//!
//! - `Diagnostic` — ADR-0018 names the `?*Diagnostic` out-param as one of
//!   the three Zig error shapes the Rust port collapses into `Result<T, E>`.
//! - `Outcome` / `Receipt` / `Failure` — the typed-Outcome shape is the
//!   second such collapse target (ADR-0018). The success/failure return
//!   shape for the future Adapter trait lands with the first concrete
//!   Backend Adapter under real adapter pressure.
//! - `MutationFailure` / `FailureClass` — ADR-0016 settles the contract and
//!   defers the in-memory type to the first concrete Backend Adapter.
//!
//! `display_prefix` already lives under [`crate::store`] alongside its only
//! current consumer (`tk init`); revisit the placement when the store layer
//! port lands and real cross-module consumers exist.

pub mod backend_item_snapshot;
pub mod item_class;
pub mod mutation_payload;
pub mod mutation_type;
pub mod mutation_view;
pub mod origin;
pub mod priority;
pub mod status;
pub mod ticket_kind;
