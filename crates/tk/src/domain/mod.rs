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
//! Two Zig-side shapes are deliberately not ported as standalone types here:
//!
//! - `Diagnostic` — ADR-0018 names the `?*Diagnostic` out-param as one of
//!   the three Zig error shapes the Rust port collapses into `Result<T, E>`.
//!   Captured stderr and SQLite errmsgs ride on typed error payloads instead.
//! - `MutationFailure` / `FailureClass` — ADR-0016 settles the contract but
//!   the persisted shape is a flat `{"detail":"…"}` wrapper, not a classified
//!   record. The wrapper lives at the store boundary
//!   ([`crate::store::sync`]); a richer classified type only earns its place
//!   when a concrete Backend Adapter produces the evidence to classify.
//!
//! [`outcome`] ports the typed Apply-result shape (ADR-0009 taxonomy): the
//! sync engine is the "real adapter pressure" ADR-0018 deferred it for.
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
pub mod outcome;
pub mod priority;
pub mod status;
pub mod ticket_kind;
