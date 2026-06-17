//! Pure domain helpers — no SQLite, filesystem, Git, or subprocess access.
//!
//! The schema-determined value types: Priority, ItemStatus, SelectionState,
//! TicketKind, ItemClass, Origin, MutationType, MutationState,
//! MutationPayload, MutationView, and BackendItemSnapshot. Each one is pinned
//! by an existing SQL CHECK constraint
//! or by an ADR — the shape exists independently of any future Backend Adapter
//! — so the rest of the codebase uses typed values instead of raw strings at
//! the store boundary.
//!
//! Two shapes are deliberately not modelled as standalone types here:
//!
//! - `Diagnostic` — ADR-0018 folds diagnostics into `Result<T, E>`; captured
//!   stderr and SQLite errmsgs ride on typed error payloads instead.
//! - `MutationFailure` / `FailureClass` — ADR-0016 settles the contract but
//!   the persisted shape is a flat `{"detail":"…"}` wrapper, not a classified
//!   record. The wrapper lives at the store boundary
//!   ([`crate::store::sync`]); a richer classified type only earns its place
//!   when a concrete Backend Adapter produces the evidence to classify.
//!
//! [`apply_outcome`] carries the typed Apply-result shape (ADR-0009 taxonomy);
//! the sync engine is the consumer ADR-0018 deferred it for.
//!
//! `display_prefix` already lives under [`crate::store`] alongside its only
//! current consumer (`tk init`); revisit the placement when real cross-module
//! consumers exist.

pub mod apply_outcome;
pub mod backend_item_snapshot;
pub mod backend_kind;
pub mod item_class;
pub mod mutation_payload;
pub mod mutation_state;
pub mod mutation_type;
pub mod mutation_view;
pub mod origin;
pub mod priority;
pub mod selection_state;
pub mod status;
pub mod ticket_kind;
