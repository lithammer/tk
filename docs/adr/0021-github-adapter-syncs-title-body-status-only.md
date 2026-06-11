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
