//! Preflight for one `tk promote` invocation: a pure function from a
//! Repository Store snapshot to either the ordered Mutations the operation
//! commits or every problem it would hit (ADR-0035, ADR-0036).
//!
//! Nothing here touches SQLite, Git, or a Backend — the planner reasons over
//! [`PromotionGraph`] alone, so the whole operation is judged before a byte is
//! written and a refused Promotion leaves the outbox empty. Findings
//! accumulate rather than short-circuit: one run reports everything the user
//! has to fix.

use std::collections::{HashMap, HashSet};

use crate::domain::backend_intent::BackendIntent;
use crate::domain::backend_kind::BackendKind;
use crate::domain::dependency_rule::{self, DependencyClassification, DependencyRejection};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::{
    DependencyRef, EpicRef, MutationPayload, Promotion, StatusChange,
};
use crate::domain::mutation_type::MutationType;
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::domain::promotion_graph::{GraphItem, PromotionGraph};
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
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

/// One reason the operation cannot proceed, as data the command renders.
///
/// Every variant carries its parts typed; no variant carries assembled prose,
/// because `tk promote` and any other reader phrase the same fact differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionFinding {
    /// A `triage` Ticket: captured-but-unaccepted work is not pushed to a
    /// Backend, and must be Accepted first.
    TriageTicket { item: ItemRef },
    /// The Backend Adapter declares it cannot create this Item Class under
    /// Promotion.
    ItemClassNotRepresentable {
        item: ItemRef,
        item_class: ItemClass,
    },
    /// The Backend Adapter declares it cannot create this Ticket Kind under
    /// Promotion.
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
        remedy: DependencyRemedy,
    },
    /// A Dependency the operation would make backend intent, on a Backend
    /// that declares it cannot represent Dependencies. Keeping it local
    /// instead would leave the backend-backed Blocked Item exposing an
    /// incomplete blocking relationship (ADR-0035).
    DependencyNotRepresentable { blocked: ItemRef, blocking: ItemRef },
    /// Epic membership the operation would make backend intent, on a Backend
    /// that declares it cannot represent membership.
    EpicMembershipNotRepresentable { ticket: ItemRef, epic: ItemRef },
}

/// What the user can do about a rejected Dependency.
///
/// Derived from the rejection reason at plan time so the command renders one
/// typed choice instead of inventing advice per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyRemedy {
    /// Promote the Blocking Item in the same operation, or drop the edge.
    PromoteBlockingItemOrUnblock,
    /// The endpoints belong to two different Backends and no Promotion moves
    /// either one, so only dropping the edge resolves it.
    Unblock,
}

impl DependencyRemedy {
    fn for_rejection(reason: DependencyRejection) -> Self {
        match reason {
            DependencyRejection::BackendBlockedLocalBlocking => Self::PromoteBlockingItemOrUnblock,
            DependencyRejection::BackendKindMismatch => Self::Unblock,
        }
    }
}

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

/// Sort key for relationship work: the creation order of the first endpoint,
/// then of the second. Dependencies order by (Blocked, Blocking) and Epic
/// membership by (Ticket, Epic), so findings and Mutations about the same
/// pair of Items land together whichever relationship they describe.
type EndpointOrder = (i64, i64);

