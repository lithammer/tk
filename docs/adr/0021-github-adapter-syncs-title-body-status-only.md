# The v1 GitHub Backend Adapter syncs fields and pushes relationships

The v1 GitHub Backend Adapter (tk-34) maps item *fields* between tk and a
GitHub repository, chiefly through the `gh issue` subcommands:

- title and body — bidirectional for Backend Tickets and Backend Epics (`gh
  issue edit`, from `update_ticket` / `update_epic`; read back on Pull),
- Item Status as a two-state axis — `done` ↔ CLOSED (`gh issue close`),
  `open`/`active` ↔ OPEN (`gh issue reopen`) — bidirectional,
- Ticket Kind — Promotion sets the initial Bug representation described below,
  and Pull maps the current Backend representation to `TicketKind`. No later
  Mutation pushes Kind, so Pull remains authoritative after creation,
- the issue refresh (`gh issue view <n>` → `BackendItemRefresh`).

GitHub Bug Promotion prefers a usable native `Bug` Issue Type for any
repository. When that capability is unavailable, a personal repository falls
back to an existing `bug` Label; an organization repository rejects preflight.
A native Issue Type or Label counts as the reserved representation only when
its name equals `Bug` case-insensitively. Renamed, localized, prefixed, or
otherwise heuristic aliases are not inferred. A missing representation rejects
preflight rather than causing tk to create repository taxonomy.

The personal-repository `bug` Label is private Backend Adapter encoding for
Ticket Kind, not general Label support in tk. It adds no Label field, command,
or Mutation Type to the Repository Store; tk-21 retains ownership of those
domain and interface decisions.

Backend Pull remains authoritative for Ticket Kind after Promotion. A
personal repository treats a present native Issue Type as authoritative: `Bug`
maps to Bug, while every other native type maps to Task. The reserved `bug`
Label maps to Bug only when the issue is typeless, so a later GitHub rollout
does not invalidate older labeled Bugs. Removing the only representation, or
changing a native Issue Type away from `Bug`, refreshes the local Ticket as
`Task`; tk does not restore the previous Backend representation.

The installed `gh` 2.98.0 source exposes `issueType` and `labels`, but not
repository ownership, through `gh issue view --json`; `gh repo view --json`
separately exposes `isInOrganization`. Adopt, Pull, and Promotion
Reconciliation retain the existing issue-read and identity-validation path.
Only a typeless issue carrying the reserved Label needs the additional
repository-ownership read, whose result the Adapter caches by repository for
that invocation. Native types and ordinary typeless Tasks require no ownership
call.

Promotion Reconciliation also maps the candidate's Ticket Kind through that
repository-specific representation. The mapped Kind must equal the retained
Promotion's Kind, for both Task and Bug, and a mismatch is refused even with
`--force`; the user must correct it on GitHub before reconciling.

A 2026-08-24 live probe against the user-owned
`lithammer/tk-gh-playground` repository with `gh` 2.98.0 showed why capability
must match the creation path. The REST repository Issue Types endpoint listed
enabled Task, Bug, and Feature types, but `gh issue edit --type Bug` resolved
no available types and wrote nothing. Repository ownership selects whether
the Label fallback is allowed; the REST listing alone does not prove native
Issue Type capability. A REST create with an unavailable `type` then created
issue #24 successfully but silently left it typeless; the probe issue was
closed after inspection. In contrast, a direct `createIssue` GraphQL mutation
with an invalid Issue Type ID returned no issue, and a title search confirmed
that it created nothing. The REST result is observed behavior, not a claim
from `gh` source; the GraphQL input contract is the API primitive that can
couple representation and creation in one mutation.

Bug Promotion therefore uses one direct GraphQL `createIssue` path with an
exclusive representation proved during preflight: either the native Issue Type
or the fallback Label, never both. Apply resolves that representation's current
ID. Task and Epic Promotion retain their shipped `gh issue create` path. A
future move to `gh issue create --type Bug` requires both usable
personal-repository Issue Types and an upstream single-create contract that
cannot strand a typeless issue before returning its receipt; personal
availability alone is insufficient. The revisit is tracked by gh-49.

Every Promotion calls the requirements-aware capability resolver established
by ADR-0036. The GitHub Adapter satisfies static facets without I/O; when Bug
is requested, it performs a fallible, repository-aware read to resolve
repository ownership and currently usable Bug representations. It returns a
plain `PromotionCapabilities` value to the pure Promotion planner. A
successful inspection with no usable representation produces the ordinary
unsupported-Bug preflight finding. Authentication, transport, and
malformed-response failures remain Adapter read errors rather than being
reported as unsupported capability. Repository-specific GitHub knowledge does
not enter the command or planner.

