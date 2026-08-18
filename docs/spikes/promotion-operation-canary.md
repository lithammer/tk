# Spike: Promotion Operation canary (tk-142)

Field verification of tk's own Promotion Operation behaviour against a real
GitHub Backend: `--children` ordering, receipt wiring, Epic membership, and
Promotion Cancellation's outcome certification. Companion to
[gh-cli-issue-behavior.md](./gh-cli-issue-behavior.md), which records what `gh`
itself does; this document records what tk does with it.

- **Date:** 2026-08-18
- **Binary under test:** built from `0697d91d` (clean tree), release profile,
  toolchain 1.96.0, copied aside so a later rebuild could not change what
  produced the evidence.
- **gh version:** 2.97.0 (2026-07-31)
- **Sandbox:** private repo `lithammer/tk-gh-playground`, issues #9–#22, all
  titled `[tk-142-canary-2026-08-18-f4849fdd]` and closed afterwards. Six
  throwaway clones — one for raw `gh` probes and five each carrying their own
  `tk init` store — separate stores because
  an `applying` Mutation blocks Backend Pull, Mutation Apply, Adopt, and Remote
  clear, so one wedged scenario would have blocked the rest.

## What the run confirms

Every behaviour tk contracts for was exercised against the real Backend and
matched.

**Promotions are ordered ahead of the Mutations that resolve their identities.**
A `tk promote <epic> --children` over an Epic with two Tickets and a Dependency
queued, in Mutation Sequence order: `promote_epic`, `promote_ticket`,
`promote_ticket`, `add_ticket_to_epic`, `add_ticket_to_epic`, `add_dependency`.
The relationship payloads carry *internal* item ids (`epic_id`, `blocking_id`),
not Backend keys, so the Backend address really is resolved from the receipt at
Apply time rather than fixed when the plan is committed (ADR-0036).

**Epic membership and Dependencies land on GitHub.** The Epic held both children
as sub-issues, both children read back `parent`, and the Blocked Ticket read back
`blockedBy` naming the Blocking Ticket.

**Membership applies for an Adopted Ticket too.** An issue created outside any
Promotion Operation was adopted, given the promoted Epic as parent, and joined it
on GitHub.

**`tk update <id> --no-parent` clears the parent upstream.** It emits
`remove_ticket_from_epic`, which the Adapter applies as `--remove-parent`
(ADR-0021), and the Epic dropped the child.

**A certified auth rejection lands `failed` and cancels.** Promoting under an
invalid `GH_TOKEN` left Mutation 1 `failed` with `Class: auth` and the verbatim
401 detail; `tk promote cancel` withdrew the operation; `tk remote clear` then
succeeded. GitHub's issue numbering did not advance, so the certification of
no-effect was correct against the real Backend.

