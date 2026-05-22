# tk next orders by Effective Priority within Workspace Scope

`tk next` ranks ready **Tickets** by **Effective Priority** (lowest
first), then own **Priority**, then `created_seq`, where **Effective
Priority** is the minimum of a **Ticket**'s own **Priority** and the
**Effective Priorities** of every item it transitively blocks through
`blocked_by` **Dependencies** within the active **Workspace Scope**.
**Epics** in the chain contribute the minimum **Effective Priority**
over their open child **Tickets**. **Effective Priority** never
appears as a stored field or on `tk show` / `tk list` output. When
the selected **Ticket**'s **Effective Priority** is lower than its
own **Priority**, `tk next` may render a stderr rationale line so a
non-obvious pick is explainable without a follow-up `tk show`. See
the `Effective Priority` entry in [CONTEXT.md](../../CONTEXT.md) for
the definition and propagation rules.

## Considered Options

**Order by own Priority only.** The pre-existing SQL sorted by
`priority asc, created_seq asc`. Rejected because in a backlog with
many P2/P3 chores blocking a P1, `tk next` never surfaces the chore,
so the P1 remains perpetually blocked while the agent picks
unrelated P2 work.

**Tiebreaker-only propagation.** Keep own **Priority** as the
primary sort, only consult **Effective Priority** when two candidates
have the same own **Priority**. Rejected because a P3 **Blocking
Item** still loses to a P2 unrelated chore, so the "chip away toward
a blocked higher-**Priority** **Ticket**" case remains unsolved.

**Scope-permeable propagation.** Walk the **Dependency** chain
across the **Workspace Scope** boundary so an in-scope **Ticket**'s
**Effective Priority** reflects out-of-scope **Blocked Items** it
gates. Rejected because a **Workspace Scope** is paired with a
**Ticket Branch**; ordering scoped work by out-of-scope signals
prioritizes in-scope **Tickets** for reasons that have nothing to do
with the active feature branch.

**Display Effective Priority on every read view.** Rejected because
**Effective Priority** is a derived selection signal, not a stored
property. Surfacing it next to **Priority** on `tk show` / `tk list`
doubles the priority field while adding no information in the common
case (**Effective Priority** equals own **Priority**). The stderr
rationale on `tk next` covers the only spot a user actually needs
the explanation.

**Rationale on stdout.** Rejected because `id="$(tk next)"` is a
supported usage; a second stdout line would break it. Stderr
preserves the contract while still giving humans and interactive
agents the reason.

## Consequences

- The selection SQL in `src/store/repository.zig` (`next_ready_ticket_sql`)
  is replaced by a recursive CTE that computes **Effective Priority**
  for each candidate using the existing `dependencies` and `items`
  tables; no schema additions are needed.
- `tk list --ready` ordering is untouched (it already sorts by
  `created_seq`, not **Priority**). The two views serve different
  jobs and need not agree on ordering.
- Rationale rendering is optional. If walking back to the
  contributing **Blocked Item** turns out to be awkward in SQL (for
  example when the contribution comes through several **Epic** hops),
  the rationale may be omitted; CONTEXT.md says "may render," not
  "must."
- When multiple reachable **Tickets** share the candidate's
  **Effective Priority**, the rationale names the one with the lowest
  `created_seq`. This is an implementation tie break, not a guarantee
  — callers must not depend on which contributor is named when the
  store contains ties.
- **Dependency** edges and **Epic**-membership edges are each
  acyclic, but their union is intentionally not: a **Ticket** may
  carry a **Dependency** that blocks its containing **Epic**, which
  closes a `dep → membership` round trip through every sibling that
  also blocks that **Epic**. The recursive CTE carries a path string
  per row and refuses to extend an edge whose destination is already
  in the path, so the walk stays bounded by the number of distinct
  reachable nodes. A depth cap remains as defense in depth against a
  malformed schema.