/// Both endpoints of every edge, and the containing Epic of every Item in the
/// snapshot, are present in `PromotionGraph::items` by the snapshot's own
/// contract. A missing one is a `read_promotion_graph` fault, not user input.
const GRAPH_IS_CLOSED: &str = "PromotionGraph carries every Item its edges and containers name";

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
    let by_id: HashMap<&str, &GraphItem> = graph.items.iter().map(|i| (i.id.as_str(), i)).collect();

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
    // Every rule below reads the Origins the whole operation *will* produce,
    // not the ones it starts from (ADR-0035).
    let intent_after = |item: &GraphItem| -> BackendIntent {
        if promoted.contains(item.id.as_str()) {
            BackendIntent::PendingPromotion {
                backend_kind: backend.text().to_owned(),
            }
        } else {
            item.backend_intent.clone()
        }
    };

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

    let mut relationship_findings: Vec<(EndpointOrder, PromotionFinding)> = Vec::new();
    let mut relationship_mutations: Vec<(EndpointOrder, MutationDraft)> = Vec::new();

    for edge in &graph.dependencies {
        let blocked = *by_id.get(edge.blocked_id.as_str()).expect(GRAPH_IS_CLOSED);
        let blocking = *by_id.get(edge.blocking_id.as_str()).expect(GRAPH_IS_CLOSED);
        let order = (blocked.created_seq, blocking.created_seq);

        match dependency_rule::classify(&intent_after(blocked), &intent_after(blocking)) {
            DependencyClassification::Rejected(reason) => {
                relationship_findings.push((
                    order,
                    PromotionFinding::DependencyRejected {
                        blocked: ItemRef::of(blocked),
                        blocking: ItemRef::of(blocking),
                        reason,
                        remedy: DependencyRemedy::for_rejection(reason),
                    },
                ));
            }
            DependencyClassification::BecomesBackendIntent => {
                // An edge whose endpoints were both backend-bound before the
                // operation is already backend intent; this Promotion neither
                // creates it nor is judged on it. Only edges the operation
                // itself binds are captured, which is also what makes a
                // re-invoked `tk promote` append nothing.
                if !promoted.contains(blocked.id.as_str())
                    && !promoted.contains(blocking.id.as_str())
                {
                    continue;
                }
                if capabilities.can_represent_dependencies() {
                    relationship_mutations.push((order, dependency_draft(blocked, blocking)));
                } else {
                    relationship_findings.push((
                        order,
                        PromotionFinding::DependencyNotRepresentable {
                            blocked: ItemRef::of(blocked),
                            blocking: ItemRef::of(blocking),
                        },
                    ));
                }
            }
            DependencyClassification::StaysLocal => {}
        }
    }

    for ticket in &graph.items {
        let Some(container_id) = ticket.container_id.as_deref() else {
            continue;
        };
        // Either endpoint may be the promoted one: `--children` promotes
        // Tickets into an Epic, and promoting an Epic snapshots membership for
        // the Tickets it already contains. A pair the operation does not move
        // is either already backend intent or still local, and neither is this
        // Promotion's business — the same bound the Dependency rule uses.
        if !promoted.contains(ticket.id.as_str()) && !promoted.contains(container_id) {
            continue;
        }
        let epic = *by_id.get(container_id).expect(GRAPH_IS_CLOSED);
        // Membership becomes backend intent only when Ticket and Epic are
        // backed by the same Backend after the operation. Mixed-Origin
        // membership stays local and is not a problem.
        if intent_after(ticket).backend_kind() != Some(backend.text())
            || intent_after(epic).backend_kind() != Some(backend.text())
        {
            continue;
        }
        let order = (ticket.created_seq, epic.created_seq);
        if capabilities.can_represent_epic_membership() {
            relationship_mutations.push((order, membership_draft(ticket, epic)));
        } else {
            relationship_findings.push((
                order,
                PromotionFinding::EpicMembershipNotRepresentable {
                    ticket: ItemRef::of(ticket),
                    epic: ItemRef::of(epic),
                },
            ));
        }
    }

    relationship_findings.sort_by_key(|(order, _)| *order);
    findings.extend(relationship_findings.into_iter().map(|(_, f)| f));
    if !findings.is_empty() {
        return Err(findings);
    }

    relationship_mutations.sort_by_key(|(order, _)| *order);
    let mut mutations = promotions;
    mutations.extend(relationship_mutations.into_iter().map(|(_, m)| m));
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
    if item.backend_intent != BackendIntent::Local {
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
    // all: reporting both against a Backend declaring neither says one thing
    // twice. A Backend that takes Tickets but not one Kind — GitHub and `bug`,
    // once tk-137 lands — still reports the Kind on its own.
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
        ItemStatus::Open => None,
        ItemStatus::Active | ItemStatus::Done => Some(MutationDraft {
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
            status: ItemStatus::Open,
            title: format!("Title of {id}"),
            body: String::new(),
            created_seq,
            container_id: None,
            backend_intent: BackendIntent::Local,
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
            backend_intent: BackendIntent::Backend {
                backend_kind: kind.to_owned(),
            },
            ..item
        }
    }

    fn pending_on(kind: &str, item: GraphItem) -> GraphItem {
        GraphItem {
            backend_intent: BackendIntent::PendingPromotion {
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

    fn all_capabilities() -> PromotionCapabilities {
        PromotionCapabilities::none()
            .with_item_class(ItemClass::Ticket)
            .with_item_class(ItemClass::Epic)
            .with_ticket_kind(TicketKind::Task)
            .with_ticket_kind(TicketKind::Bug)
            .with_dependencies()
            .with_epic_membership()
    }

    fn plan(graph: &PromotionGraph) -> PromotionPlan {
        plan_promotion(graph, all_capabilities(), BackendKind::Github).expect("plan")
    }

    fn findings(
        graph: &PromotionGraph,
        capabilities: PromotionCapabilities,
    ) -> Vec<PromotionFinding> {
        plan_promotion(graph, capabilities, BackendKind::Github).expect_err("findings")
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
            findings(&g, all_capabilities()),
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
                        status: ItemStatus::Done,
                        ..ticket("blocker", 2)
                    },
                ],
            ),
            vec![edge("t1", "blocker")],
        );

        assert_eq!(
            findings(&g, all_capabilities()),
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
                remedy: DependencyRemedy::PromoteBlockingItemOrUnblock,
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
            findings(&g, all_capabilities()).as_slice(),
            [PromotionFinding::DependencyRejected {
                reason: DependencyRejection::BackendKindMismatch,
                remedy: DependencyRemedy::Unblock,
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
    fn active_and_done_items_push_their_status_but_open_ones_do_not() {
        let g = with_children(graph(
            "e1",
            vec![
                epic("e1", 1),
                contained_in(
                    "e1",
                    GraphItem {
                        status: ItemStatus::Active,
                        ..ticket("active", 2)
                    },
                ),
                contained_in(
                    "e1",
                    GraphItem {
                        status: ItemStatus::Done,
                        ..ticket("done", 3)
                    },
                ),
                contained_in("e1", ticket("open", 4)),
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
            vec![
                (
                    "active",
                    MutationPayload::ItemStatus(StatusChange {
                        status: "active".to_owned()
                    })
                ),
                (
                    "done",
                    MutationPayload::ItemStatus(StatusChange {
                        status: "done".to_owned()
                    })
                ),
            ]
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
                            status: ItemStatus::Done,
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
