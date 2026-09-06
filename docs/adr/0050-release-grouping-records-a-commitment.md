---
status: accepted
---

# Next-release inclusion is local to the Repository Store

tk-214 asks how to choose the Tickets for the next release and see what
remains. Priority must continue to rank urgency; using P1 to record release
membership distorts `tk next` selection. The work can span Epics.

The MVP keeps one current Plan in the Repository Store, initially for the
next release. There is no named-plan registry or plan switching. Explicit
operations add and remove Tickets; a local view shows the remaining work.
The Plan can span Epics and contain both Local and Backend Tickets without
requiring Promotion. Membership is local; the Tickets retain their existing
Backend behavior.

This approach delivers release inclusion and a terminal view without adding
Backend planning identity, membership refresh, or write reconciliation.
Read-only Milestones would still require either network reads when used or
stored membership with a freshness contract. Projects add project/item
identity and, for field-based planning, field/value mapping. See
[the comparison](../spikes/next-release-inclusion.md).

Membership is owned by the Repository Store under ADR-0049's Local Field
rule. Local ownership does not promise fresh Backend Ticket status: the view
uses the Ticket state already held in the store.

Backend mapping, plan Promotion, and release-tool enforcement are deferred.
Remote sharing may require revisiting identity and ownership; this MVP does
not introduce extension points for it. Broader roadmap planning is outside
the current scope.

## Command family

`tk plan` shows progress in a dedicated view. `tk plan add ID [ID…]` and
`tk plan remove ID [ID…]` edit membership. `tk next --plan` selects ready work
from the Plan; bare `tk next` retains its existing behavior.

`tk plan clear` removes all Plan membership and preserves every Ticket and
its state. It can clear unfinished work as an explicit abandonment of the
Plan; it never closes those Tickets. Done Tickets retain membership until
explicitly removed or cleared, so progress remains visible. Removing only
finished Tickets leaves unfinished members for the next round.

The MVP has no Plan archive or separate Plan status. Clearing membership
makes room for the next Plan without a second lifecycle to manage.

`clear` is the only bulk reset in the MVP. `finish` is deferred because it
would require a completion rule beyond removing membership. A done-only
cleanup shortcut such as `remove --done` is also deferred: explicit removal
already lets unfinished work carry forward. The progress view supplies
closure through its remaining and done counts.

Bulk add and remove resolve and validate every supplied ID before changing
membership. An unknown ID or an Epic rejects the whole operation without
partial changes. Adding an existing member and removing a nonmember are
harmless no-ops.

Any Ticket may belong to the Plan, including triage, parked, active and done
Tickets. Membership changes preserve Ticket state. `tk next --plan` keeps
the existing readiness rules; the Plan view explains why unfinished members
are not selectable.

`tk next --plan` selects only Plan members. Dependencies on Items outside
the Plan still block readiness, but do not cause those Items to be selected
or added automatically. `tk plan` names outside blockers so the operator can
include them explicitly or work them separately. Membership bounds the work
selected; it is not an entry point for automatic dependency traversal.

Plan selection intersects an Epic Scope when both are supplied.
`tk next <epic-id> --plan` selects only ready Tickets that belong to both;
an Epic supplied through `TK_SCOPE` has the same effect. The explicit Epic
argument retains precedence over `TK_SCOPE`. Without an Epic Scope,
`tk next --plan` considers the whole Plan. Neither constraint silently
replaces the other.

Effective Priority propagation stops at the same selection boundary: the
Plan alone, or the Plan/Epic intersection. Only work within that boundary
contributes urgency; Dependencies outside it still block readiness. This
extends ADR-0022's distinction between a selection boundary and a
presentational filter to Plan selection, as recorded in its amendment.

Plan names the activity without suggesting release tagging or publication.
Its broader name leaves room for other outcomes without adding them to the
MVP. A dedicated view can organize progress and readiness independently of
the List Tree. `tk list --release` was rejected because its proposed rendering
changes would make it behave differently from the other list filters.

## Plan view

`tk plan` always shows the whole Plan and ignores `TK_SCOPE`. It answers what
remains in the Plan; an ambient Epic Scope must not hide unfinished members.
`tk next --plan` still respects Epic Scope when selecting work.

The view groups Tickets into Ready, In progress, Waiting and Done, in that
order. Empty sections and a redundant Plan header are omitted. Each Ticket
appears once. Waiting explains why an unfinished member cannot be selected,
including unresolved blockers, triage or parking. Outside blockers are named
as such. Done members stay visible until removed or cleared.

The footer reports remaining and done/total counts over the whole Plan.

The MVP adds no Plan indicators or filters to `tk show` or `tk list`.
Release tagging and commit-derived release notes remain outside the Plan.

## Prototype evidence

The throwaway prototype is preserved on local branch
`prototype/tk-214-plan`, commit `2323781ea6f4f13a991074e98771128fec1d7ef1`,
at `crates/tk/src/commands/release-plan.prototype.html`. Its adjacent Markdown
file records the verdict. It compares three command families and terminal
views on the same fixture.
Variant A supplies the selected dedicated Plan commands and progress
sections. The prototype is evidence for the interaction, not a full model
of tk selection: Epic-mediated Dependencies and triage are not exercised.

Confirmed 2026-09-06. tk-214 owns this design decision; tk-215 owns production
implementation.
