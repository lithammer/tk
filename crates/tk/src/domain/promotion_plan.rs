//! The ordered Mutations one `tk promote` invocation commits (ADR-0035,
//! ADR-0036).
//!
//! The other half of the Promotion contract [`super::promotion_graph`] opens:
//! `store/` produces the snapshot the planner reasons over and commits the plan
//! it returns, so both shapes are infrastructure-free types shared by `store/`
//! and the engine rather than types owned by the engine `store/` would then
//! have to import.
//!
//! [`crate::promotion::plan::plan_promotion`] is the only producer;
//! [`crate::store::promotion::commit_promotion_plan`] the only consumer.

use super::item_class::ItemClass;
use super::mutation_payload::MutationPayload;
use super::mutation_type::MutationType;

/// One Mutation the plan will append.
///
/// It carries no Promotion Operation: that identity is one per `tk promote`
/// invocation and the outbox writer stamps it across the whole batch
/// (ADR-0036).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationDraft {
    pub mutation_type: MutationType,
    /// Internal stable `items.id` the Mutation targets: the Blocked Item for
    /// a Dependency, the Ticket for Epic membership.
    pub item_id: String,
    pub item_class: ItemClass,
    pub payload: MutationPayload,
}

/// The ordered Mutations one `tk promote` invocation commits.
///
/// The order is the outbox contract (ADR-0035): item Promotions first, then
/// the relationship Mutations whose payloads name Items those Promotions
/// create, then the status pushes. Backend identities resolve as each
/// Mutation is applied, after the preceding Promotion receipts have assigned
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PromotionPlan {
    pub mutations: Vec<MutationDraft>,
}

impl PromotionPlan {
    /// Whether the operation found nothing to promote. Re-invoking
    /// `tk promote` on work that is already Backend or already Pending
    /// Promotion is a success that appends nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }
}