Native Issue Type absence requires an exhaustive direct GraphQL read. GitHub's
schema makes `Repository.issueTypes` nullable. An initial null is Adapter
policy for no native Issue Type representation available to this caller, and
is terminal native absence. A non-null connection still requires reading each
`issueTypes` page, including `isEnabled` and `pageInfo`, until `hasNextPage` is
false; only then may it report that no enabled case-insensitive `Bug` exists.
Null after a promised next page, omitted or malformed fields, GraphQL errors,
and failed pages are Adapter read errors. It must not use `gh` 2.98.0's
`RepoIssueTypes` helper as this proof because that helper requests only the
first 50 nodes and no page information. Apply uses the same exhaustive
resolver before creation.

The Issue Type or Label ID is not durable Promotion intent and is not
persisted. Preflight proves that the repository can currently represent Bug;
Apply resolves the current ID again immediately before the atomic mutation. If
that read finds the representation missing, Apply rejects without creating an
issue and ordinary sync can retry after the repository taxonomy is repaired.
Once `createIssue` starts, the ADR-0036 receipt contract still applies: a
canonical URL proves creation, while a result without one is indeterminate
unless the Adapter can prove that creation did not run.

Ticket Kind is also not duplicated in the Promotion payload. It is immutable
current state on the targeted Repository Store Item, so Apply and Promotion
Reconciliation read it there when constructing or validating the Backend
operation. Title and body remain retained in the Promotion payload because
later local edits can change them while the original creation snapshot stays
fixed.

Dependencies and Epic membership are push-only. `add_dependency` and
`remove_dependency` edit the blocked issue's dependency edge.
`add_ticket_to_epic` sets the Ticket's parent; `remove_ticket_from_epic`
clears it. Backend Pull does not reconcile either relationship from GitHub.
The local edge or membership in the Repository Store remains authoritative for
tk's read views and `tk next`.

## Relationship deferral was a scope choice, not a capability gap

