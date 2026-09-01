# Backend Pull shields content fields

A Backend Pull snapshot can pair an authoritative Lifecycle or Ticket Kind with
a stale title and body. An unresolved local content Mutation must keep title
and body visible without hiding those Backend values. The Backend Adapter
returns the snapshot; the Repository Store decides which fields to merge.

## Decision

Before merging refreshes, the Repository Store decodes every `pending` or
`failed` Mutation Type. `UpdateTicket` or `UpdateEpic` shields title and body
for its exact `(item_id, item_class)`. Other Mutation Types leave title and body
Backend-authoritative and cannot remove a shield added by another row. Every
Mutation Type is classified explicitly, including Promotion. In a valid Store,
unresolved Promotions target Local Items without Backend keys, while an
`applying` Mutation remains the global Pull barrier.

Lifecycle is always imported. A `done` Lifecycle clears Work State to `idle`,
and an `open` Lifecycle preserves Work State. For Tickets, a present Ticket
Kind replaces the stored Kind and `None` preserves it. Epics remain `NULL`
regardless of refresh data. The merge keeps `updated_at` at the later of the
stored value and refresh time, even when title and body are preserved.

ADR-0034's working set remains open-only. This decision does not change the
`done` lock or remote reopen handling.

## Why now

ADR-0010 rejected field-level merge because no concrete case required it in
v1. The concrete case is a Backend close while a local title/body Mutation
remains unresolved. Hiding that close leaves the local Item open after the
Backend has ended the work. Shielding only content preserves the local edit
while Lifecycle and Ticket Kind converge.

## Considered Options

Whole-row shielding was the original ADR-0010 rule. It is too coarse once a
Backend close must land independently of title/body intent. Always overwriting
content exposes stale Backend values before Apply confirms local intent.

Per-field timestamps were rejected because local and Backend clocks are not a
reliable ordering source. A durable three-way conflict state was rejected for
v1 because the Repository Store has no conflict-resolution workflow or
Backend version contract. The fixed rule needs no new Store state and keeps the
existing all-or-nothing merge transaction.

## Consequences

If Backend close lands while content intent is unresolved, and that Mutation
later fails and is explicitly skipped, the done Item leaves Pull and its local
content does not self-heal. tk-126 and gh-53 own broader done-Item
reconciliation.
