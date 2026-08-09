# Promotion intent precedes Backend capability

tk-136 builds the whole local half of Promotion — preflight, the ordered
outbox, and receipt application — before a Backend in the same build may act
on it. The first creation slice must add its duplicate guard at the same time
as the non-idempotent call: there can be no shipped interval in which an
ambiguous creation is automatically replayed.

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
is load-bearing rather than tidy: a Repository Store retains one Backend kind
across Backend Items and non-terminal Promotions. `tk remote set` checks that
cohort under its write transaction, and Backend read and Promotion commit
transactions recheck the configured Remote after their Backend/preflight work.
Clearing and restoring the same kind remains valid because repository
resolution belongs to the Adapter.

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

### Promotion uses a directional creation seam

Ordinary Mutations resolve to typed `BackendEdit` variants that contain only
the identities and payload required by that edit. Promotion resolves to a
typed `BackendCreate::Ticket` or `BackendCreate::Epic`; an independent
Mutation-Type/Item-Class mismatch is unrepresentable after resolution.
Identity resolution happens immediately before each delivery, so a Promotion
receipt remains visible to later Mutations in the same run.

Creation returns one of three value outcomes: `Created(identity)`, certified
no-effect `Rejected(failure)`, or `Indeterminate(failure)`. It has no generic
process-error arm because observed `gh issue create` behavior showed that a
completed nonzero invocation may still create the issue.

The engine durably marks a Promotion `applying` before the creation call.
`Created` applies identity, marks the Mutation applied, clears failure, and
advances the cursor in one transaction. `Rejected` moves it to `failed`.
`Indeterminate` leaves it `applying` with diagnostic evidence. An applying row
is not automatically replayed and is a global barrier for Pull, Apply, Adopt,
Promotion commit, and Remote clear. Local Repository Store edits remain
available. Reconcile and explicit-risk retry are separate recovery work.

Every remote-changing workflow holds the repository-scoped
`<git-common-dir>/tk/remote.lock` file lock across its Backend and Store
effects. Promotion acquires it before preflight and passes the same owning
guard into its nested sync. The stable file is opened in place and never
deleted or replaced; dropping the guard, including process termination,
releases ownership. This serialization prevents concurrent processes from
both passing an `applying` check, while the durable state covers crashes after
Backend invocation. Sync Skip also holds the guard while changing Mutation Log
ordering, then opens the Adapter only after that curation commits.

## Considered Options

**Let the GitHub Adapter accept Promotion and reject it at Apply.** Rejected.
The rejection stops the apply loop, the Mutation cannot be skipped, and the
Remote cannot be cleared while it is pending — so a single `tk promote` would
leave a permanently stopped sync with no in-product exit.

**Leave `applying` for a later recovery slice.** Rejected after creation
evidence was gathered. Introducing the creation call before its duplicate
guard would make the first build capable of silently replaying an ambiguous
creation. Migration 008 therefore rebuilds the Mutation table at the earliest
point the non-idempotent seam exists.

**Keep one generic Apply seam.** Rejected. A permissive projection admits
Promotion as an edit, missing counterpart identities, and invalid
Mutation-Type/Item-Class combinations. Directional enums make those states
unrepresentable at the Adapter boundary and preserve creation certainty as a
distinct contract.

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
- A receipt that cannot be persisted — for example, a Display ID collision
  against an existing `item_ids` row — rolls back with the Mutation still
  `applying`. Automatic sync cannot create a second backend object.
