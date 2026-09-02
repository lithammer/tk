# Work State splits out of Item Status

> **Amended by ADR-0044.** Backend Pull may preserve `title` and `body` for an
> Item with unresolved content intent, but it still imports Backend Lifecycle
> and a present Ticket Kind. `None` preserves the stored Kind, `done` clears
> Work State to `idle`, and a refresh keeps `updated_at` at the later of the
> stored value and refresh time.

`items.status` currently fuses two orthogonal concerns into one column: a
lifecycle the **Backend Adapter** shares with its **Backend** — open or
closed — and a purely local fact, whether someone is working the item right
now. gh-52 traces to that fusion: `tk start` and `tk stop` on a
Backend-bound Ticket each queue a `set_item_status` **Mutation** that tries
to push `active` to a **Backend Adapter** with no `active` to receive it,
and a
merged pull request that closes the underlying issue can land `done` on a row
an operator is still working. This decision splits the column:
`items.status` keeps only the shared lifecycle, `work_state` becomes a new
**Local Field**, and **Item Status** — the three-valued `open` / `active` /
`done` view CONTEXT.md defines and every command prints — becomes a
value derived from the two.

What the Adapter cannot express is narrower than what GitHub cannot
represent. ADR-0021 scopes the v1 GitHub Backend Adapter's Item Status axis
to `gh issue close` and `gh issue reopen` over GitHub's two-valued
`IssueState`, and its status Apply maps `done` to `close`, `open` and
`active` alike to `reopen`. `cli/cli`, read at tag `v2.98.0` to match the
installed `gh`, shows there was never a third verb to map:
`pkg/cmd/issue/issue.go` registers `close` and `reopen` as the only
state-mutating subcommands, and `pkg/cmd/issue/edit/edit.go` declares no
state flag at all. Whether `active` should earn an outward representation —
a GitHub Projects field, a Jira transition — is gh-68's question, and this
decision leaves it open.

## Decision

### `items.status` becomes Lifecycle; `work_state` becomes Work State

The stored lifecycle is two-valued — `open` or `done` — and is the only axis
a **Backend Adapter** observes or changes. `work_state` is a new
column holding `idle` or `active`: **Work State**, a **Local Field** never
applied to a **Backend** and never recorded as a **Mutation**. It applies to
Tickets and Epics alike — `tk start` and `tk stop` already act on an Epic —
unlike Ticket-only **Selection State** (ADR-0027). Only the relocated `active`
⟹ `accepted` conjunct stays inside the `item_class = 'ticket'` branch,
because **Selection State** is what that conjunct reads. **Item Status**
stops being a stored column and becomes a pure function of the two:
`done` when **Lifecycle** is `done`; otherwise `active` when **Work State**
is `active`, else `open`. CONTEXT.md's **Item Status** entry keeps its three
values — users still see three; only the storage and sync prose move.

Inverting the derivation this way — rather than narrowing **Item Status** to
two values and adding a separate flag — keeps rendering byte-identical.
`tk adopt` prints a `Status:` line through **Item Status**'s
`Display` impl and must keep printing `Status: active` for a Ticket someone
is working, exactly as it does today. It also turns a missed reader into a
compile error: **Item Status** loses its `FromSql` impl, so nothing can read
a raw `items.status`
value and mistake it for the derived view without going through the code that
combines both columns.

### Backend Pull imports Lifecycle; it clears Work State only as a consequence

**Backend Pull** may write `work_state` in exactly one circumstance: as the
local consequence of importing a closed **Lifecycle**, never to carry a
Backend-observed value into a **Local Field**. The **Backend Adapter** reads
no in-progress state, so there is nothing for Pull to import into
`work_state` — the column is simply not part of what Pull reads. But a closed
issue means the work is over, so Pull clears `work_state` to `idle` in the
same merge that lands `done`, the same way `tk done` clears it locally.

The same test governs any future addition to the refresh write: it may carry
a value Pull is authoritative for reading, or clear a **Local Field** as the
consequence of a **Lifecycle** transition Pull *is* authoritative for.

