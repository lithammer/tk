//! Pure relationship preflight for Promotion and Re-Adopt. It derives the
//! required Backend capabilities from a Repository Store snapshot, then builds
//! either the ordered Mutations the operation commits or every problem it
//! would hit (ADR-0035, ADR-0036, ADR-0047).
//!
//! Nothing here touches SQLite, Git, or a Backend. Every input is pure domain
//! data, so the whole operation is judged before a byte is written and a
//! refused Promotion leaves the outbox empty. Findings accumulate rather than
//! short-circuit: one run reports everything the user has to fix.

use std::collections::{HashMap, HashSet};

use crate::domain::backend_binding::BackendBinding;
use crate::domain::backend_kind::BackendKind;
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::epic_membership_rule;
use crate::domain::item_class::ItemClass;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, MutationPayload, Promotion, StatusChange,
};
use crate::domain::mutation_type::MutationType;
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::domain::promotion_graph::{GraphItem, PromotionGraph};
use crate::domain::promotion_plan::{MutationDraft, PromotionPlan};
use crate::domain::selection_state::SelectionState;
use crate::domain::ticket_kind::TicketKind;

/// The Item a finding names: the internal stable `items.id` that keys it, and
/// the Display ID the rendered diagnostic shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub id: String,
    pub display_id: String,
}

impl ItemRef {
    fn of(item: &GraphItem) -> Self {
        Self {
            id: item.id.clone(),
            display_id: item.display_id.clone(),
        }
    }
}

/// Derive the Backend capability facets needed by a Promotion graph.
///
/// This pass lets the Adapter read the Backend only for requested
/// repository-specific facets while the planner stays independent of the
/// Adapter.
#[must_use]
pub fn promotion_requirements(
    graph: &PromotionGraph,
    backend: BackendKind,
) -> PromotionRequirements {
    let promoted: HashSet<&str> = graph
        .items
        .iter()
        .filter(|item| is_promoted(graph, item))
        .map(|item| item.id.as_str())
        .collect();
    let mut requirements = PromotionRequirements::none();

    for item in graph
        .items
        .iter()
        .filter(|item| promoted.contains(item.id.as_str()))
    {
        requirements = requirements.with_item_class(item.item_class);
        if let Some(kind) = item.ticket_kind {
            requirements = requirements.with_ticket_kind(kind);
        }
    }

    let relationship_facets = relationship_requirements(graph, &promoted, backend);
    if relationship_facets.requires_dependencies() {
        requirements = requirements.with_dependencies();
    }
    if relationship_facets.requires_epic_membership() {
        requirements = requirements.with_epic_membership();
    }

    requirements
}

