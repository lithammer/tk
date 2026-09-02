//! Pure planning for relationship intent created by Promotion or Re-Adopt
//! (ADR-0035, ADR-0047).

use std::collections::{HashMap, HashSet};

use super::backend_binding::BackendBinding;
use super::backend_kind::BackendKind;
use super::dependency_rule::{self, DependencyClassification, DependencyRejection};
use super::epic_membership_rule::{self, Classification as MembershipClassification};
use super::item_class::ItemClass;
use super::mutation_payload::{DependencyRef, EpicRef, MutationPayload};
use super::mutation_type::MutationType;
use super::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use super::promotion_graph::{GraphDependency, GraphItem};
use super::promotion_plan::MutationDraft;

/// One Item named by a relationship finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipItem {
    /// Stable internal Item ID.
    pub id: String,
    /// Display ID shown to the user.
    pub display_id: String,
    /// Item Class used when the remedy must name the Item kind.
    pub item_class: ItemClass,
}

impl RelationshipItem {
    fn of(item: &GraphItem) -> Self {
        Self {
            id: item.id.clone(),
            display_id: item.display_id.clone(),
            item_class: item.item_class,
        }
    }
}

/// One relationship problem that stops a Binding change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationshipFinding {
    /// A Dependency the resulting graph cannot hold.
    DependencyRejected {
        /// Blocked endpoint.
        blocked: RelationshipItem,
        /// Blocking endpoint.
        blocking: RelationshipItem,
        /// Resulting-graph rule that rejects the edge.
        reason: DependencyRejection,
    },
    /// A Dependency the Backend cannot represent.
    DependencyNotRepresentable {
        /// Blocked endpoint.
        blocked: RelationshipItem,
        /// Blocking endpoint.
        blocking: RelationshipItem,
    },
    /// Epic Membership the Backend cannot represent.
    EpicMembershipNotRepresentable {
        /// Member Ticket.
        ticket: RelationshipItem,
        /// Containing Epic.
        epic: RelationshipItem,
    },
}

/// Derive the relationship facets needed for the Items an operation binds.
///
/// Keeping this pass draft-free lets callers resolve Backend capabilities
/// before the transactional planner builds payloads.
#[must_use]
pub(crate) fn requirements(
    items: &[GraphItem],
    dependencies: &[GraphDependency],
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> PromotionRequirements {
    let by_id: HashMap<&str, &GraphItem> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut requirements = PromotionRequirements::none();
    if classify_dependencies(dependencies, &by_id, bound_ids, backend)
        .iter()
        .any(|edge| matches!(edge.verdict, EdgeVerdict::BecomesBackendIntent))
    {
        requirements = requirements.with_dependencies();
    }
    if !bound_memberships(items, &by_id, bound_ids, backend).is_empty() {
        requirements = requirements.with_epic_membership();
    }
    requirements
}

/// Plan the relationship Mutations created when an operation binds Items.
///
/// Findings and Mutations follow endpoint creation order (ADR-0035).
///
/// # Errors
///
/// Returns every ordered relationship finding when the resulting graph is
/// invalid or the Backend lacks a required capability.
pub(crate) fn plan(
    items: &[GraphItem],
    dependencies: &[GraphDependency],
    bound_ids: &HashSet<&str>,
    capabilities: PromotionCapabilities,
    backend: BackendKind,
) -> Result<Vec<MutationDraft>, Vec<RelationshipFinding>> {
    let by_id: HashMap<&str, &GraphItem> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut findings: Vec<(EndpointOrder, RelationshipFinding)> = Vec::new();
    let mut mutations: Vec<(EndpointOrder, MutationDraft)> = Vec::new();

    for edge in classify_dependencies(dependencies, &by_id, bound_ids, backend) {
        match edge.verdict {
            EdgeVerdict::Rejected(reason) => findings.push((
                edge.order,
                RelationshipFinding::DependencyRejected {
                    blocked: RelationshipItem::of(edge.blocked),
                    blocking: RelationshipItem::of(edge.blocking),
                    reason,
                },
            )),
            EdgeVerdict::BecomesBackendIntent => {
                if capabilities.can_represent_dependencies() {
                    mutations.push((edge.order, dependency_draft(edge.blocked, edge.blocking)));
                } else {
                    findings.push((
                        edge.order,
                        RelationshipFinding::DependencyNotRepresentable {
                            blocked: RelationshipItem::of(edge.blocked),
                            blocking: RelationshipItem::of(edge.blocking),
                        },
                    ));
                }
            }
            EdgeVerdict::Untouched => {}
        }
    }

    for membership in bound_memberships(items, &by_id, bound_ids, backend) {
        if capabilities.can_represent_epic_membership() {
            mutations.push((
                membership.order,
                membership_draft(membership.ticket, membership.epic),
            ));
        } else {
            findings.push((
                membership.order,
                RelationshipFinding::EpicMembershipNotRepresentable {
                    ticket: RelationshipItem::of(membership.ticket),
                    epic: RelationshipItem::of(membership.epic),
                },
            ));
        }
    }

    findings.sort_by_key(|(order, _)| *order);
    if !findings.is_empty() {
        return Err(findings.into_iter().map(|(_, finding)| finding).collect());
    }
    mutations.sort_by_key(|(order, _)| *order);
    Ok(mutations
        .into_iter()
        .map(|(_, mutation)| mutation)
        .collect())
}

/// Dependencies order by (Blocked, Blocking); membership orders by (Ticket,
/// Epic). This keeps findings and Mutations deterministic across both kinds.
type EndpointOrder = (i64, i64);

/// Both endpoints of every Dependency must occur in `items`.
const GRAPH_IS_CLOSED: &str = "relationship graph carries both endpoints of every edge it names";

/// The Backend Binding an Item has after the operation lands. Relationship
/// rules treat Pending Promotion and Backend as the same Backend kind.
fn binding_after(
    item: &GraphItem,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> BackendBinding {
    if bound_ids.contains(item.id.as_str()) {
        BackendBinding::PendingPromotion {
            backend_kind: backend.text().to_owned(),
        }
    } else {
        item.backend_binding.clone()
    }
}

/// What one Dependency means in the graph the operation will leave.
enum EdgeVerdict {
    /// The resulting graph cannot hold this edge.
    Rejected(DependencyRejection),
    /// The operation creates Backend intent for this edge.
    BecomesBackendIntent,
    /// The operation neither creates nor invalidates this edge.
    Untouched,
}

struct JudgedEdge<'a> {
    order: EndpointOrder,
    blocked: &'a GraphItem,
    blocking: &'a GraphItem,
    verdict: EdgeVerdict,
}

struct BoundMembership<'a> {
    order: EndpointOrder,
    ticket: &'a GraphItem,
    epic: &'a GraphItem,
}

