# Cancellation withdraws a Promotion whose outcome was never observed

ADR-0038 gave a certified Promotion its exit and refused an indeterminate one,
naming Promotion Reconciliation and Promotion Retry instead. The tk-142 canary
(docs/spikes/promotion-operation-canary.md) followed every documented exit to
its end against a Promotion whose title GitHub will never accept, and none of
them resolves it. Promotion Retry replays the frozen payload and fails
identically. Promotion Reconciliation has no candidate key for an object
that was never created. Forced reconciliation against an unrelated issue is the
only thing that works, and it permanently binds the Ticket to a foreign Backend
object and queues an overwrite of that object's title and body. Nothing says
so.

The same canary found that `failed` is reachable in the field essentially only
through a bad token or a missing `gh`. A real Backend refusal lands `applying`,
so the state ADR-0038 left without an exit is the state operators actually
reach.

## Decision

`tk promote cancel <id>` accepts an `applying` Promotion. There is no flag and
no operator attestation: cancellation reaches no Backend, so it cannot learn
what the creation did, and the withdrawal records that nothing was learned
rather than asserting anything about it.

### The refusal contradicted a half-state ADR-0038 already accepted

ADR-0038 refused an `applying` Promotion because "a partial withdrawal would
leave an operation whose Epic may exist upstream and whose children are gone:
exactly the half-state operation scope exists to prevent". Two paragraphs
earlier it accepts that shape: an already `applied` Promotion "does not block
cancellation of the operation's remaining Promotions; it is reported, since tk
never compensates by deleting Backend objects". The canary printed it —
`Already created upstream, left in place: Epic gh-21` beside a withdrawn child.

An `applying` Promotion is the same shape with less certainty. The only
difference is whether the report can say the object does exist or may exist,
and ADR-0038 had already decided that certainty is not what gates a withdrawal.

### Three exits, one per belief the operator can hold

Recovery is complete once each belief about an unobserved creation has a
command: it was created, so bind it (Promotion Reconciliation); it was not, and
the Promotion is still wanted, so create it again and accept the duplicate risk
(Promotion Retry); the Promotion is no longer wanted, so withdraw it (Promotion
Cancellation). Exactly one was missing.

### An unobserved withdrawal is a distinct terminal state

`cancelled` means withdrawn intent that created nothing — that is what ADR-0038
decided and what CONTEXT.md defines. Writing it on a row that may have created
a Backend object would make Promotion Cancellation's own definition untrue,
which is verbatim the argument ADR-0038 used against recording withdrawn intent
as `skipped`. `abandoned` keeps both meanings intact:

- Cancelled from `pending` or `failed`: tk either never called the Backend or
  observed its refusal. Nothing exists upstream.
- Abandoned from `applying`: tk called the Backend and recorded no identity
  for what came back. Something may exist upstream, and tk will never look
  again.

The state is defined by that missing identity rather than by an Indeterminate
outcome, because one other route reaches `applying` without one: a creation
whose receipt arrived but whose identity could not be committed to the
Repository Store. That row is Created, not Indeterminate, and cancelling it
abandons an object tk saw but cannot address. Its own diagnostic prints the
Backend key and names reconcile alone — offering a withdrawal there would send
the operator to discard an object they can name.

`applying → abandoned` is the only edge into the state, so only a Promotion can
reach it — the `mutations.state` CHECK carries that restriction the way it
already restricts `skipped` to non-Promotions, through an ADR-0028
foreign-keys-off table rebuild. The edge preserves `failure_json`, because the
indeterminate creation's diagnostic evidence is the reason the withdrawal
happened.

Every nonterminal query names `pending`, `failed`, and `applying` positively,
so `abandoned` is terminal without further rule: the Item's Backend Binding
returns to Local, the Promotion Operation counts as resolved, no Mutation is
orphaned by clearing the Remote, and the `applying` barrier lifts.

`abandoned` joins the Sync Log's default filter for `cancelled`'s reason — a
withdrawal must be visible without a flag — and also gets `tk sync log
--abandoned`. `applying` has no such filter because at most one `applying` row
can exist; `abandoned` rows accumulate, and they are the only rows in the log
that mean tk may have left something behind upstream.

### The report names what was never learned, and cannot name the object

Cancellation's report gains a line for the abandoned Promotion saying its
creation outcome was never observed, so any object it created is now untracked.
It cannot hand the operator `tk adopt <backend-key>`: no key was ever observed,
which is the whole reason the Promotion was indeterminate. Recovery means the
operator finding the object themselves and adopting or closing it. That is a
real limit of this exit, not a rendering gap.

