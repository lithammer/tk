# Promotion captures representable Dependencies from the resulting graph

Promotion is the boundary where current local state becomes backend intent.
It therefore captures existing Dependencies according to the Origins that the
whole Promotion operation will produce, rather than according to the order in
which individual backend objects happen to be created.

## Decision

Promotion preflights its target and any Promotion Children together. An
existing Dependency becomes backend intent when its Blocked Item and Blocking
Item will both be backed by the configured Backend after the operation.
Dependencies among Promotion Children are included without following
Dependency edges to discover additional items.

Preflight rejects the whole operation before creating backend objects when:

- a backend-backed Blocked Item would still depend on a Local Blocking Item;
- the configured Backend Adapter cannot represent a Dependency that would
  become backend intent; or
- any other Dependency edge makes the resulting graph invalid.

The diagnostic reports every invalid Dependency, identifies both endpoints,
explains the reason, and gives an available remedy. A done Blocking Item
resolves readiness but does not remove its Dependency, so retained resolved
edges are included by Promotion too.

After preflight, one local transaction appends the complete ordered outbox.
Item Promotion Mutations precede Dependency and Epic-membership Mutations
whose payloads refer to stable internal Item IDs. Backend identities are
resolved when each Mutation is applied, after preceding Promotion receipts
have assigned them. This removes the crash window that would exist if
relationship intent were appended only after backend creation.

An item has Pending Promotion from the durable local commit until its
Promotion receipt assigns a backend identity. Backend-applicable changes made
during that interval append ordered Mutations behind Promotion.

## Remote failure and retry

Preflight and the local outbox commit are atomic; backend effects are not.
Accepted Promotion Mutations and their receipts remain applied if a later
creation or relationship Mutation fails. The failure is recorded in the
Mutation Log, later Mutations remain pending, and `tk sync` resumes from that
point after repair. tk does not compensate by deleting backend objects that
were already created.

Pending Promotion cancellation is a broader recovery decision because later
title, body, status, membership, Dependency, and External Blocker Mutations
may all rely on the missing backend identity. Its semantics are deferred from
this decision.

## Considered Options

**Sync only Dependencies added after Promotion.** Rejected because equivalent
Repository Store states would produce different backend graphs depending on
whether the Dependency was added before or after Promotion.

**Automatically promote Local Blocking Items.** Rejected because Promotion is
explicit curation and does not follow Dependencies.

**Keep unrepresentable Dependencies local after Promotion.** Rejected because
the backend-backed item would expose an incomplete blocking relationship.

**Append relationship intent after backend creation.** Rejected because a
process failure could leave created backend objects with no durable record of
the relationships still to apply.

**Delete already-created backend objects after a later failure.** Rejected
because remote creation is not transactional and those objects may already be
visible or have received human activity.
