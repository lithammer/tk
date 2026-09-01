# A Repository Store index needs a proven consumer

`items_next_idx` has served no query since migration 005. Nothing reaches it:
`explain query plan` over every SQL literal in `crates/tk/src` that SQLite can
plan finds no query that uses the index.

Migration 001 created it on `(priority, created_seq)` to serve `tk next`.
Migration 005 added a `selection_state = 'accepted'` conjunct to its partial
`where` clause that `tk next`'s seed does not carry, so SQLite cannot prove the
seed's rows all live in the index. Migrations 006 and 011 carried the widened
clause forward through two more rebuilds.

## Decision

Migration 012 drops `items_next_idx`. It is a plain `drop index` with foreign
keys enforced: ADR-0028's foreign-keys-off rebuild governs CHECK changes, not
indexes.

A Repository Store index earns its place by naming a query whose
`explain query plan` reaches it. The evidence is the plan itself, not a
predicate that looks like the index's `where` clause — those two drifted apart
here without anything noticing. An index no plan reaches is dropped rather than
carried through the next rebuild.

ADR-0028's recreate list loses `items_next_idx` and gains the reason it is
absent. Every rebuild must still recreate the other four `items` objects and
the `items_no_escape_from_done` trigger.

## Why now

No change to how `tk next` orders can revive the index. Selection sorts by
`eff.ep asc, ann.priority asc, ann.created_seq asc`, where `eff.ep` is
Effective Priority computed per candidate, so no index on `items` can serve
that sort. The index's key is the ordering ADR-0015 replaced: the option
ADR-0015 rejected sorts by `priority asc, created_seq asc`, which is this
index's key exactly. Three migrations carried its `where` clause forward after
its key had no consumer left.

## Considered Options

**Revive the filter.** Adding `and selection_state = 'accepted'` to the
`reachable` seed in `store/repository/next.rs` makes the plan reach the index,
and it changes no output. `reachable`, `eff`, and the contributor sub-SELECT
are all keyed by `start_id`; the outer query already requires an `accepted`
candidate, so a trimmed seed's rows only ever fed an Effective Priority that
was discarded; every accepted candidate still seeds itself, so the inner
`join eff` loses no row; and `prop_edge` is untouched, so Effective Priority
still propagates through `triage` and `parked` intermediates. CONTEXT.md's rule
that non-accepted Tickets are excluded as candidates is what puts the trimmed
seeds out of reach, and it makes the seed set exactly the candidate set.

Rejected because it saves one scan of `items` while binding the seed's
predicate to stay a subset of a partial index's `where` clause from then on.
That is the coupling migration 005 already broke once, in the direction nothing
observes. It would re-arm at the next migration to touch the index, and it runs
backwards too: relaxing the outer `accepted` filter later would quietly narrow
selection through the seed instead. One saved scan of a few hundred rows does
not earn a rule spanning a SQL string and a schema file with nothing to enforce
it.

**Leave it dead.** Rejected. It keeps write-time maintenance nothing reads,
and — the cost that matters — leaves ADR-0028's recreate list telling every
future rebuild to reproduce the index, which is how it survived 005, 006, and
011.

**Restructure `tk next` so the key becomes usable.** Rejected: it means giving
up Effective-Priority-led ordering, which ADR-0015 ratified against exactly the
alternative this index's key encodes.

## Consequences

`tk next`'s query plan does not change. The seed still scans `items`, as do
`prop_edge`'s Epic-membership arm and `eff` — three scans either way, which is
why no measurement argued for reviving the index.

`store/repository/next.rs` keeps a seed that is a superset of the candidate
set. That is deliberate, and recorded here rather than in a comment beside the
SQL: narrowing it changes no output and touches selection SQL for nothing
anyone can observe.

`tk list --ready` is the one query shape that would use the whole index, filter
and sort together. Its four conjuncts sit inside a `case ?1 when 'ready' then …`
expression the planner cannot read. Inline them into a `where` clause and that
query reaches an index shaped like this one with no temporary B-tree for the
ordering. Re-adding it then is an additive `create index`, argued from a plan
rather than a resemblance.

gh-59 asks for access paths on `mutations`. It inherits the standard: name the
query and the plan that reaches the new index.