### Migration 011 rebuilds `items` and cancels manufactured status Mutations

Relocating a table-level CHECK is a rebuild: SQLite cannot alter one in
place, so migration 011 is a foreign-keys-off `items` rebuild in the shape
ADR-0028 records. It adds `work_state`, narrows the `status` CHECK to
`('open','done')`, moves ADR-0029's `active` ⟹ `accepted` conjunct onto
`work_state` inside the `item_class = 'ticket'` branch it already lives in,
and recreates every schema object ADR-0028 enumerates,
`items_no_escape_from_done` among them. Each `status = 'active'` row is
rewritten `status = 'open'` with
`work_state = 'active'`; every other row takes `work_state = 'idle'`. Because
one column could never hold both, no pre-existing row lands in the `(done,
active)` shape the two `done` writers exist to avoid.

Before this split, every Item Status transition on a Backend Ticket —
including `tk start` and `tk stop`, which never should have reached a Backend
at all — queued a `set_item_status` Mutation. Every already-queued `pending`
or `failed` `set_item_status` Mutation targeting `open` or `active` is tk's
own manufactured record of that defect, not deliberate user intent the way a
Promotion or a title edit is: nobody asked tk to push a "start working"
signal to a Backend. ADR-0038's human-only cancellation rule governs
Promotion Cancellation, which withdraws an operation a human explicitly asked
for; it does not extend to rows that exist only because of a defect this
split removes. Migration 011 cancels these non-closing `set_item_status` rows
directly.

A Mutation carrying a Promotion Operation id is left queued instead. It
belongs to an operation ADR-0038 already governs, and silently rewriting a row
that operation may still act on would blur the same human-decides line
ADR-0038 draws for Promotion itself. The GitHub Adapter rejects a non-closing
`set_item_status` target outright, so these surviving rows fail at Apply and
the operator's exit is the ordinary one: `tk sync --skip`.

That carve-out is a finite population only because Promotion stops minting
new members. The Promotion planner drafts a status Mutation for an Item that
is `active` or `done`; after the split it drafts one only for `done`, since
Work State is not a Backend concern. Narrowing the planner's own status field
to **Lifecycle** is what forces this: the planner's `active` arm stops
compiling, rather than silently continuing to draft a target the Adapter now
rejects.

### A migration may write `mutations.state`, under two conditions

`store::mutations::transition` is the only *runtime* writer of
`mutations.state`. A migration is not a runtime workflow and has no
`transition` to call, so it writes the column directly — but only along an
edge `MutationState`'s transition table already names, and its SQL must cite
that row. The table then stays a complete index of the edge's callers, which
is what `transition`'s monopoly was for. Migration 011 is
the first migration to rely on this, taking the `pending, failed` →
`cancelled` edge.

That gives `cancelled` a third provenance. ADR-0038 introduced it to keep two
apart — a Mutation a human skipped after a failure, and one a human withdrew
before any attempt — and migration 011 adds a third with no human behind it
at all: a row tk queued from its own defect and now withdraws unasked. The
operator sees this. `cancelled` joins the Sync Log's default filter, so the
first `tk sync log` after upgrade lists withdrawals nobody requested, at the
timestamps described below.

### `state_changed_at` is left untouched on cancelled rows

`store::mutations::transition` normally stamps it on every edge it takes.
Stamping it here would mean one of two things: threading `apply_one_txn`'s
`now_iso` into a second statement outside `mig.sql`, since `execute_batch`
takes a plain SQL string with no parameter slot; or letting SQLite's own clock
write a stored timestamp. `Clock` is the single seam every stored timestamp in
the **Repository Store** goes through, and a second time source is not worth
one migration's tidiness.

The cost is bounded: `tk sync log` reports the Mutation's original append
time for these rows, not the moment migration 011 cancelled them.

## Considered Options

**Narrowing Item Status to two values and adding a separate flag.** Rejected:
it puts the churn on rendering rather than on the storage layer,
which is where the change actually is. The derivation recorded above avoids
it.

