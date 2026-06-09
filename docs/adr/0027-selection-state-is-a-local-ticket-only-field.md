# Selection State is a local, Ticket-only field separate from Item Status

The tk-72 epic adds **Selection State**, a Ticket-only field with three
values — `triage`, `accepted`, and `parked` — that decides whether an open
Ticket is *selectable now*. `accepted` is the default for normal `tk add`
and for newly imported Backend Tickets: real work that can become ready for
`tk next`. `triage` is captured-but-unaccepted work that needs a human
decision before it becomes selectable. `parked` is accepted work
intentionally held out of automatic selection until unparked.

We model Selection State as a new nullable `items.selection_state` column —
a **Local Field**, stored current state like `priority` and `closing_reason`
(ADR-0023) — and explicitly **not** as an Item Status, a Priority, or a
Backend-synced field. `tk next` and `tk list --ready` select only `accepted`
open Tickets; `triage` and `parked` Tickets are excluded both as direct
candidates and as Effective Priority contributors (ADR-0015).

Selection State answers a different question from Item Status. `open`,
`active`, and `done` are **lifecycle** (ADR-0006); triage and parking are
local **selection policy**. A captured idea is not "in progress" or "closed",
and a parked Ticket is still open work — folding either into Item Status
would overload lifecycle and weaken the done-is-terminal rule. It also
answers a different question from execution mode: AFK/HITL would decide
whether a Ticket suits an unattended agent; Selection State decides whether
it is selectable at all. Execution mode is out of scope here.

## Considered Options

- **Use a low Priority (e.g. `P4`) for held work.** Rejected: a `P4` Ticket
  is still real accepted work that should eventually be selected. Priority
  ranks accepted work; it cannot also mean "not accepted yet" without
  overloading ranking with acceptance.
- **Add a fourth Item Status.** Rejected: lifecycle and selection policy are
  orthogonal. A parked Ticket has lifecycle `open`; a triage bug is still a
  bug. Collapsing policy into lifecycle loses that and complicates every
  Item Status transition.
- **Make Priority optional and treat "no Priority" as untriaged.** Rejected:
  this overloads the ranking field with an acceptance decision and leaves
  `parked` (accepted, ranked, but held) unrepresentable.
- **Map Selection State to a Backend field or carry it on a Mutation.**
  Rejected. There is no clean GitHub/Jira mapping for "locally held", and
  pushing it one-way is the same half-sync asymmetry ADR-0021 declined for
  relationships and ADR-0023 declined for the Closing Reason. Parking a
  Backend Ticket in one checkout must claim nothing about upstream workflow.

## Consequences

- New nullable `items.selection_state` column added via `ALTER TABLE ADD
  COLUMN` with a cross-column CHECK that rejects a value on an Epic
  (`item_class = 'epic'` ⟹ `NULL`) and an unknown value on a Ticket
  (`item_class = 'ticket'` ⟹ value in `triage`/`accepted`/`parked`). Epics
  stay outside the field and keep null Priority. The forward migration
  backfills every existing Ticket to `accepted` without bumping `updated_at`,
  so the field is total over Tickets from the moment it exists and the
  migration does not read as a user edit.
- Selection State is **local-only**: `accept`, `park`, and `unpark` will
  update current state without appending a Mutation, the change is never
  pushed to a Remote, and Backend Pull preserves it for existing Backend
  Tickets (finished in tk-77). The Mutation Log and the V1 Mutation Type
  list are unchanged; the Mutation Log stays backend intent, not a local
  audit history.
- `tk show` renders `Selection: <state>` for Tickets on its own line under
  the facet bar; the shared `item_header` facet bar (reused by `tk grep`,
  ADR-0026) is left untouched so content search stays free of Selection
  State noise. The `[triage]`/`[parked]` list badges and the focused list
  filters land in later tk-72 slices.
- **Staging (this slice, tk-73).** tk-73 keeps the standing
  Ticket-requires-Priority CHECK intact, so the acceptance criterion
  "accepted Tickets require Priority" holds transitively — every Ticket
  still requires a Priority. Two strict invariants are deferred to a single
  SQLite table rebuild in tk-74, the slice that first needs it:
  - The combined `triage ⟺ null Priority` (and `accepted`/`parked` ⟹
    non-null Priority), which only becomes expressible once triage can
    create a null-Priority Ticket.
  - The strict `ticket ⟹ non-null selection_state`. `ALTER TABLE ADD
    COLUMN` validates its CHECK against existing rows, which hold a transient
    `NULL` before the backfill runs, so a CHECK that forbids `NULL` on a
    Ticket would abort the migration. tk-73 therefore relies on the writers
    plus the backfill to keep Selection State total over Tickets, and tk-74's
    rebuild promotes that to a schema guarantee.

  Between tk-73 and tk-74 the schema permits `selection_state = 'triage'` on
  a Ticket that still carries a Priority, and a `NULL` `selection_state` on a
  Ticket — intentionally unreachable intermediate states, because no command
  produces them, which tk-74's rebuild forecloses.
