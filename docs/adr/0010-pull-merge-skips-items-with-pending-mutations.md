# Backend Pull merge skips rows with pending or failed Mutations

> **Amended by ADR-0044.** When a Pull snapshot matches an existing Item, an
> unresolved `pending` or `failed` `UpdateTicket` or `UpdateEpic` Mutation for
> its exact `(item_id, item_class)` preserves only `title` and `body`. Pull still
> imports Lifecycle and a present Ticket Kind; `None` preserves the stored Kind.
> A `done` Lifecycle clears Work State to `idle`, and a refresh keeps
> `updated_at` at the later of the stored value and refresh time. All other
> Mutation Types admit Backend title/body, including Promotion variants. An
> `applying` Mutation remains the global Pull barrier.

When a Pull snapshot matches an existing local row, the sync engine originally
skipped the entire row's overwrite if any `pending` or `failed` Mutation
targeted it; otherwise it overwrote `title`, `body`, `status`, and
`updated_at` in one transaction. The historical whole-row rule is replaced by
ADR-0044's field authority policy.

## Considered Options

Field-level merge (per-MutationType rules for which snapshot fields to
keep) was rejected: the rule set grows with every new MutationType and
no concrete case required it in v1. ADR-0044 supplies one: a Backend close
must land while local content intent remains unresolved. Always overwrite was
rejected because it flips local titles to stale backend values before Apply
flips them back, producing visible flicker. Per-field timestamps was rejected
because clock skew between local and backend makes the comparison unreliable.

## Consequences

Absence from a snapshot is treated as no-op, not a delete signal: v1
cannot distinguish "backend deleted" from "Pull was filtered" or "auth
hid it," so stranded local Backend items are safer than data loss on
a misread Pull.
