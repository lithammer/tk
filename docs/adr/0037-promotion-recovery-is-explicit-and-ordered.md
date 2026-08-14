# Promotion recovery is explicit and ordered

A Promotion creation is non-idempotent. When tk cannot observe its outcome,
the Mutation remains `applying`: automatic sync cannot know whether retrying
would create a duplicate Backend object. ADR-0036 established that durable
barrier and deferred its operator recovery workflow.

## Decision

`tk promote reconcile <id> <backend-key>` inspects a candidate Backend object
through a narrow Adapter read. The Adapter returns canonical identity, title,
and body only; reconciliation does not classify Ticket Kind or lifecycle
state. Exact title and body equality with the retained Promotion snapshot is
the default proof that the candidate is the created object.

A candidate identity that another Item already holds is refused outright, with
or without `--force`: attaching it would break Backend Item and Display ID
uniqueness, and the object is more likely one Adopt already imported than the
one this Promotion created.

A mismatch writes nothing unless `--force` is supplied. Forced reconciliation
attaches the confirmed identity and appends current local title and body as a
normal update Mutation in the same Promotion Operation. Receipt application,
the forced update, the `applied` transition, and Sync Cursor advancement commit
atomically. Backend lifecycle remains authoritative after attachment, so the
nested sync may immediately import a closed Backend object as `done`.

`tk promote retry <id>` is the explicit-risk alternative. It moves an
`applying` Promotion back to `pending`, then delegates to the normal ordered
sync engine. A pending Promotion is an idempotent input to the command. A
`failed` Promotion is refused: ordinary sync already retries it, so accepting it
here would ask the operator to accept a duplicate-creation risk that does not
apply. Retry never records an identity or advances the cursor by itself.

Recovery preserves global Mutation Sequence order. Reconcile or retry may act
only on the earliest `pending`, `failed`, or `applying` Mutation; terminal rows
before it do not block recovery. Before nested sync, the command captures every
nonterminal Promotion in queue order and afterwards reports every Display ID
replacement that landed, including Promotions outside the requested Item's
operation.

Both recovery commands hold the repository-scoped Remote workflow lock across
Backend access, Store changes, and nested sync. Reconciliation updates the
existing Promotion Operation; it never substitutes a new operation identity.

This decision amends ADR-0036's original global barrier: an `applying`
Mutation still prevents Backend progress, Adopt, and Remote clear, but a later
Promotion Operation may commit durable intent behind it. Global ordering keeps
that later intent from applying until the earlier nonterminal Mutation is
resolved.

## Considered Options

**Retry an `applying` Promotion automatically.** Rejected. The Backend object
may already exist, so replay could duplicate externally visible work.

**Accept any operator-supplied Backend key without inspection.** Rejected. A
mistyped key would permanently bind the Local Item to an unrelated object.

**Import Backend status during inspection.** Rejected. Inspection proves
identity and content only. Normal Backend Pull remains the single owner of
backend lifecycle refresh after the identity is attached.

**Recover only the requested Promotion and report only its Display ID.**
Rejected. Nested sync drains the global Mutation Log and may land other queued
Promotions; hiding those replacements would make successful effects difficult
to discover.

## Consequences

- An indeterminate creation always requires a human choice between confirmed
  reconciliation and explicit-risk retry.
- `--force` is not a relaxed comparison alone; it durably converges the
  Backend object to current local content.
- Recovery cannot jump past unrelated nonterminal intent.
- The Sync Cursor remains monotonic even when reconciliation resolves a
  Mutation behind already-applied terminal history.
