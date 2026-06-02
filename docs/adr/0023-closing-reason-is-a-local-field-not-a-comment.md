# Closing Reason is a local-only field, not a Comment

`tk done -m "<reason>"` (tk-61) records an optional explanation when a
Ticket or Epic is marked `done`. We model this **Closing Reason** as a
**Local Field** — a nullable `closing_reason` column on `items`, stored
current state in the Repository Store like `body` and `priority` — and
explicitly **not** as a Comment. It is captured once, on the actual
transition to `done`, and rendered by `tk show`; it never reaches a
Backend in v1.

A closing reason reads like a comment, and v1 defers Comments
(CONTEXT.md; the `mutations` Mutation Type list; ADR-0021). Treating it as
a Local Field dissolves that conflict instead of blocking the ticket on
the Comments slice: Priority is the precedent — local-only, unsynced,
shown by `tk show`.

## Considered Options

- **Model it as a Comment now.** Rejected: Comments are deferred from v1,
  and a comment stream, its schema, and backend round-trip are exactly the
  work tk-62 (Support Ticket Comments) owns.
- **Append the reason to `body`.** Rejected: muddies the body contract and
  cannot render as its own `tk show` section.
- **Carry the reason on the `set_item_status` Mutation to the Backend.**
  Rejected for v1. `gh issue close --comment` and a Jira resolution
  comment exist, so this is a **scope choice, not a capability gap**:
  pushing a closing comment without reading it back on Pull is the same
  half-sync asymmetry ADR-0021 declined for relationships, and the
  round-trip is the deferred comment-sync problem. Reflecting a Closing
  Reason on a Backend is deferred indefinitely to tk-62 / tk-109.

## Consequences

- New nullable `items.closing_reason` column with a CHECK that it is
  non-null only when `status = 'done'`, plus a non-empty CHECK
  (`closing_reason is null or length(closing_reason) > 0`), mirroring the
  existing `external_blockers.reason` and `title` guards. The
  done-is-terminal rule (ADR-0006) means at most one closing reason per
  item.
- Closing Reason is **set-once at the transition**. The existing
  already-at-target no-op short-circuit in `set_item_status` stands: re-
  running `tk done -m` on an already-done item does not amend the reason
  and reports a soft error (`tk done: '<id>' is already done; closing
  reason not changed`, exit 1). A later `tk update --closing-reason` could
  add amendment if a need appears.
- `-m` is inline-only (no `-F -` stdin path); an empty or whitespace-only
  value is rejected before the write.
- No change to the Mutation Log, the V1 Mutation Type list, the
  `set_item_status` payload, or the GitHub Backend Adapter. tk-109 (under
  the tk-62 Comments epic) revisits whether the Closing Reason should
  later become a Comment that syncs to Backends.
