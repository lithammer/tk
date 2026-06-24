# The v1 GitHub Backend Adapter syncs item fields, not relationships

The v1 GitHub Backend Adapter (tk-34) maps item *fields* between tk and a
GitHub repository through the `gh issue` subcommands:

- title and body — bidirectional (`gh issue edit`, from `update_ticket`; read
  back on Pull),
- Item Status as a two-state axis — `done` ↔ CLOSED (`gh issue close`),
  `open`/`active` ↔ OPEN (`gh issue reopen`) — bidirectional,
- Ticket Kind — Pull-only: the `issueType` `--json` field maps to
  `TicketKind` (`"Bug"` → `Bug`; every other value — incl. `"Task"`,
  `"Feature"`, org-custom types, and a typeless issue — → `Task`, matching the
  closed two-variant `TicketKind`). No Mutation changes a Ticket's Kind, so
  there is nothing to push; `--type` is never written in v1,
- the issue itself (`gh issue list --state all` → `BackendItemSnapshot`).

It does **not** sync Dependencies or Epic membership. Those Mutations are
still emitted for same-Backend item pairs (`add_dependency`,
`remove_ticket_from_epic`, …) and still reach the adapter, so the adapter
returns a no-op `ApplyOutcome::Accepted` for them: it records that the
Mutation was handled without driving any `gh` call. The local Dependency
edge and Epic membership already live in the Repository Store and keep
driving `tk next` and the read views; there is simply no backend action to
take in v1.

## This is a scope choice, not a capability gap

`gh` 2.94.0 (cli/cli#13057, "Add Issues 2.0 support") makes every
relationship first-class in `gh issue` — no raw `gh api` required:

- Dependencies — `gh issue edit --add-blocked-by`/`--remove-blocked-by`
  (and `--add-blocking`/`--remove-blocking`), read back via the
  `blockedBy`/`blocking` `--json` fields.
- Sub-issues / Epic membership — `gh issue edit --parent`/`--remove-parent`/
  `--add-sub-issue`/`--remove-sub-issue`, read back via `parent`/`subIssues`.

The deferral therefore rests on scope, not capability:

1. **Slice size.** tk-34 is already sizable and blocked on tk-106
   (`tk remote set`); widening it to a second sync axis — two more Apply arms
   plus the relationship fields on Pull — is held out so the first adapter
   ships bounded. Dependency sync is now cheap and symmetric (native push
   *and* read-back), so deferring it front-loads no technical risk; it is
   purely a question of where the line falls. tk-107 owns it.
2. **Epics are unreachable pre-Promote.** Sub-issues map to Epic membership,
   but no GitHub Backend Epic can exist in v1 — Promotion is not implemented,
   Pull hardcodes `item_class:Ticket`, and `gh issue create` is out of scope —
   so there is no parent number to point `--parent` at. Sub-issue sync is
   gated on the Promote slice regardless of `gh`'s capabilities.

## Considered Options

- **Fold dependency sync into tk-34 now.** Rejected: it adds a second sync
  axis (two Apply arms, the `blockedBy`/`blocking` Pull fields) to a slice
  already blocked and sizable. The native flags make it cheap and symmetric,
  so it defers without technical risk — the only cost is the backfill debt
  below.
- **Reject relationship Mutations.** Rejected: the v1 sync engine stops at
  the first `ApplyOutcome::Rejected`, so a single `tk block` between two
  GitHub Tickets would wedge the whole Mutation queue until `tk sync --skip`.
  No-op-`Accepted` keeps the queue draining.

## Consequences

- Relationship intent is not reflected on GitHub in v1, and Dependency
  Mutations no-op-`Accepted` before relationship sync lands are already
  `applied` — they will not auto-replay when tk-107 ships. A future slice
  decides whether to backfill.
- The relationship surface — dependencies, and sub-issues once Promote lands —
  is tracked by tk-107, now via the native `gh issue` flags above rather than
  raw `gh api`.
- `active` has no GitHub representation; Pull normalises it toward `open`.
  The resulting clobber of a locally-`active` Ticket is a defect tracked by
  tk-108. Remote reopens of an item already imported as `done` remain
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
  their no-op-Accepted arms. The Mutation sits on the blocked item
  (`backend_key`); the blocking item's number is resolved from its internal
  `items.id` at Mutation-load time onto `MutationView.counterpart_backend_key`,
  so the adapter stays store-free. Three consequences worth recording:
  - **Title/body/status round-trip; dependencies do not.** Backend Pull does
    not reconstruct Dependency edges in v1 — relationship sync is one
    directional (local intent → GitHub). Read-back is deferred to its own
    ticket because ADR-0034's opt-in working set makes edge reconciliation a
    distinct problem: only an edge whose *both* endpoints are Adopted could
    round-trip, and an edge to an un-Adopted issue has no local item to point
    at. This asymmetry is the deliberate v1 shape, not an oversight.
  - **Dependencies become ordinary wedge-on-`Rejected` Mutations.** This ADR's
    earlier "no-op-Accepted keeps the queue draining" rationale was a
    workaround for having no real apply; now that one exists, a failed
    dependency Apply stops the Apply loop at its `sequence` like any
    `update_ticket`, recovered by fixing the cause or `tk sync --skip`. No
    backfill of dependency Mutations that no-op-Accepted before this slice.
  - **Sub-issue / Epic-membership sync stays deferred and splits to a
    Promote-gated ticket.** No GitHub Backend Epic can exist pre-Promotion
    (Pull hardcodes `item_class: Ticket`, `tk adopt` inserts Tickets), so
    `add_ticket_to_epic` is unreachable; its arm stays no-op-Accepted. It will
    reuse the same `counterpart_backend_key` resolution (`EpicRef` → parent
    number) once backend Epics are real.