fn classify_dependencies<'a>(
    dependencies: &'a [GraphDependency],
    by_id: &HashMap<&'a str, &'a GraphItem>,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> Vec<JudgedEdge<'a>> {
    dependencies
        .iter()
        .map(|edge| {
            let blocked = *by_id.get(edge.blocked_id.as_str()).expect(GRAPH_IS_CLOSED);
            let blocking = *by_id.get(edge.blocking_id.as_str()).expect(GRAPH_IS_CLOSED);
            let verdict = match dependency_rule::classify(
                &binding_after(blocked, bound_ids, backend),
                &binding_after(blocking, bound_ids, backend),
            ) {
                DependencyClassification::Rejected(reason) => EdgeVerdict::Rejected(reason),
                // An edge already backed before the operation is existing
                // Backend intent. Only an edge touching a newly bound Item
                // creates a fresh Mutation.
                DependencyClassification::BecomesBackendIntent
                    if bound_ids.contains(blocked.id.as_str())
                        || bound_ids.contains(blocking.id.as_str()) =>
                {
                    EdgeVerdict::BecomesBackendIntent
                }
                DependencyClassification::BecomesBackendIntent
                | DependencyClassification::StaysLocal => EdgeVerdict::Untouched,
            };
            JudgedEdge {
                order: (blocked.created_seq, blocking.created_seq),
                blocked,
                blocking,
                verdict,
            }
        })
        .collect()
}

fn bound_memberships<'a>(
    items: &'a [GraphItem],
    by_id: &HashMap<&'a str, &'a GraphItem>,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> Vec<BoundMembership<'a>> {
    let mut bound = Vec::new();
    for ticket in items {
        let Some(container_id) = ticket.container_id.as_deref() else {
            continue;
        };
        if !bound_ids.contains(ticket.id.as_str()) && !bound_ids.contains(container_id) {
            continue;
        }
        // `read_graph` gathers Items before it re-reads membership. A
        // concurrent parent update may therefore name an Epic absent from the
        // snapshot; skip the torn pair instead of guessing at its Binding.
        let Some(epic) = by_id.get(container_id).copied() else {
            continue;
        };
        if epic_membership_rule::classify(
            &binding_after(ticket, bound_ids, backend),
            &binding_after(epic, bound_ids, backend),
        ) == MembershipClassification::StaysLocal
        {
            continue;
        }
        bound.push(BoundMembership {
            order: (ticket.created_seq, epic.created_seq),
            ticket,
            epic,
        });
    }
    bound
}

fn dependency_draft(blocked: &GraphItem, blocking: &GraphItem) -> MutationDraft {
    MutationDraft {
        mutation_type: MutationType::AddDependency,
        item_id: blocked.id.clone(),
        item_class: blocked.item_class,
        payload: MutationPayload::DependencyRef(DependencyRef {
            blocking_id: blocking.id.clone(),
        }),
    }
}

fn membership_draft(ticket: &GraphItem, epic: &GraphItem) -> MutationDraft {
    MutationDraft {
        mutation_type: MutationType::AddTicketToEpic,
        item_id: ticket.id.clone(),
        item_class: ticket.item_class,
        payload: MutationPayload::EpicRef(EpicRef {
            epic_id: epic.id.clone(),
        }),
    }
}