`gh` 2.94.0 (cli/cli#13057, "Add Issues 2.0 support") makes every
relationship first-class in `gh issue` — no raw `gh api` required:

- Dependencies — `gh issue edit --add-blocked-by`/`--remove-blocked-by`
  (and `--add-blocking`/`--remove-blocking`), read back via the
  `blockedBy`/`blocking` `--json` fields.
- Sub-issues / Epic membership — `gh issue edit --parent`/`--remove-parent`/
  `--add-sub-issue`/`--remove-sub-issue`, read back via `parent`/`subIssues`.

The original deferral rested on scope, not capability:

1. **Slice size.** tk-34 is already sizable and blocked on tk-106
   (`tk remote set`); widening it to a second sync axis — two more Apply arms
   plus the relationship fields on Pull — is held out so the first adapter
   ships bounded. The native CLI makes both push and read-back possible, so
   deferring it front-loads no technical risk; it is purely a question of
   where the line falls. tk-107 owns it.
2. **Epics were unreachable pre-Promote.** Sub-issues map to Epic membership,
   but no GitHub Backend Epic existed before the Promotion slice, so there was
   no parent number for `--parent`. Sub-issue sync was therefore gated on
   Promotion regardless of `gh`'s capabilities.

## Considered Options

- **Fold dependency sync into tk-34 now.** Rejected: it adds a second sync
  axis (two Apply arms, the `blockedBy`/`blocking` Pull fields) to a slice
  already blocked and sizable. The native relationship surface makes both
  directions possible, so it defers without technical risk — the only cost is
  the backfill debt below.
- **Reject relationship Mutations.** Rejected: the v1 sync engine stops at
  the first `BackendEditOutcome::Rejected`, so a single `tk block` between two
  GitHub Tickets would wedge the whole Mutation queue until `tk sync --skip`.
  No-op-`Acknowledged` keeps the queue draining.

## Consequences

- Relationship Mutations no-op-`Acknowledged` before their real Apply arms
  landed are already `applied` and do not auto-replay. A future slice decides
  whether to backfill them.
- Dependency and Epic-membership Apply use the native `gh issue` flags above
  rather than raw `gh api`. Backend Pull does not read either relationship;
  reconciliation remains a separate concern.
- `active` has no GitHub representation, so an incoming OPEN says only that
  the issue is not closed. Pull therefore never resets a locally-`active`
  Ticket; it writes `open` only where the local status was already `open`
  (tk-108). Remote reopens of an item already imported as `done` remain
  deferred per ADR-0006.

## History

- Originally (tk-34, 2026-05) this ADR deferred relationship sync partly as a
  capability gap: `gh issue` had no relationship flags, so any push meant raw
  `gh api`, and tk-107 was framed around that mechanism. `gh` 2.94.0
  (cli/cli#13057, 2026-06-10) made relationships and issue types native to
  `gh issue`, so the deferral became purely a slice-size choice, issue-type →
  `TicketKind` moved into tk-34's Pull, and tk-107 was re-scoped to the native
  flags.
- (tk-34, 2026-06) The Pull *mechanism* this ADR specified — a single
  `gh issue list --state all` mirroring every issue into the Repository Store —
  is superseded by ADR-0034: tk syncs an opt-in working set, so Backend Pull
  refreshes only the Adopted items by key (`gh issue view <n>`) and never
  discovers un-Adopted issues. The field set, the two-state status axis, the
  relationship-deferral decision, and the issueType → `TicketKind` mapping
  recorded above are unaffected — only the list-everything ingest is replaced.
- (tk-107, 2026-06) Dependency sync lands, **push-only**. Apply for
  `add_dependency` / `remove_dependency` drives the native `gh issue edit
  --add-blocked-by` / `--remove-blocked-by` flags (`gh` ≥ 2.94.0), replacing
  their no-op-Acknowledged arms. The Mutation sits on the blocked item
  identity; the blocking item's identity is resolved from its internal
  `items.id` immediately before delivery, and both endpoints ride the typed
  `BackendEdit::AddDependency`, so the adapter stays store-free. Three
  consequences worth recording:
  - **Title/body/status round-trip; dependencies do not.** Backend Pull does
    not reconstruct Dependency edges in v1 — relationship sync is one
    directional (local intent → GitHub). Read-back is deferred to its own
    ticket because ADR-0034's opt-in working set makes edge reconciliation a
    distinct problem: only an edge whose *both* endpoints are Adopted could
    round-trip, and an edge to an un-Adopted issue has no local item to point
    at. This asymmetry is the deliberate v1 shape, not an oversight.
  - **Dependencies become ordinary wedge-on-`Rejected` Mutations.** This ADR's
    earlier "no-op-Acknowledged keeps the queue draining" rationale was a
    workaround for having no real apply; now that one exists, a failed
    dependency Apply stops the Apply loop at its `sequence` like any
    `update_ticket`, recovered by fixing the cause or `tk sync --skip`. No
    backfill of dependency Mutations that no-op-Acknowledged before this slice.
  - **Sub-issue / Epic-membership sync stayed deferred to Promotion.** No
    GitHub Backend Epic could exist pre-Promotion (`tk adopt` inserts Tickets),
    so `add_ticket_to_epic` remained unreachable and no-op-Acknowledged while
    no real parent identity existed.
- (tk-132, 2026-08) Epic field and membership Apply lands, **push-only**.
  `update_epic` uses the same title/body edit contract as `update_ticket`.
  `add_ticket_to_epic` sets the Ticket's parent with `gh issue edit --parent`,
  resolving the Epic's identity in the Repository Store so the adapter stays
  store-free. `remove_ticket_from_epic` uses `--remove-parent` on the Ticket
  and names no counterpart at all. Two reasons, the second observed rather than
  reasoned:
  - Epic membership is 0..1, so a Ticket's cleared `container_id` is the whole
    intent. Every Apply arm therefore edits the Mutation's own item, and a
    removal needs no Epic identity to be addressable — which also means a
    removal cannot be stopped by an Epic that has no Backend identity yet.
  - The alternative, `--remove-sub-issue <ticket>` against the Epic tk
    expected, was **observed to exit 0 without changing a divergent parent**
    (spike, `gh` 2.97.0): it reports a removal that never happened and leaves
    the Repository Store and GitHub disagreeing. `--remove-parent` is
    idempotent on a parentless issue and clears whichever parent is attached,
    so the push converges. Since Backend Pull is field-only, tk cannot see a
    divergent parent to reconcile, which makes converging the only honest
    option.

  Backend Pull remains field-only and does not reconcile parent or sub-issue
  data, though the spike confirms `--json parent,subIssues` is available to a
  future reconciliation slice.
- (tk-137, 2026-08) GitHub Promotion lands. Task Tickets and Epics share the
  typeless `gh issue create --title ... --body ...` surface; no `--type` or
  relationship flag rides creation. The Adapter declares Ticket, Epic, Task,
  Dependency, and Epic-membership capabilities together, because the latter
  two already have real Apply arms. Bug remains unsupported. A trustworthy
  issue URL is the creation receipt; completed results without one are
  indeterminate except for the observed initial 401/bad-credentials frame.
  Backend Pull remains field-only, so relationship reconciliation is still
  out of scope.
  Adopt and Promotion retain the canonical issue URL as the GitHub backend key;
  future view/edit/relationship calls therefore stay pinned to that repository.
- (tk-108, 2026-08) The `active` clobber this ADR recorded as a defect is
  fixed. Because `open` and `active` are one Backend state, an incoming OPEN
  carries no evidence that locally started work stopped, so Backend Pull's
  merge keeps a local `active` instead of resetting it; CLOSED still
  lands `done` from either local status. The `active` implies `accepted`
  clamp (ADR-0029) is unchanged and still takes precedence for an incoming
  `active` on a non-accepted Ticket. In the field the clobber appeared on the
  *second* `tk sync` after `tk start`: the first is protected by the merge's
  in-flight guard while the Ticket's own status Mutation is still queued,
  which is why it survived earlier lifecycle checks.