**Both diagnostics tk-141 fixed now point at cancellation.** With a `failed`
Promotion in the log, `tk remote clear` and `tk sync --skip` each recommend
`tk promote cancel`, and cancellation accepts the row. The three-way circular
guidance tk-141 was opened against is gone for the `failed` case — but see
[The `applying` dead end](#the-applying-dead-end) for the state it is not gone
for.

**Cancellation opens no Adapter.** The all-pending withdrawal was run with `gh`
absent from `PATH` entirely and succeeded, which exercises ADR-0038's claim that
the exit of last resort works with a broken Remote.

**A cancelled Mutation keeps its Mutation Failure record.** After cancellation
the withdrawn `failed` Promotion still renders its 401 detail. `failure_json`
presence therefore cannot distinguish a Skipped Mutation from a Cancelled one,
which is the reason ADR-0038 gives for making `cancelled` a distinct state.

**Cancellation refuses a Dependency it would half-represent, naming the remedy.**
In an operation whose Epic Promotion applied while a child's was rejected, the
rejected child was the Blocking Item of a Dependency whose Blocked Item had
landed. Cancellation refused:

```
tk promote: cannot cancel the Promotion Operation for gh-21:
  gh-22 is backend-backed and would be left waiting on s5-3, which the withdrawal returns to local. Run 'tk unblock gh-22 s5-3' to drop the Dependency, then cancel again.
```

It names the offending edge and the exact `tk unblock` invocation. This is
ADR-0035's Dependency classification acting as cancellation's third caller,
judged against the Backend Binding the withdrawal would produce.

**Cancellation reports rather than compensates, and enumerates rather than
counts.** After `tk unblock`, cancelling the same operation printed:

```
Cancelled Promotion: Ticket s5-3
Already created upstream, left in place: Epic gh-21
Already created upstream, left in place: Ticket gh-22
Withdrew add_dependency for gh-22 (Mutation 5)
Withdrew remove_dependency for gh-22 (Mutation 7)
Withdrew 1 further Mutation(s) targeting the cancelled items.
```

The applied Promotions are reported and left alone; the two Mutations whose
target is a real upstream object are enumerated; the one whose target is itself
cancelled is counted. Both issues remained open on GitHub with nothing deleted.

**The withdrawn set is one hop and not over-broad.** `add_ticket_to_epic` for the
*landed* child survived as `pending` — it resolves without any cancelled item's
address — and applied on the next `tk sync`, attaching the child to the Epic.

**Re-promotion after cancellation converges with no residue.** The same Epic and
children re-promoted to fresh issues, and the withdrawn rows stayed `cancelled`
in the Mutation Log. No duplicate Backend objects were created.

## Divergences from the Ticket's premises

The tk-142 Ticket predicted behaviour by reading source. Three of its premises
about the surrounding workflow were wrong, and they change how the procedure
should be read.

**`tk promote` does not queue for a later `tk sync` — it drains inline.**
`commands/promote.rs` commits the plan and then calls `sync::run_sync` in the
same invocation. With a healthy Remote every Mutation is `applied` before the
command returns, so `tk sync log` prints "No Mutations recorded." immediately
after a three-item `--children` Promotion. Two consequences:

- The Ticket's instruction to confirm ordering from `tk sync log` after a
  successful Promotion cannot be followed. The list view excludes `applied`
  rows (tk-143 owns that doc mismatch), and there is no pending window to
  inspect beforehand. Ordering was read from the `tk sync log <sequence>` detail
  view instead, which reaches any state.
- The Ticket's "cancel before running `tk sync`, the path an operator takes when
  they simply change their mind" describes a state that operator cannot occupy.
  Once `tk promote` returns successfully the issues exist; there is nothing to
  withdraw. An all-`pending` Promotion Operation requires the drain to abort
  before its Apply loop.

  This canary reached that state by giving Backend Pull a key it could not fetch:
  with one Adopted item in the store and `gh` absent from `PATH`,
  `adapter.refresh_item` fails and propagates out of `run_sync` ahead of the
  Apply loop, leaving all six Mutations `pending`. A second route, read from the
  code rather than observed here: `run_sync` returns `ApplyingMutation` before
  Pull, so promoting anything while another operation sits `applying` also
  commits a plan whose drain refuses immediately.

  Either way the route is a broken or barred environment, not a change of mind.

**GitHub's issue title limit is not 256 characters in practice.** A
257-character title was accepted and stored intact, as was 1024. A 70000-character
title was rejected. Enforcement sits somewhere above 1024; the exact boundary was
not pinned, and 70000 is what this canary used to provoke a validation failure
reliably.

**A validation rejection does not arrive HTTP-shaped.** See the companion
document; the consequence for tk is that the failure classified as `Unknown`, and
the Ticket's expected `HTTP 422` never appeared.

## The `applying` dead end

The Ticket asked whether the cancellation refusal's guidance is good enough when
an operator believes a Promotion is hopeless. It is not. Every documented exit
was followed to its end against a Promotion whose title GitHub will never accept:

| step | result |
|---|---|
| `tk promote cancel <id>` | refused — indeterminate outcome; names reconcile and retry |
| `tk sync` | refused; names reconcile and retry |
| `tk sync --skip <seq>` | refused; **names `tk promote cancel`**, which just refused |
| `tk remote clear` | refused, **with no next step named at all** |
| `tk promote retry <id>` | fails identically, stays `applying`, recommends retry again |
| `tk promote reconcile <id> 999999` | refused — the honest key, for an object that was never created, does not resolve |
| `tk promote reconcile <id> <unrelated>` | refused — snapshot mismatch; offers `--force` |
| `tk promote reconcile <id> <unrelated> --force` | **succeeds** — binds the Ticket to an issue the Promotion did not create, and queues an `update_ticket` that then fails the same way |
| `tk sync --skip <that update>` | allowed — it is an edit, not a Promotion |
| `tk remote clear` | succeeds |

So the state is escapable, but the only exit that works is forcibly binding the
Ticket to a Backend object the Promotion never created, and no diagnostic says
so. `MarkSkippedError::CannotSkipPromotion` carries only the Mutation Sequence,
with no Mutation state, so its recommendation to cancel is unconditional: for an
`applying` Promotion it sends the operator to a command that refuses. That is
structurally the circular guidance tk-141 was opened to remove, surviving in the
one state tk-141 did not cover.

Per the Ticket, no behaviour was changed here. The guidance question is
tk-147; the classifier anchor is tk-148; the Cancelled Mutation rendering
noted below is tk-149.

## Observations too small for a Ticket

- GitHub's `subIssuesSummary` can read stale immediately after a parent edit: a
  readback reported 2 sub-issues while `subIssues.nodes` in the same response
  held 1. A second query agreed with the nodes. Readback assertions should
  prefer `subIssues.nodes` over the summary counter.
- A multi-line Mutation Failure detail breaks the `tk sync log` list indent — the
  401 frame's second line renders flush-left rather than under the `└─`.

## Fault injection

Four of the five scenarios needed no injection. The fifth — an operation whose
Epic Promotion applies while a child's is certified `Rejected` — is not reachable
*deterministically*, because certification requires either a pre-spawn runner
failure or the 401 frame, and neither is something one `tk sync` run can be
steered into part-way through. Two real routes exist and were rejected for
control rather than fidelity: revoking the token server-side mid-drain, which is
a race against however long the remaining creates take, and interrupting a drain
between Mutations and resuming it under a changed environment.

It was reached with a `gh` shim on `PATH` that passes every invocation through to
the real binary except an `issue create` whose argv carries a marker, for which
it replays the 401 frame captured earlier in the same session from a genuinely
invalid `GH_TOKEN`. The replay is byte-identical by construction because the shim
writes the captured file rather than a re-typed copy of it.

```bash
#!/usr/bin/env bash
CAP='<the captured stderr of a real invalid-GH_TOKEN gh issue create>'
MARK='REJECT-VIA-SHIM'
is_create=0; has_mark=0
[ "$1" = "issue" ] && [ "$2" = "create" ] && is_create=1
for a in "$@"; do case "$a" in *"$MARK"*) has_mark=1;; esac; done
if [ "$is_create" = 1 ] && [ "$has_mark" = 1 ]; then
  cat "$CAP" >&2
  exit 1
fi
exec /usr/bin/gh "$@"
```

Only the one child's rejection is injected. The Epic and the surviving child were
created on GitHub for real, and the whole cancellation path — the Dependency
refusal, the `tk unblock` remedy, the report, the surviving membership Mutation,
and the untouched Backend objects — ran against real Backend state.

That `failed` is reachable in the field essentially only through a bad token or a
missing `gh` is itself the most consequential thing this canary found: `applying`
is where a real Backend refusal lands, and it is the state whose guidance is
weakest.
