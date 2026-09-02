//! Pure domain helpers — no SQLite, filesystem, Git, or subprocess access.
//!
//! The schema-determined value types: Priority, ItemStatus, Lifecycle,
//! WorkState, SelectionState, TicketKind, ItemClass, Origin,
//! BindingDisplayProvenance, MutationType, MutationState, MutationPayload,
//! Backend Adapter operation contracts, BackendBinding, and
//! PromotionCapabilities. Each one is pinned by an existing SQL CHECK
//! constraint or by an ADR — the shape exists independently of any future
//! Backend Adapter — so the rest of the codebase uses typed values instead of
//! raw strings at the store boundary. Lifecycle and WorkState are the two axes
//! ADR-0043 splits `items.status` into.
//!
//! [`backend_binding`] is the one derived from two sources rather than read off
//! a column: Origin plus the Mutation Log answer whether an Item is Pending
//! Promotion (ADR-0036). The store resolves it; the type keeps the vocabulary
//! out of raw origin/`backend_kind` pairs.
//!
//! [`promotion_graph`] and [`promotion_plan`] are the pair of contract types
//! `tk promote` passes across the store/engine boundary: the
//! infrastructure-free snapshot `store/` produces and the planner reasons over,
//! and the ordered Mutations the planner returns for `store/` to commit.
//! Neither may live inside the engine, which `store/` does not import.
//!
//! Two shapes are deliberately not modelled as standalone types here:
//!
//! - `Diagnostic` — ADR-0018 folds diagnostics into `Result<T, E>`; captured
//!   stderr and SQLite errmsgs ride on typed error payloads instead.
//! - `MutationFailure` — the persisted record is [`backend_outcome::Failure`]
//!   rather than a second wrapper with the same fields.
//!
//! [`backend_outcome`] carries the directional write results defined by
//! ADR-0009 and used by the sync engine.
//!
//! [`dependency_rule`] is the one behaviour rather than a value type: what a
//! Dependency edge means for the Mutation Log (ADR-0035). It sits here because
//! both `tk block` and Promotion preflight decide it, over different graphs.
//!
//! `display_prefix` already lives under [`crate::store`] alongside its only
//! current consumer (`tk init`); revisit the placement when real cross-module
//! consumers exist.

pub mod backend_binding;
pub mod backend_kind;
pub mod backend_operation;
pub mod backend_outcome;
pub mod binding_display_provenance;
pub mod dependency_rule;
pub mod item_class;
pub mod lifecycle;
pub mod mutation_payload;
pub mod mutation_state;
pub mod mutation_type;
pub mod origin;
pub mod priority;
pub mod promotion_capability;
pub mod promotion_graph;
pub mod promotion_plan;
pub mod selection_state;
pub mod status;
pub mod ticket_kind;
pub mod work_state;
