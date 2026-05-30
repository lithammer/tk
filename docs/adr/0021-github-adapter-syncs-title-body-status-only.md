# The v1 GitHub Backend Adapter syncs title, body, and status only

The v1 GitHub Backend Adapter (tk-34) maps three things between tk and a
GitHub repository, both directions, through the `gh issue` subcommands:

- title and body (`gh issue edit`, from `update_ticket`),
- Item Status as a two-state axis — `done` ↔ CLOSED (`gh issue close`),
  `open`/`active` ↔ OPEN (`gh issue reopen`) — and back on Pull,
- the issue itself (`gh issue list --state all` → `BackendItemSnapshot`).

It does **not** sync Dependencies or Epic membership. Those Mutations are
still emitted for same-Backend item pairs (`add_dependency`,
`remove_ticket_from_epic`, …) and still reach the adapter, so the adapter
returns a no-op `ApplyOutcome::Accepted` for them: it records that the
Mutation was handled without driving any `gh` call. The local Dependency
edge and Epic membership already live in the Repository Store and keep
driving `tk next` and the read views; there is simply no backend action to
take.

## This is a scope choice, not a capability gap

GitHub *can* model both relationships, and both are generally available:

- Issue dependencies ("blocked by" / "blocking") — REST API, GA 2025-08-21.
- Sub-issues — GraphQL `addSubIssue` and REST, GA 2025.

The deferral rests on three reasons, in order of weight:

1. **Round-trip symmetry.** `gh issue` has no relationship flags, so even
   pushing a Dependency means dropping to raw `gh api` (REST for
   dependencies, GraphQL-with-feature-header for sub-issues). Pushing on
   Apply without reading relationships back on Pull yields an asymmetric
   half-sync — the edge exists locally and on GitHub but cannot be
   reconstructed from a fresh Pull. Closing that gap needs per-issue
   relationship fetches (N+1) or GraphQL batching, the exact complexity
   tk-34 scoped out.
2. **Epics are unreachable pre-Promote.** Sub-issues map to Epic
   membership, but no GitHub Backend Epic can exist in v1 — Promotion is
   not implemented — so building sub-issue sync now is speculative.
3. **`gh api` friction.** Sub-issue mutations through `gh api` have known
   defects (cli/cli#12258), and the relationship surface is a second
   integration with its own request shapes and error taxonomy.

## Considered Options

- **Full bidirectional dependency sync via `gh api` now.** Rejected:
  roughly doubles the slice (a second `gh api` surface, per-issue Pull
  fetches, distinct error shapes) for a relationship tk already treats as
  primarily local and selection-driving.
- **Apply-only dependency push via `gh api`.** Rejected: the half-sync
  asymmetry above is a durable wart, not a stepping stone.
- **Reject relationship Mutations.** Rejected: the v1 sync engine stops at
  the first `ApplyOutcome::Rejected`, so a single `tk block` between two
  GitHub Tickets would wedge the whole Mutation queue until `tk sync
  --skip`.

## Consequences

- Relationship intent is not reflected on GitHub in v1, and Dependency
  Mutations no-op-Accepted before relationship sync lands are already
  `applied` — they will not auto-replay when it does. A future slice
  decides whether to backfill.
- The full relationship surface (dependencies now; sub-issues once Promote
  lands; an optional GitHub issue-type → `TicketKind` mapping on Pull) is
  tracked by tk-107.
- `active` has no GitHub representation; Pull normalises it toward `open`.
  The resulting clobber of a locally-`active` Ticket is a defect tracked by
  tk-108. Remote reopens of an item already imported as `done` remain
  deferred per ADR-0006.
