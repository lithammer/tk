# Promotion intent precedes Backend capability

tk-136 builds the whole local half of Promotion — preflight, the ordered
outbox, and receipt application — but the delivery order in tk-135 places safe
non-idempotent Apply (tk-139) and GitHub issue creation (tk-137) after it. That
slice must therefore produce durable Promotion intent that no Backend in the
same build is allowed to act on, and it must do so without leaving a shape that
the two following slices have to unpick.

## Decision

### Promotion Mutations target Local items

A `promote_ticket` or `promote_epic` Mutation names an Item whose Origin is
still local; that is the whole point of the operation. The rule that decides
whether a current-state write also appends backend intent therefore stops
reading Origin and reads a derived **Pending Promotion** state instead: an Item
appends Mutations when it is a Backend Item, or when it is a Local Item whose
Promotion is already in the Mutation Log. CONTEXT.md's **Mutation** and
**Ticket Mutation** entries and the "Origin gates Mutations" paragraph in
ARCHITECTURE.md are amended to match.

The Promotion payload records the Backend the operation targets, alongside the
title and body snapshot. Resolving the Pending Promotion state then reads the
Mutation Log alone, and no Repository Store write path consults Remote
configuration to decide whether two Dependency endpoints share a Backend. This
is load-bearing rather than tidy: nothing below the command layer prevents a
store from holding `github` Items under a `jira` Remote. `remotes.backend_kind`
admits both, `items.backend_kind` carries no CHECK, and `clear_remote` leaves
Backend Items intact. The only guard is a usage error in `tk remote set`, which
the Jira adapter (tk-35) removes.

### Backend capability is declared per facet and staged

A Backend Adapter declares, as static typed data, whether it can represent each
Item class, Ticket Kind, Dependency, and Epic membership under Promotion.
Preflight reads that declaration and rejects before any backend call, and the
rejection aggregates with every other finding rather than short-circuiting.

The GitHub Adapter declares no Promotion support in the slice that introduces
the planner. tk-137 turns on Task and Epic creation once `gh issue create` is
implemented; tk-132 turns on Epic membership once sub-issue Apply is
implemented. Bug stays off until GitHub can represent the closed Ticket Kind
reliably.

Staging is a safety property, not sequencing preference. Sync stops at the
first rejected Mutation, a Promotion Mutation is not a Sync Skip candidate, and
`tk remote clear` refuses while any Mutation is pending or failed. An Adapter
that accepted Promotion intent it could not apply would therefore leave the
whole outbox stopped — including unrelated Mutations for Adopted Backend
Tickets — with no in-product remedy until the user installed a later build.
Refusing at preflight leaves the outbox empty instead.

### Promotion applies through the existing Apply seam

`Adapter::apply_mutation` carries Promotion like every other Mutation Type, and
the Mutation Receipt becomes a typed enum whose Promotion variant carries the
Adapter-owned backend key and Display ID as required fields. Receipt
application commits in the same transaction that marks the Mutation applied and
advances the Sync Cursor, so no window exists in which a Mutation is applied
while its Item is still Local.

A separate Adapter method for non-idempotent creation is deferred to tk-137.
The receipt's *success* shape is fixed by the schema and is modelled now; the
certified-no-effect versus indeterminate *failure* taxonomy depends on what
`gh issue create` is observed to emit, so it is deferred to the slice that
gathers that evidence.

## Considered Options

**Let the GitHub Adapter accept Promotion and reject it at Apply.** Rejected.
The rejection stops the apply loop, the Mutation cannot be skipped, and the
Remote cannot be cleared while it is pending — so a single `tk promote` would
leave a permanently stopped sync with no in-product exit.

**Add the `applying` Mutation State in the same migration as the Promotion
Operation column.** Rejected. ADR-0028's "cheaper to bake in once" applies when
a table rebuild is already required for an independent reason, as it was for
migration 005; the Promotion Operation column is a nullable `ALTER TABLE ADD
COLUMN` and forces no rebuild. The pairing between `applying` and the
`state`/`failure_json` CHECK also depends on tk-139's transition order, so
choosing it here risks a second rebuild anyway.

**Split the Adapter trait into edit and create seams now.** Rejected. Only the
success half of the split is schema-determined; committing the failure taxonomy
before tk-137's evidence is the case ADR-0016 set the precedent against. The
typed receipt already makes a Promotion receipt without a backend key
unrepresentable, which is where the unrecoverable state would otherwise be
produced.

**Resolve backend identity when the applicable Mutations are loaded.**
Rejected. A receipt applied part-way through a run changes the identity of the
Items that later Mutations in the same run target, so identities captured at
load time are stale by construction. Identity is resolved per Mutation,
immediately before Apply, while the decode pass stays batched so an
undecodable row still fails the run before any backend write.

**Keep gating on Origin by flipping a promoted Item to Backend Origin at commit
time.** Rejected. The Item has no backend identity until its receipt arrives,
and the `items` CHECK requires `backend_kind` and `backend_key` to be present
whenever Origin is `backend`.

## Consequences

- `tk promote` against a configured GitHub Remote refuses until tk-137. Every
  preflight refusal is exercisable as a scenario, because no path to the
  refusal spawns `gh`; everything past preflight is covered by Fake Adapter
  tests until then.
- Pending Promotion Items exist only in tests for the same interval, so the
  Pending Promotion gate is validated against Mutation Log rows the outbox
  commit actually wrote rather than against hand-built fixtures.
- Three GitHub Adapter comments assert premises this work falsifies: the
  `unreachable!` arm for the `promote_*` Mutation Types, the two `expect` calls
  on `backend_key` and `counterpart_backend_key`, and the Epic-membership arm's
  claim that no GitHub Backend Epic can exist pre-Promotion. Each changes in
  the slice that falsifies it rather than in the slice that first reaches it.
- A receipt that cannot be persisted — a Display ID collision against an
  existing `item_ids` row — rolls back with the Mutation still applicable, so a
  later sync would create a second backend object. Staging keeps this
  unreachable here; tk-139 owns it, and needs a way to free or choose the
  Display ID, because reconciling re-attaches into the same collision.
