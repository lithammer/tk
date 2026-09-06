# Next-release inclusion in tk

_Researched 2026-09-05 for tk-214. Local-only Plan design confirmed 2026-09-06
in ADR-0050._

## Research plan

- Question: what is the least costly maintainable approach that lets a human
  choose the next release's Tickets and lets tk show what remains?
- Candidates: read-only GitHub Milestone, writable GitHub Milestone, and
  GitHub Project used only for release inclusion, with read-only and writable
  access assessed separately.
- Search terms: milestone filtering, ProjectV2 items, draft issues, project
  field values, pagination, OAuth scopes, Backend Pull, Scope, Shared Field.
- Sources: GitHub documentation, cli/cli source at installed gh v2.100.0,
  tk architecture and ADRs. No live Backend writes or API probes are needed.
- Repository filter: cli/cli v2.100.0 and this tk checkout; no popularity or
  ecosystem ranking is relevant.
- Budget: two focused research branches and a local architecture pass,
  inspecting at most twelve primary documents or source groups.
- Deliverable: this file, with a comparison, evidence ledger, negative
  evidence, recommendation, and the next unresolved design question.

## Summary

Local-only next-release inclusion is the selected MVP. It serves Local and
Backend Tickets from the Repository Store without planning network reads,
remote identity, or reconciliation. It still needs local membership,
commands, rendering and lifecycle rules. Backend Ticket status remains only
as fresh as the existing store state.

Read-only Milestones are the narrowest remote candidate examined. GitHub owns
membership; tk reads it without an assignment command or membership
Mutations. A Project UI can still display native Milestone metadata, so that
representation need not dictate the human planning interface. However, live
reads incur latency and failure handling; stored reads need refresh rules.

Read-only Projects also remain viable. A project per release avoids custom
field mapping; a release field in an existing project needs a field/value
contract. Writable support adds delivery and reconciliation work to either
representation.

The comparison below rates maintenance surfaces, not measured implementation
time. The remaining product question is whether tk only reports remaining
work or also restricts `tk next` to that work.

## Candidates

| Approach | Human workflow | Maintenance surface in tk | Main trade-off |
| --- | --- | --- | --- |
| Local only (selected) | Choose Tickets and inspect remaining work in tk | Local membership, commands, lifecycle and rendering; selection rules if included | No shared GitHub plan; avoids network reads for membership |
| GitHub only | Choose and inspect work in GitHub | None | Does not connect release membership to tk selection |
| Read-only Milestone | Set membership in GitHub; tk reads it | Milestone identity, complete reads, issue mapping, error handling | No membership edits from tk; Local Tickets need Promotion to become GitHub issues |
| Writable Milestone | Set membership from tk or GitHub | Read support plus assignment/removal, Mutations, pending-write protection, retry and lifecycle rules | Earns its cost if editing from tk is a real need |
| Read-only Project | Use one project per release, or a release field in an existing project | Owner/project identity, item kinds and pagination; field identity/type/value when field-based; project access | Better fit for an established Project workflow, with more representation choices to specify |
| Writable Project | Edit project membership or release fields from tk | Project reads plus item and field writes, write permissions, reconciliation and recovery | Broader editing surface than this task currently needs |

For both read-only representations, distinguish two implementations. An
on-demand remote view needs network access and does not require persisting
membership. Importing membership into the Repository Store supports local
queries but needs a schema, refresh semantics, and a clear freshness contract.
Read-only does not mean free of state design if it is stored.

## tk constraints

- `BackendItemRefresh` carries title, body, Lifecycle and optional Ticket
  Kind; it has no planning field. Adding one would extend Adapter and Store
  contracts, not merely rendering.
- Backend Pull refreshes exact known open Items. Its Store merge does not
  discover or insert Items. A view of a whole remote release can therefore
  differ from a view of its Adopted Tickets. Neither should silently claim
  to be the other.
- `tk next` selects locally from `NextScope::None` or `NextScope::Epic`.
  Release selection would need a deliberate extension. ADR-0022 also makes
  Scope the boundary of Effective Priority propagation, so filtering the
  selected result afterward is not equivalent to defining release Scope.
- ADR-0049 requires Backend authority on Pull for an imported Shared Field;
  it says a Mutation *may* push it. Read-only import does not inherently
  violate that rule. A transient remote report need not introduce a field
  at all. The exact classification still belongs in the selected design.

## External read and write contracts

The CLI findings below come from source at **gh v2.100.0**, matching the
installed version, not from live probes. API claims come from official docs.
No Backend changes were made during this research.

- `gh issue list --milestone` resolves a number to a title and uses Search.
  The default limit is 30, Search caps at 1,000, and JSON export precedes the
  cap warning. A complete release view must not treat this output as proof
  of complete membership. The repository-issues REST endpoint can instead
  filter by Milestone number and be paginated; it also returns PRs, which a
  Ticket-only view must distinguish.
- `gh issue edit` supports setting and removing a Milestone. Its GraphQL
  write does not remove tk's need for a delivery contract. REST documentation
  warns that Milestone writes without push access can be silently dropped;
  that warning does not establish the behavior of gh's GraphQL path.
- `gh project item-list` also defaults to 30 items and exposes a total count.
  Its Issue content query does not request Issue state, so its JSON alone
  is insufficient to derive Issue completion. A reader can request state
  explicitly or use tracked Ticket state under a clearly stated contract.