**A queue-time representability check**, refusing to queue a
`set_item_status` Mutation whenever its target is one the Adapter cannot
represent. Rejected: it under-fixes. `tk stop` targets `open`, and `open`
*is* representable, so a representability check would still let `tk stop` on
a Backend Ticket queue a Mutation — one that shields the Item from **Backend
Pull** for as long as it stays `pending`. The defect is not that one
particular target is unrepresentable; it is that Work State was never a
Backend concern to begin with, which a representability check does not know.

**A Mutation-type-aware Backend Pull shield**, changing which Mutation Types
withhold an Item from Pull. Rejected as out of scope: that is tk-157's
decision, and settling it as a side effect of a push-side fix would prejudge
a question this work leaves open.

**A cross-field CHECK, `status = 'done'` implies `work_state = 'idle'`.**
Rejected: a CHECK aborts the whole write it appears in, and the Pull merge
collects every refresh before its one merge transaction, as ADR-0034
records. A merged pull request
that closes an issue whose local row is `open` and `active` would then abort
the whole batch — including every other Item's refresh riding the same
transaction — on exactly the row the merge exists to resolve. Both `done`
writers, `tk done` and **Backend Pull**, clear Work State themselves instead,
so `(done, active)` is unreachable without ever asking a CHECK to abort a
batch write to prevent it. A write can choose not to produce a state; a CHECK
can only refuse one already produced.

## Consequences

- **Item Status** loses `FromSql`. That makes every row-mapping reader a
  compile error until it fetches both columns, but it cannot reach a SQL
  string literal — so the predicates below are guarded only by tests over
  those views, which they need.
- The **Backend Adapter** read contract narrows with the column:
  `AdoptedItem`'s and `BackendItemRefresh`'s status fields become
  **Lifecycle**, so an Adapter cannot construct an `active` refresh at all,
  and the Promotion graph's status field narrows for the same reason.
  `MutationPayload`'s `StatusChange` deliberately keeps its string status:
  migration 011 leaves Promotion-carried `open`/`active` rows queued to fail
  at Apply, so the payload must stay able to express the target the Adapter
  then rejects.
- Splitting a fused column leaves two classes of predicate, and only one of
  them announces itself. A predicate naming the departing value —
  `status in ('open','active')` — stops matching and breaks visibly. A
  predicate spelling `status = 'open'` to mean *not started* stays valid SQL
  and silently widens: after the split an in-progress Ticket satisfies it, so
  `tk list --ready` lists work already underway and `tk next` recommends it,
  violating CONTEXT.md's rule that a Ticket is ready only when its **Item
  Status** is `open`. Every such site needs `and work_state = 'idle'`. The
  `triage` and `parked` arms are the exception that shows why: they pair
  `status = 'open'` with a non-`accepted` **Selection State**, which the
  relocated ADR-0029 conjunct keeps out of `active`, and `ready` requires
  `accepted` so it inherits no such protection. The site inventory belongs to
  the implementing work, not here.
- Amends ADR-0029: the `active` ⟹ `accepted` conjunct now reads `work_state`.
  It is relocated, not retired.
- Amends ADR-0021: outbound Item Status push becomes close-only; the
  `open`/`active` → OPEN direction it recorded as bidirectional is retired.
- Amends ADR-0034: the **Backend Pull** working set it spells
  `status in ('open','active')` becomes `status = 'open'`.
- Amends ADR-0038: `cancelled` no longer separates two provenances but three,
  the third being a withdrawal tk performs on its own behalf.
- Amends ARCHITECTURE.md: `store::mutations::transition` is *the only* writer
  of the `state` column, owning the `failure_json` and `state_changed_at`
  bookkeeping each edge implies, becomes true only of runtime writers.
- gh-52 closes only its expressibility half: pushing Work State to a Backend
  becomes structurally impossible rather than discouraged. ADR-0044 narrows
  the **Backend Pull** shield to title/body, so a pending content Mutation no
  longer hides a Backend close from Pull. ADR-0046 owns a skipped failed close;
  tk-198 owns skipped content on a done Item. General remote reopen remains
  deferred. gh-68 becomes a cleaner question once Work State could carry its
  own Mutation Type.