/// Derive the relationship facets needed for the Items an operation binds.
///
/// `bound_ids` names Items the operation will bind to `backend`. Keeping this
/// pass draft-free lets callers resolve Backend capabilities before the
/// transactional planner builds payloads.
#[must_use]
pub(crate) fn relationship_requirements(
    graph: &PromotionGraph,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> PromotionRequirements {
    let by_id: HashMap<&str, &GraphItem> = graph.items.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut requirements = PromotionRequirements::none();
    if classify_dependencies(graph, &by_id, bound_ids, backend)
        .iter()
        .any(|edge| matches!(edge.verdict, EdgeVerdict::BecomesBackendIntent))
    {
        requirements = requirements.with_dependencies();
    }
    if !bound_memberships(graph, &by_id, bound_ids, backend).is_empty() {
        requirements = requirements.with_epic_membership();
    }
    requirements
}

/// Plan the relationship Mutations created when an operation binds Items.
///
/// The function owns ADR-0035's resulting-graph classification, capability
/// checks, endpoint ordering, and payload construction for both Promotion and
/// Re-Adopt.
pub(crate) fn plan_relationships(
    graph: &PromotionGraph,
    bound_ids: &HashSet<&str>,
    capabilities: PromotionCapabilities,
    backend: BackendKind,
) -> Result<Vec<MutationDraft>, Vec<PromotionFinding>> {
    let by_id: HashMap<&str, &GraphItem> = graph.items.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut findings: Vec<(EndpointOrder, PromotionFinding)> = Vec::new();
    let mut mutations: Vec<(EndpointOrder, MutationDraft)> = Vec::new();

    for edge in classify_dependencies(graph, &by_id, bound_ids, backend) {
        match edge.verdict {
            EdgeVerdict::Rejected(reason) => findings.push((
                edge.order,
                PromotionFinding::DependencyRejected {
                    blocked: ItemRef::of(edge.blocked),
                    blocking: ItemRef::of(edge.blocking),
                    reason,
                },
            )),
            EdgeVerdict::BecomesBackendIntent => {
                if capabilities.can_represent_dependencies() {
                    mutations.push((edge.order, dependency_draft(edge.blocked, edge.blocking)));
                } else {
                    findings.push((
                        edge.order,
                        PromotionFinding::DependencyNotRepresentable {
                            blocked: ItemRef::of(edge.blocked),
                            blocking: ItemRef::of(edge.blocking),
                        },
                    ));
                }
            }
            EdgeVerdict::Untouched => {}
        }
    }

    for membership in bound_memberships(graph, &by_id, bound_ids, backend) {
        if capabilities.can_represent_epic_membership() {
            mutations.push((
                membership.order,
                membership_draft(membership.ticket, membership.epic),
            ));
        } else {
            findings.push((
                membership.order,
                PromotionFinding::EpicMembershipNotRepresentable {
                    ticket: ItemRef::of(membership.ticket),
                    epic: ItemRef::of(membership.epic),
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

/// One reason the operation cannot proceed, as data the command renders.
///
/// Every variant carries its parts typed; no variant carries assembled prose,
/// because `tk promote` and any other reader phrase the same fact differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionFinding {
    /// A `triage` Ticket: captured-but-unaccepted work is not pushed to a
    /// Backend, and must be Accepted first.
    TriageTicket { item: ItemRef },
    /// The Backend Adapter cannot create this Item Class under Promotion.
    ItemClassNotRepresentable {
        item: ItemRef,
        item_class: ItemClass,
    },
    /// The Backend Adapter cannot create this Ticket Kind under Promotion.
    TicketKindNotRepresentable {
        item: ItemRef,
        ticket_kind: TicketKind,
    },
    /// An existing Dependency the resulting graph cannot hold. ADR-0035
    /// requires both endpoints, the reason, and an available remedy.
    DependencyRejected {
        blocked: ItemRef,
        blocking: ItemRef,
        reason: DependencyRejection,
    },
    /// A Dependency the operation would make backend intent, on a Backend
    /// that cannot represent Dependencies. Keeping it local instead would
    /// leave the backend-backed Blocked Item exposing an incomplete blocking
    /// relationship (ADR-0035).
    DependencyNotRepresentable { blocked: ItemRef, blocking: ItemRef },
    /// Epic membership the operation would make backend intent, on a Backend
    /// that cannot represent membership.
    EpicMembershipNotRepresentable { ticket: ItemRef, epic: ItemRef },
}

/// Sort key for relationship work: the creation order of the first endpoint,
/// then of the second. Dependencies order by (Blocked, Blocking) and Epic
/// membership by (Ticket, Epic), so findings and Mutations about the same
/// pair of Items land together whichever relationship they describe.
type EndpointOrder = (i64, i64);

/// Both endpoints of every edge are present in `PromotionGraph::items` by the
/// snapshot's own contract — edges are collected from the Items, so neither
/// endpoint can be one the read never saw. A missing one is a
/// `read_graph` fault, not user input.
const GRAPH_IS_CLOSED: &str = "PromotionGraph carries both endpoints of every edge it names";

/// The Backend Binding an Item has after the operation lands. ADR-0035 and
/// ADR-0047 judge relationships against this future graph.
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

/// What one Dependency means for this operation, judged against the graph the
/// operation leaves behind.
///
/// Both passes read the same verdict, so the requirements pass cannot name a
/// facet the planner does not need, nor miss one it does.
enum EdgeVerdict {
    /// ADR-0035 refuses the resulting graph, whatever this operation touches.
    Rejected(DependencyRejection),
    /// This operation is what makes the Dependency backend intent, so the
    /// Backend has to be able to represent it.
    BecomesBackendIntent,
    /// Nothing for this operation to create or answer for.
    Untouched,
}

/// One judged Dependency: its endpoints, its sort key, and what it means here.
struct JudgedEdge<'a> {
    order: EndpointOrder,
    blocked: &'a GraphItem,
    blocking: &'a GraphItem,
    verdict: EdgeVerdict,
}

/// A Ticket/Epic membership this operation makes backend intent, with its sort
/// key. Pairs the operation does not move, and pairs whose Epic the snapshot
/// never saw, are already filtered out.
struct BoundMembership<'a> {
    order: EndpointOrder,
    ticket: &'a GraphItem,
    epic: &'a GraphItem,
}

/// Judge every Dependency in the snapshot once, in `graph.dependencies` order.
fn classify_dependencies<'a>(
    graph: &'a PromotionGraph,
    by_id: &HashMap<&'a str, &'a GraphItem>,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> Vec<JudgedEdge<'a>> {
    graph
        .dependencies
        .iter()
        .map(|edge| {
            let blocked = *by_id.get(edge.blocked_id.as_str()).expect(GRAPH_IS_CLOSED);
            let blocking = *by_id.get(edge.blocking_id.as_str()).expect(GRAPH_IS_CLOSED);
            let verdict = match dependency_rule::classify(
                &binding_after(blocked, bound_ids, backend),
                &binding_after(blocking, bound_ids, backend),
            ) {
                DependencyClassification::Rejected(reason) => EdgeVerdict::Rejected(reason),
                // An edge whose endpoints were both backend-bound before the
                // operation is already backend intent; this operation neither
                // creates it nor is judged on it. Only edges the operation
                // itself binds are captured, which is also what makes a
                // re-invoked `tk promote` append nothing.
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

/// Collect every Epic membership this operation makes backend intent, in
/// `graph.items` order.
fn bound_memberships<'a>(
    graph: &'a PromotionGraph,
    by_id: &HashMap<&'a str, &'a GraphItem>,
    bound_ids: &HashSet<&str>,
    backend: BackendKind,
) -> Vec<BoundMembership<'a>> {
    let mut bound = Vec::new();
    for ticket in &graph.items {
        let Some(container_id) = ticket.container_id.as_deref() else {
            continue;
        };
        // Either endpoint may be bound by the operation: `--children` promotes
        // Tickets into an Epic, and promoting an Epic snapshots membership for
        // the Tickets it already contains. A pair the operation does not move
        // is either already backend intent or still local, and neither is this
        // operation's business — the same bound the Dependency rule uses.
        if !bound_ids.contains(ticket.id.as_str()) && !bound_ids.contains(container_id) {
            continue;
        }
        // A container absent from the snapshot is a torn concurrent edit, not a
        // graph fault: `read_graph` collects the item set first and
        // re-reads each Item's `container_id` second, un-transacted, so a
        // `tk update --parent` committing in between names an Epic the read
        // never saw. Nothing can be decided about a membership only half of
        // which is visible, so it is skipped rather than guessed at.
        let Some(epic) = by_id.get(container_id).copied() else {
            continue;
        };
        // Membership becomes backend intent only when Ticket and Epic are
        // backed by the same Backend after the operation. Mixed-Origin
        // membership stays local and is not a problem.
        if epic_membership_rule::classify(
            &binding_after(ticket, bound_ids, backend),
            &binding_after(epic, bound_ids, backend),
        ) == epic_membership_rule::Classification::StaysLocal
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

/// Preflight a `tk promote` of `graph.target_id` against `backend`, the
/// Backend the configured Remote names.
///
/// Returns the ordered plan, or every finding that stops the operation. An
/// operation with nothing to promote succeeds with an empty plan.
///
/// # Errors
///
/// Returns the collected [`PromotionFinding`]s when the operation cannot
/// proceed: item findings in creation order first, then relationship findings
/// in endpoint order.
pub fn plan_promotion(
    graph: &PromotionGraph,
    capabilities: PromotionCapabilities,
    backend: BackendKind,
) -> Result<PromotionPlan, Vec<PromotionFinding>> {
    // `graph.items` is in creation order, so every list derived from this one
    // is too.
    let operation: Vec<&GraphItem> = graph
        .items
        .iter()
        .filter(|i| is_promoted(graph, i))
        .collect();
    if operation.is_empty() {
        return Ok(PromotionPlan::default());
    }

    let promoted: HashSet<&str> = operation.iter().map(|i| i.id.as_str()).collect();
    let mut findings = Vec::new();
    let mut promotions = Vec::new();
    let mut statuses = Vec::new();

    for item in &operation {
        collect_item_findings(item, capabilities, &mut findings);
        promotions.push(promotion_draft(item, backend));
        if let Some(draft) = status_draft(item) {
            statuses.push(draft);
        }
    }

    let relationship_mutations = match plan_relationships(graph, &promoted, capabilities, backend) {
        Ok(mutations) => mutations,
        Err(relationship_findings) => {
            findings.extend(relationship_findings);
            Vec::new()
        }
    };
    if !findings.is_empty() {
        return Err(findings);
    }

    let mut mutations = promotions;
    mutations.extend(relationship_mutations);
    // A Promotion creates an open backend object, so a locally active or
    // closed Item needs its status pushed after creation. The Closing Reason
    // is a Local Field and never travels.
    mutations.extend(statuses);
    Ok(PromotionPlan { mutations })
}

/// Whether the operation promotes this Item: the target, or — with
/// `--children` — a Ticket the target Epic directly contains. Work that is
/// already Backend or already Pending Promotion contributes nothing, which is
/// what makes the command idempotent.
///
/// A Backend Epic target with `--children` therefore promotes its local
/// children while contributing no Promotion of its own. That is a supported
/// operation, not an error.
fn is_promoted(graph: &PromotionGraph, item: &GraphItem) -> bool {
    if item.backend_binding != BackendBinding::Local {
        return false;
    }
    item.id == graph.target_id
        || (graph.children_requested
            && item.container_id.as_deref() == Some(graph.target_id.as_str()))
}

fn collect_item_findings(
    item: &GraphItem,
    capabilities: PromotionCapabilities,
    findings: &mut Vec<PromotionFinding>,
) {
    if item.selection_state == Some(SelectionState::Triage) {
        findings.push(PromotionFinding::TriageTicket {
            item: ItemRef::of(item),
        });
    }
    // Findings accumulate rather than short-circuit (ADR-0036), but the Ticket
    // Kind question only arises for a Backend that can create the Item Class at
    // all. Reporting both when the Backend supports neither says one thing
    // twice; a Backend that supports Tickets but not one Kind still reports the
    // Kind on its own.
    if capabilities.can_create_item_class(item.item_class) {
        if let Some(ticket_kind) = item.ticket_kind
            && !capabilities.can_create_ticket_kind(ticket_kind)
        {
            findings.push(PromotionFinding::TicketKindNotRepresentable {
                item: ItemRef::of(item),
                ticket_kind,
            });
        }
    } else {
        findings.push(PromotionFinding::ItemClassNotRepresentable {
            item: ItemRef::of(item),
            item_class: item.item_class,
        });
    }
}

fn promotion_draft(item: &GraphItem, backend: BackendKind) -> MutationDraft {
    MutationDraft {
        mutation_type: match item.item_class {
            ItemClass::Ticket => MutationType::PromoteTicket,
            ItemClass::Epic => MutationType::PromoteEpic,
        },
        item_id: item.id.clone(),
        item_class: item.item_class,
        payload: MutationPayload::Promotion(Promotion {
            title: item.title.clone(),
            body: item.body.clone(),
            backend_kind: backend.text().to_owned(),
        }),
    }
}

fn status_draft(item: &GraphItem) -> Option<MutationDraft> {
    match item.status {
        Lifecycle::Open => None,
        Lifecycle::Done => Some(MutationDraft {
            mutation_type: MutationType::SetItemStatus,
            item_id: item.id.clone(),
            item_class: item.item_class,
            payload: MutationPayload::ItemStatus(StatusChange {
                status: item.status.text().to_owned(),
            }),
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::promotion_graph::GraphDependency;

    fn ticket(id: &str, created_seq: i64) -> GraphItem {
        GraphItem {
            id: id.to_owned(),
            display_id: format!("tk-{created_seq}"),
            item_class: ItemClass::Ticket,
            ticket_kind: Some(TicketKind::Task),
            selection_state: Some(SelectionState::Accepted),
            status: Lifecycle::Open,
            title: format!("Title of {id}"),
            body: String::new(),
            created_seq,
            container_id: None,
            backend_binding: BackendBinding::Local,
        }
    }

    fn epic(id: &str, created_seq: i64) -> GraphItem {
        GraphItem {
            item_class: ItemClass::Epic,
            ticket_kind: None,
            selection_state: None,
            ..ticket(id, created_seq)
        }
    }

    fn contained_in(epic_id: &str, item: GraphItem) -> GraphItem {
        GraphItem {
            container_id: Some(epic_id.to_owned()),
            ..item
        }
    }

    fn on_backend(kind: &str, item: GraphItem) -> GraphItem {
        GraphItem {
            backend_binding: BackendBinding::Backend {
                backend_kind: kind.to_owned(),
            },
            ..item
        }
    }

    fn pending_on(kind: &str, item: GraphItem) -> GraphItem {
        GraphItem {
            backend_binding: BackendBinding::PendingPromotion {
                backend_kind: kind.to_owned(),
            },
            ..item
        }
    }

    fn edge(blocked_id: &str, blocking_id: &str) -> GraphDependency {
        GraphDependency {
            blocked_id: blocked_id.to_owned(),
            blocking_id: blocking_id.to_owned(),
        }
    }

    fn graph(target_id: &str, items: Vec<GraphItem>) -> PromotionGraph {
        PromotionGraph {
            target_id: target_id.to_owned(),
            children_requested: false,
            items,
            dependencies: Vec::new(),
        }
    }

    fn with_children(graph: PromotionGraph) -> PromotionGraph {
        PromotionGraph {
            children_requested: true,
            ..graph
        }
    }

    fn with_edges(graph: PromotionGraph, dependencies: Vec<GraphDependency>) -> PromotionGraph {
        PromotionGraph {
            dependencies,
            ..graph
        }
    }

    fn plan(graph: &PromotionGraph) -> PromotionPlan {
        plan_promotion(graph, PromotionCapabilities::all(), BackendKind::Github).expect("plan")
    }

    fn findings(
        graph: &PromotionGraph,
        capabilities: PromotionCapabilities,
    ) -> Vec<PromotionFinding> {
        plan_promotion(graph, capabilities, BackendKind::Github).expect_err("findings")
    }

    fn ticket_with_kind(id: &str, created_seq: i64, kind: TicketKind) -> GraphItem {
        GraphItem {
            ticket_kind: Some(kind),
            ..ticket(id, created_seq)
        }
    }

    #[test]
    fn requirements_include_the_promoted_ticket_class_and_kind() {
        let requirements = promotion_requirements(
            &graph("t1", vec![ticket_with_kind("t1", 1, TicketKind::Bug)]),
            BackendKind::Github,
        );

        assert!(requirements.requires_item_class(ItemClass::Ticket));
        assert!(!requirements.requires_item_class(ItemClass::Epic));
        assert!(requirements.requires_ticket_kind(TicketKind::Bug));
        assert!(!requirements.requires_ticket_kind(TicketKind::Task));
        assert!(!requirements.requires_dependencies());
        assert!(!requirements.requires_epic_membership());
    }

    #[test]
    fn requirements_include_relationships_the_operation_makes_backend_intent() {
        let graph = with_edges(
            with_children(graph(
                "e1",
                vec![epic("e1", 1), contained_in("e1", ticket("t1", 2))],
            )),
            vec![edge("t1", "e1")],
        );

        let requirements = promotion_requirements(&graph, BackendKind::Github);

        assert!(requirements.requires_item_class(ItemClass::Ticket));
        assert!(requirements.requires_item_class(ItemClass::Epic));
        assert!(requirements.requires_ticket_kind(TicketKind::Task));
        assert!(requirements.requires_dependencies());
        assert!(requirements.requires_epic_membership());
    }

    /// `(mutation_type, item_id)` pairs, the shape order assertions read best
    /// in.
    fn shape(plan: &PromotionPlan) -> Vec<(&str, &str)> {
        plan.mutations
            .iter()
            .map(|m| (m.mutation_type.text(), m.item_id.as_str()))
            .collect()
    }

    #[test]
    fn a_standalone_local_ticket_promotes_itself() {
        let plan = plan(&graph("t1", vec![ticket("t1", 1)]));

        assert_eq!(
            plan.mutations,
            vec![MutationDraft {
                mutation_type: MutationType::PromoteTicket,
                item_id: "t1".to_owned(),
                item_class: ItemClass::Ticket,
                payload: MutationPayload::Promotion(Promotion {
                    title: "Title of t1".to_owned(),
                    body: String::new(),
                    backend_kind: "github".to_owned(),
                }),
            }]
        );
    }

    #[test]
    fn a_local_epic_without_children_promotes_only_itself() {
        let g = graph(
            "e1",
            vec![epic("e1", 1), contained_in("e1", ticket("t1", 2))],
        );

        assert_eq!(shape(&plan(&g)), vec![("promote_epic", "e1")]);
    }

    #[test]
    fn promoting_an_epic_snapshots_membership_for_its_existing_backend_tickets() {
        // tk-136: "Promoting an Epic snapshots membership for existing Backend
        // Tickets". Without `--children` the Local sibling stays local — it is
        // not a Promotion Child — but the Backend Ticket and the Epic end up on
        // the same Backend, so that membership becomes intent.
        let g = graph(
            "e1",
            vec![
                epic("e1", 1),
                contained_in("e1", on_backend("github", ticket("backed", 2))),
                contained_in("e1", ticket("local", 3)),
            ],
        );

        assert_eq!(
            shape(&plan(&g)),
            vec![("promote_epic", "e1"), ("add_ticket_to_epic", "backed")]
        );
    }

    #[test]
    fn an_epic_with_children_promotes_its_local_children_too() {
        let g = with_children(graph(
            "e1",
            vec![
                epic("e1", 1),
                contained_in("e1", ticket("t1", 2)),
                contained_in("e1", on_backend("github", ticket("t2", 3))),
                ticket("outside", 4),
            ],
        ));

        // t2 is already a Backend Ticket in the Epic: promoting the Epic
        // brings both onto the same Backend, so its membership becomes intent
        // too (tk-136, "Promoting an Epic snapshots membership for existing
        // Backend Tickets").
        assert_eq!(
            shape(&plan(&g)),
            vec![
                ("promote_epic", "e1"),
                ("promote_ticket", "t1"),
                ("add_ticket_to_epic", "t1"),
                ("add_ticket_to_epic", "t2"),
            ]
        );
    }

    #[test]
    fn a_backend_epic_target_with_children_promotes_only_the_children() {
        let g = with_children(graph(
            "e1",
            vec![
                on_backend("github", epic("e1", 1)),
                contained_in("e1", ticket("t1", 2)),
            ],
        ));

        assert_eq!(
            shape(&plan(&g)),
            vec![("promote_ticket", "t1"), ("add_ticket_to_epic", "t1")]
        );
    }

    #[test]
    fn an_already_backend_target_plans_nothing() {
        let g = graph("t1", vec![on_backend("github", ticket("t1", 1))]);

        assert!(plan(&g).is_empty());
    }

    #[test]
    fn an_already_pending_target_plans_nothing() {
        let g = graph("t1", vec![pending_on("github", ticket("t1", 1))]);

        assert!(plan(&g).is_empty());
    }

    #[test]
    fn a_triage_ticket_is_refused() {
        let g = graph(
            "t1",
            vec![GraphItem {
                selection_state: Some(SelectionState::Triage),
                ..ticket("t1", 1)
            }],
        );

        assert_eq!(
            findings(&g, PromotionCapabilities::all()),
            vec![PromotionFinding::TriageTicket {
                item: ItemRef {
                    id: "t1".to_owned(),
                    display_id: "tk-1".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn an_uncreatable_item_class_is_refused() {
        let g = graph("e1", vec![epic("e1", 1)]);
        let capabilities = PromotionCapabilities::none().with_item_class(ItemClass::Ticket);

        assert!(matches!(
            findings(&g, capabilities).as_slice(),
            [PromotionFinding::ItemClassNotRepresentable {
                item_class: ItemClass::Epic,
                ..
            }]
        ));
    }

    #[test]
    fn an_uncreatable_ticket_kind_is_refused() {
        let g = graph(
            "t1",
            vec![GraphItem {
                ticket_kind: Some(TicketKind::Bug),
                ..ticket("t1", 1)
            }],
        );
        let capabilities = PromotionCapabilities::none()
            .with_item_class(ItemClass::Ticket)
            .with_ticket_kind(TicketKind::Task);

        assert!(matches!(
            findings(&g, capabilities).as_slice(),
            [PromotionFinding::TicketKindNotRepresentable {
                ticket_kind: TicketKind::Bug,
                ..
            }]
        ));
    }

    #[test]
    fn item_findings_accumulate_instead_of_short_circuiting() {
        let g = graph(
            "t1",
            vec![GraphItem {
                selection_state: Some(SelectionState::Triage),
                ticket_kind: Some(TicketKind::Bug),
                ..ticket("t1", 1)
            }],
        );

        // Selection State and Ticket Kind are independent facets, so a Backend
        // that takes the Item Class still reports both.
        let capabilities = PromotionCapabilities::none().with_item_class(ItemClass::Ticket);
        assert!(matches!(
            findings(&g, capabilities).as_slice(),
            [
                PromotionFinding::TriageTicket { .. },
                PromotionFinding::TicketKindNotRepresentable { .. },
            ]
        ));
    }

    #[test]
    fn an_unrepresentable_item_class_subsumes_the_ticket_kind_finding() {
        // A Backend that cannot create Tickets at all has nothing to say about
        // which Kind, and reporting both says one thing twice.
        let g = graph(
            "t1",
            vec![GraphItem {
                ticket_kind: Some(TicketKind::Bug),
                ..ticket("t1", 1)
            }],
        );

        assert!(matches!(
            findings(&g, PromotionCapabilities::none()).as_slice(),
            [PromotionFinding::ItemClassNotRepresentable { .. }]
        ));
    }

    #[test]
    fn a_promoted_blocked_item_may_not_wait_on_a_local_blocking_item() {
        // The Blocking Item is `done`: a resolved Dependency is retained, so
        // Promotion still has to represent it (ADR-0035).
        let g = with_edges(
            graph(
                "t1",
                vec![
                    ticket("t1", 1),
                    GraphItem {
                        status: Lifecycle::Done,
                        ..ticket("blocker", 2)
                    },
                ],
            ),
            vec![edge("t1", "blocker")],
        );

        assert_eq!(
            findings(&g, PromotionCapabilities::all()),
            vec![PromotionFinding::DependencyRejected {
                blocked: ItemRef {
                    id: "t1".to_owned(),
                    display_id: "tk-1".to_owned(),
                },
                blocking: ItemRef {
                    id: "blocker".to_owned(),
                    display_id: "tk-2".to_owned(),
                },
                reason: DependencyRejection::BackendBlockedLocalBlocking,
            }]
        );
    }

    #[test]
    fn a_blocking_item_on_another_backend_is_refused() {
        let g = with_edges(
            graph(
                "t1",
                vec![ticket("t1", 1), on_backend("jira", ticket("blocker", 2))],
            ),
            vec![edge("t1", "blocker")],
        );

        assert!(matches!(
            findings(&g, PromotionCapabilities::all()).as_slice(),
            [PromotionFinding::DependencyRejected {
                reason: DependencyRejection::BackendKindMismatch,
                ..
            }]
        ));
    }

    #[test]
    fn a_dependency_inside_the_operation_becomes_intent_in_either_creation_order() {
        // The Blocking Item is created *after* the Blocked Item, so a planner
        // reading current Origins in creation order would reject this edge.
        // The whole operation is evaluated together (ADR-0035).
        let g = with_edges(
            with_children(graph(
                "e1",
                vec![
                    epic("e1", 1),
                    contained_in("e1", ticket("early", 2)),
                    contained_in("e1", ticket("late", 3)),
                ],
            )),
            vec![edge("early", "late")],
        );

        assert!(
            shape(&plan(&g)).contains(&("add_dependency", "early")),
            "an edge between two promoted Items is backend intent"
        );
    }

    #[test]
    fn a_dependency_on_an_item_already_on_the_backend_becomes_intent() {
        let g = with_edges(
            graph(
                "t1",
                vec![ticket("t1", 1), on_backend("github", ticket("blocker", 2))],
            ),
            vec![edge("t1", "blocker")],
        );

        assert_eq!(
            plan(&g).mutations[1],
            MutationDraft {
                mutation_type: MutationType::AddDependency,
                item_id: "t1".to_owned(),
                item_class: ItemClass::Ticket,
                payload: MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "blocker".to_owned(),
                }),
            }
        );
    }

    #[test]
    fn a_dependency_the_backend_cannot_represent_is_refused() {
        let g = with_edges(
            graph(
                "t1",
                vec![ticket("t1", 1), on_backend("github", ticket("blocker", 2))],
            ),
            vec![edge("t1", "blocker")],
        );
        let capabilities = PromotionCapabilities::none()
            .with_item_class(ItemClass::Ticket)
            .with_ticket_kind(TicketKind::Task);

        assert!(matches!(
            findings(&g, capabilities).as_slice(),
            [PromotionFinding::DependencyNotRepresentable { .. }]
        ));
    }

    #[test]
    fn a_dependency_between_two_items_the_operation_does_not_touch_is_left_alone() {
        // Promoting a child of a Backend Epic says nothing about that Epic's
        // own edges, which were already backend intent before the operation.
        let g = with_edges(
            with_children(graph(
                "e1",
                vec![
                    on_backend("github", epic("e1", 1)),
                    contained_in("e1", ticket("t1", 2)),
                    on_backend("github", ticket("blocker", 3)),
                ],
            )),
            vec![edge("e1", "blocker")],
        );

        assert_eq!(
            shape(&plan(&g)),
            vec![("promote_ticket", "t1"), ("add_ticket_to_epic", "t1")]
        );
    }

    #[test]
    fn a_local_dependency_between_two_unpromoted_items_is_silent() {
        let g = with_edges(
            graph("t1", vec![ticket("t1", 1), ticket("a", 2), ticket("b", 3)]),
            vec![edge("a", "b")],
        );

        assert_eq!(shape(&plan(&g)), vec![("promote_ticket", "t1")]);
    }

    #[test]
    fn membership_the_backend_cannot_represent_is_refused() {
        let g = graph(
            "t1",
            vec![
                on_backend("github", epic("e1", 1)),
                contained_in("e1", ticket("t1", 2)),
            ],
        );
        let capabilities = PromotionCapabilities::none()
            .with_item_class(ItemClass::Ticket)
            .with_ticket_kind(TicketKind::Task);

        assert!(matches!(
            findings(&g, capabilities).as_slice(),
            [PromotionFinding::EpicMembershipNotRepresentable { .. }]
        ));
    }

    #[test]
    fn membership_of_a_local_epic_stays_local_without_a_finding() {
        let g = graph(
            "t1",
            vec![epic("e1", 1), contained_in("e1", ticket("t1", 2))],
        );

        assert_eq!(shape(&plan(&g)), vec![("promote_ticket", "t1")]);
    }

    #[test]
    fn membership_of_an_epic_on_another_backend_stays_local_without_a_finding() {
        let g = graph(
            "t1",
            vec![
                on_backend("jira", epic("e1", 1)),
                contained_in("e1", ticket("t1", 2)),
            ],
        );

        assert_eq!(shape(&plan(&g)), vec![("promote_ticket", "t1")]);
    }

    #[test]
    fn done_items_push_their_status_but_open_ones_do_not() {
        // Only a closed Lifecycle is worth pushing. Work State is local
        // (ADR-0043), so the planner has no third value to draft, and an `open`
        // Item needs no status Mutation because a Promotion creates it open.
        let g = with_children(graph(
            "e1",
            vec![
                epic("e1", 1),
                contained_in(
                    "e1",
                    GraphItem {
                        status: Lifecycle::Done,
                        ..ticket("done", 2)
                    },
                ),
                contained_in("e1", ticket("open", 3)),
            ],
        ));

        let plan = plan(&g);
        let statuses: Vec<_> = plan
            .mutations
            .iter()
            .filter(|m| m.mutation_type == MutationType::SetItemStatus)
            .map(|m| (m.item_id.as_str(), m.payload.clone()))
            .collect();
        assert_eq!(
            statuses,
            vec![(
                "done",
                MutationPayload::ItemStatus(StatusChange {
                    status: "done".to_owned()
                })
            )]
        );
    }

    /// An Epic, two children, one Dependency, two memberships and one closed
    /// child: enough in every tier that a wrong order is visible.
    fn ordering_graph() -> PromotionGraph {
        with_edges(
            with_children(graph(
                "e1",
                vec![
                    epic("e1", 1),
                    contained_in("e1", ticket("c1", 2)),
                    contained_in(
                        "e1",
                        GraphItem {
                            status: Lifecycle::Done,
                            ..ticket("c2", 3)
                        },
                    ),
                ],
            )),
            vec![edge("c2", "c1")],
        )
    }

    #[test]
    fn mutations_are_ordered_promotions_then_relationships_then_statuses() {
        assert_eq!(
            shape(&plan(&ordering_graph())),
            vec![
                ("promote_epic", "e1"),
                ("promote_ticket", "c1"),
                ("promote_ticket", "c2"),
                ("add_ticket_to_epic", "c1"),
                ("add_ticket_to_epic", "c2"),
                ("add_dependency", "c2"),
                ("set_item_status", "c2"),
            ]
        );
    }

    #[test]
    fn findings_are_ordered_item_findings_then_relationships_in_endpoint_order() {
        let named: Vec<String> = findings(&ordering_graph(), PromotionCapabilities::none())
            .iter()
            .map(|finding| match finding {
                PromotionFinding::ItemClassNotRepresentable { item, .. } => {
                    format!("class {}", item.id)
                }
                PromotionFinding::TicketKindNotRepresentable { item, .. } => {
                    format!("kind {}", item.id)
                }
                PromotionFinding::EpicMembershipNotRepresentable { ticket, epic } => {
                    format!("membership {}->{}", ticket.id, epic.id)
                }
                PromotionFinding::DependencyNotRepresentable { blocked, blocking } => {
                    format!("dependency {}->{}", blocked.id, blocking.id)
                }
                other => panic!("unexpected finding: {other:?}"),
            })
            .collect();

        assert_eq!(
            named,
            vec![
                "class e1",
                "class c1",
                "class c2",
                "membership c1->e1",
                "membership c2->e1",
                "dependency c2->c1",
            ]
        );
    }
}