- Project membership has an item identity separate from the Issue identity.
  A reader must distinguish Issues, PRs, drafts and inaccessible content.
  A custom-field reader also needs field and value identity. The Projects
  API guide distinguishes `read:project` from `project` OAuth scopes; actual
  access depends on the credential and project, which were not probed.
- Setting a Project custom field can require adding the Issue to the Project
  and then updating its field as separate operations. Adding an existing
  Issue returns the existing item identity, so not every retry carries a
  duplicate-creation risk. Partial field-update completion still needs care.
- Projects v2 also has REST item endpoints with filtering and cursor
  pagination. Projects is not a GraphQL-only choice. Neither API has been
  selected here.

## Evidence ledger

| Source | Type | What was inspected | Confidence |
| --- | --- | --- | --- |
| [About milestones](https://docs.github.com/en/issues/using-labels-and-milestones-to-track-work/about-milestones) | Official docs | Repository grouping, open/closed work and progress display; issues and PRs both participate | High |
| [About Projects](https://docs.github.com/en/issues/planning-and-tracking-with-projects/learning-about-projects/about-projects) | Official docs | User/org planning UI, native Milestone metadata, custom fields and draft issues | High |
| [Backend read types](../../crates/tk/src/domain/backend_operation.rs) | Current tk source | `AdoptedItem`, `BackendItemRefresh`, exact-set validation in `BackendPull` | High |
| [Store refresh](../../crates/tk/src/store/sync.rs) | Current tk source | `working_set_keys` selects open Items; merge does not insert Items | High |
| [Scope resolution](../../crates/tk/src/commands/scope.rs), [next](../../crates/tk/src/commands/next.rs), [ADR-0022](../adr/0022-scope-is-an-explicit-epic-argument-not-persisted-state.md) | Source and contract | Epic-only local selection and Effective Priority boundary | High |
| [ADR-0049](../adr/0049-backend-visible-facets-follow-a-shared-authority-rule.md) | Accepted decision | Shared authority requires Pull, but does not require a tk write command | High |
| [Issue list](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/issue/list/list.go), [search implementation](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/issue/list/http.go) | Tagged CLI source | Number-to-title resolution, Search routing, limits and export-before-warning | High |
| [Repository issues API](https://docs.github.com/en/rest/issues/issues#list-repository-issues), [Milestone API](https://docs.github.com/en/rest/issues/milestones) | Official API docs | Number-based filtering, pagination, PR inclusion, Milestone identity; REST write-permission caveat | High; GraphQL write behavior not inferred |
| [Issue edit](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/issue/edit/edit.go), [edit delivery](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/pr/shared/editable_http.go) | Tagged CLI source | Existing Milestone edit surface and GraphQL delivery | High |
| [Project item list](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/project/item-list/item_list.go), [Project queries](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/project/shared/queries/queries.go) | Tagged CLI source | Limits, item/content identity and absence of Issue state in item-list query | High |
| [Project item edit](https://github.com/cli/cli/blob/v2.100.0/pkg/cmd/project/item-edit/item_edit.go), [Projects API guide](https://docs.github.com/en/issues/planning-and-tracking-with-projects/automating-your-project/using-the-api-to-manage-projects) | Tagged source and official docs | IDs and access needed for field edits; separate add and update operations | High |
| [Project items REST API](https://docs.github.com/en/rest/projects/items) | Official API docs | REST alternative with filters and cursor pagination | High |

## Negative evidence

Local ownership gives no shared GitHub plan. If remote sharing is later
needed, identity, authority and transition rules must be designed then;
local planning is not a promise of a cheap migration. Offline operation also
does not make stored Backend Ticket status current.

The least code is no tk feature: GitHub already shows remaining Milestone
work. A tk report that only repeats that view must justify its place through
the workflow it improves.

A Milestone reader still needs reliable pagination, identity and access
handling. A stored reader adds stale-data cases. It cannot make Local Tickets
into Milestone members without a Backend issue representation.

Projects may be preferable if the human already plans there. A project per
release avoids a custom field model. Projects can hold drafts, but a GitHub
draft is a remote object, not automatically a tk Local Ticket; using drafts
as their representation would require another identity and lifecycle design.

Writable Milestones offer a small user-facing action, assigning a Ticket,
but tk's durable delivery contract gives that action more responsibilities
than a single API call. That cost may be worthwhile if assignment from tk is
needed frequently; this interview has not established that need.

## Recommendation

Proceed with local-only next-release inclusion, as agreed in the interview.
It gives the human a terminal workflow to evaluate while avoiding Backend
planning contracts. Keep remote approaches as researched alternatives, not
as requirements for the local data model. Do not add plan Promotion or
Backend write paths in the MVP.

## Open questions

The selected command family is `tk plan`, `tk plan add/remove ID [ID…]`,
and `tk next --plan`, with one current Plan and no named-plan switching.

`tk plan clear` removes membership only, including unfinished members when
explicitly requested. There is no Plan archive or separate Plan status.

Plan selection intersects Epic Scope, including `TK_SCOPE`. Effective
Priority stops at that boundary; outside Dependencies still block readiness
but their Items are not selected automatically. Bulk membership edits are
atomic, reject unknown IDs and Epics, and preserve Ticket state. Membership
admits Tickets in every state. The dedicated view ignores `TK_SCOPE` and
shows Ready, In progress, Waiting and Done, omitting empty sections and a
redundant header, with whole-Plan counts. The design is confirmed in
[ADR-0050](../adr/0050-release-grouping-records-a-commitment.md).

Remote identity, unadopted remote members, planning refresh, and Backend
writes are deferred with remote planning; they are not MVP prerequisites.