### Re-promotion warns while an abandonment is unresolved

Promoting an Item that has an abandoned Promotion is where the
duplicate-creation risk is actually incurred — the risk Promotion Retry refuses
to take silently, reached by a longer road. `tk promote` warns and does not
refuse: refusing would restore a dead end one decision after removing one.

The scope is the latest *abandonment*, not the latest Promotion. Only a
Promotion the Backend accepted resolves the risk, and such an Item is never
planned for Promotion again, so it never reaches the question. Every other
later outcome created nothing, so none may mask a live risk — scoping to the
latest Promotion would let a cancelled one do exactly that. In practice the
warning therefore fires once, on the promotion that follows the withdrawal,
and keeps firing only while that promotion keeps failing.

## Considered Options

**Name forced reconciliation as the deliberate last resort.** Rejected. Compare
what a wrong choice costs. A wrong forced binding is permanent and outward: tk
edits start landing on a foreign Backend object, and ADR-0037 already treats a
duplicate binding as the one thing `--force` may not override. A withdrawal
that turns out to have abandoned a real object is inward and recoverable — an
orphan the operator can adopt or close. In-product guidance that routes
operators into the more damaging failure is worse than the silence it replaces.

**Broaden certification so a GraphQL rejection certifies Rejected.** Rejected.
`GraphQL: <message> (<path>)` is go-gh's rendering of any
HTTP-200-with-`errors` response, and validation errors, which commit nothing,
arrive in the same shape as execution errors, which may. gh's source settles
the rendering; it cannot settle the server-side claim that errors at path
`createIssue` mean no issue was committed, and GitHub's server is not a primary
source anyone can read. Anchoring on message text is what tk-148 argues against
for the same class of reason. tk-148's question — whether reading a primary
source meets ADR-0016's bar for a classifier anchor — stays open rather than
being answered here as a side effect.

**Require a flag or an operator attestation, such as `tk promote cancel <id>
--not-created <sequence>`.** Rejected. Promotion Retry already takes an
outward-facing duplicate-creation risk with no flag, and ADR-0038 accepts a
*certain* orphan with only a report. Gating the uncertain case more tightly
than the certain one inverts the house position that the report is the dry run.
A flag would also force all five indeterminate diagnostics to branch on
Mutation state and describe a two-argument invocation, where removing the
refusal makes `MarkSkippedError::CannotSkipPromotion`'s existing guidance true
as written.

**Record the withdrawal as `cancelled` and distinguish it by a marker column on
the row.** Rejected. It is cheaper — a nullable column and one CHECK clause
rather than a table rebuild and a seventh `MutationState` — and it keeps one
invocation's rows in one state. But it widens what `cancelled` means, so every
reader who learned "cancelled means nothing exists upstream" becomes wrong some
of the time, and a marker is not something `tk sync log` can list by.

**Re-snapshot title and body on Promotion Retry.** Deferred to its own
Ticket; resolved as no (tk-152): retry stays a pure state transition
(ADR-0037). It would give a fixable rejection a forward exit and would
resolve the asymmetry that forced reconciliation re-reads current local
content while retry does not. With this decision in place the forward path
already exists — withdraw, fix, promote again, which re-snapshots by
ADR-0038's rule — so retry re-snapshotting would only preserve the original
Promotion Operation identity, at the cost of softening ADR-0036's frozen-plan
model.

## Consequences

- Every Promotion state now has an exit that resolves it without a Backend call
  and without binding an Item to an object its Promotion did not create.
- `MarkSkippedError::CannotSkipPromotion`'s unconditional recommendation to
  cancel becomes true for every Mutation state, so the circular guidance tk-141
  removed for `failed` is gone for `applying` without teaching that diagnostic
  about Mutation state.
- `ClearRemoteError::ApplyingMutation` and Adopt's indeterminate refusal can
  name a next step, which neither did.
- This is the first place tk knowingly loses track of a Backend object that may
  exist. An `applied` Promotion left in place during cancellation is reported
  by Display ID; an abandoned one cannot be, because no identity was ever
  observed.
- The `mutations.state` CHECK gains `abandoned`, restricted to Promotion
  Mutation Types, through an ADR-0028 foreign-keys-off table rebuild.
- `MutationState` gains a seventh variant, so the exhaustive transition table
  forces a decision about every edge into and out of it.
- `prime.md` keeps cancellation among the human curation commands for the
  existing reason, and states that the withdrawal may abandon an object tk
  never observed — which is a stronger reason for an agent to surface it rather
  than run it, not a weaker one.
