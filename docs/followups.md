# Implementation follow-ups

Holding area for review-time follow-ups that should not block the current
slice. Each section is written in `tk add -F -` format: the first paragraph is
the ticket title, subsequent paragraphs are the body. Once Ticket can dogfood
its own Repository Store comfortably, move these entries into `tk add` and
delete this file.

Inline `// TODO(followups):` comments at the relevant call sites point
back to the section title here.

---

## Surface a stderr warning when tk init can't tighten store permissions

`setDirMode0700` swallows every chmod failure with `catch {}`. The
intent is "if the directory already exists with broader permissions,
leave it as-is", but the current implementation also masks failures
where we did just create the directory and did expect to tighten it. A
silently-0755 store is a confidentiality regression nobody will notice.

Distinguish the "already exists, leave it" case from the "chmod call
failed" case, or at least write a single-line stderr warning
(`tk init: warning: could not tighten permissions on <path>: <error>`)
so the symptom is observable.

Touches `src/commands/init.zig`.

---

## Prefix tk init clap diagnostics with the command name

A clap parse failure in `tk init` writes the diagnostic via
`diag.report(deps.stderr, err)` with no `tk init:` prefix. Every other
error path in the file starts with `tk init:`. The same drop-the-prefix
pattern lives at the top level in `cli.zig`. For an agent grepping
diagnostics, the missing prefix makes attribution to the right command
harder.

Wrap the diagnostic with a prefix and a "run 'tk init --help' for
usage" hint. Apply symmetrically to the top-level handler in `cli.zig`
so every parse failure lands the same shape.

Touches `src/cli.zig`, `src/commands/init.zig`.

---

## Centralize test-side Deps construction

`Deps` has three independent construction sites: `main.zig` (real
wiring), `Harness.deps` in `src/testing/test_cli.zig` (fake runner,
fake clock, allocating writers), and `executeScript` in
`src/testing/script.zig` (real runner, fake clock at zero, allocating
writers). They will drift as `Deps` grows by 1-2 fields per slice
(worktree service, sync engine, remote registry, tty/color flags are
all queued). The two test sites also disagree on what "a test Deps"
means.

Centralize. One option: a `cli.Deps.testing(args)` builder
parametrised on which dependencies are fake versus real. Another: have
`script.zig` go through `Harness` instead of building Deps from
scratch. Goal is one place to update when `Deps` grows.

Touches `src/cli.zig`, `src/testing/test_cli.zig`, `src/testing/script.zig`.

---

## Add color policy after list output stabilizes

`tk list` is deliberately starting with plain ASCII output so the List
Tree shape, item markers, and filtering semantics can settle before
styled rendering enters the command path.

Add `--color=auto|always|never`, fakeable `tty_stdout` plumbing on
`Deps`, and `NO_COLOR` / `CLICOLOR_FORCE` handling in a later
output-rendering slice. Apply the policy consistently to every command
that emits styled output instead of making `tk list` a one-off.

Touches future output rendering code, `src/cli.zig`, command modules
that opt into styled output, and scenario snapshot handling.

---

## Revisit deferred work as a first-class concept

The Beads-style list output includes a `deferred` state, and that concept
looks useful for work that should remain visible but should not be
selected by `tk next`.

Do not include it in the `tk list` slice. V1 **Item Status** remains
`open`, `active`, or `done`, and `blocked` remains distinct from Item
Status. Revisit later whether `deferred` should become an Item Status, a
Local Field, or a separate scheduling/visibility concept.

Touches `CONTEXT.md`, `docs/cli.md`, `docs/implementation.md`,
Repository Store schema, `tk next`, and list rendering.

---

## Decide the script runner's fakeability stance

`src/testing/script.zig` mixes a real subprocess runner with a fake
clock. Today no scenario exercises a subprocess, so the inconsistency
is theoretical, but the moment a `tk init` (or any other subprocess-
invoking) scenario lands, scenarios will fork real `git` and become
slow / flaky / dependent on the host environment.

Decide before the first such scenario lands. Either go fake-everything
(give the scenario runner a `FakeRunner` and let scenarios script
subprocess responses through a new fixture section), or
real-everything (drop the fake clock; let scenarios cover the real-
subprocess integration path while `smoke.zig` handles the linked-
binary embed/argv check). The current middle ground is the worst of
both.

Touches `src/testing/script.zig`.

---

## Pick one Allocator alias style across the codebase

`src/proc/runner.zig` and `src/proc/fake.zig` add
`const Allocator = std.mem.Allocator;` and use the short `Allocator`
in signatures. `src/store/migrations.zig` and
`src/domain/display_prefix.zig` spell out `std.mem.Allocator` inline.
Both styles currently co-exist.

Pick one. Either alias `Allocator` in the modules that don't, or
unalias the modules that do.

Touches every file that takes a `std.mem.Allocator` parameter.

---

## Pick one test-name convention across src/commands/

`src/commands/prime.zig` uses bare scope names
(`test "prime writes ..."`). Newer command files use the
`<scope>: <case>` colon-prefix form
(`test "init: returns exit 1 ..."`). The repo now has both styles in
the same directory.

The colon-prefix form already dominates; bring `prime.zig` in line, or
record the divergence as accepted in `AGENTS.md` / `CONTEXT.md`. Pin
this before more command files make the inconsistency more expensive.

Touches `src/commands/prime.zig`.

---

## Investigate terminal escape handling for rendered remote text

Slice 3 rejects NUL bytes in `tk add` messages but otherwise allows
control characters and terminal escape sequences to remain in titles and
bodies. That is acceptable for local creation because output escaping is
a cross-command rendering concern, not an add-specific parser rule.

Before remote pull/list/show output becomes broadly useful, do a
security pass on whether a synced Remote issue could inject terminal
escape sequences into `tk list`, `tk show`, creation/update output, or
agent briefing text. Decide whether Ticket should strip, escape, or
otherwise render-control user/remote text at output boundaries.

Touches future output rendering code across `tk list`, `tk show`, and
any command that prints Remote-provided titles or bodies.

---

## Stress-test Repository Store locking under parallel agents

The Repository Store enables WAL and `PRAGMA busy_timeout = 5000`, and
local Ticket creation is intended to be a short SQLite write transaction. That
should be enough for normal local use, but Ticket's agent-first workflow
means several agents may run `tk add`, `tk update`, or sync-adjacent
commands against the same Repository Store at nearly the same time.

Add a focused concurrency stress test once the first few write commands
exist. Exercise parallel local writes against one Repository Store,
observe whether busy timeouts surface in practice, and decide whether the
current retry-at-the-user-level behavior is enough or whether Ticket needs
an internal retry/backoff policy around short write transactions.

Touches Repository Store write helpers and write-command diagnostics.

---

## Design the Mutation Failure / Adapter Failure record shape

The Backend Adapter sync skeleton needs the structured failure type backing
`mutations.failure_json` (per CONTEXT.md, Mutation
Failure). It must carry retry classification (rate-limited,
validation, sync-conflict, auth, transient-network) plus enough
context for the sync engine to schedule retries and for `tk sync log`
to render a human-readable summary.

This is *not* the `Diagnostic` type in `src/store/diagnostic.zig`.
Diagnostic is an ephemeral 256-byte ASCII scratch buffer for capturing
transient SQLite errmsg across rollback boundaries. Mutation Failure
is persisted JSON with a stable schema, returned to the user days
later by `tk sync log`. Don't reuse the type; lifetimes and audiences
are different.

Likely shape: `union(enum) { rate_limited: { retry_after_s,
endpoint }, validation_failed: { field, reason }, sync_conflict: { ...
}, auth_failed, transient: { detail } }` serialized via `std.json`
into the existing `failure_json text check(json_valid(...))` column.
Pick the variant set when the first concrete adapter (GitHub or Jira)
makes real failure modes visible; don't design from imagination.

When this lands, also extend CONTEXT.md with Adapter Failure as a
sub-concept of Mutation Failure (the architecture review during the
error-handling refactor specifically flagged this as the
right moment for a glossary addition).

Reference shapes worth studying when designing this: the Zig
compiler's `Zcu.ErrorMsg` heap graph (`src/Zcu.zig`) and the packed-
arena `std.zig.ErrorBundle` (`lib/std/zig/ErrorBundle.zig`). Both
exist because the compiler has the same pressures sync will face —
multiple structured failures, stable shape across processes — and
the patterns are language-team-vetted. Don't import them wholesale
(the compiler's machinery is justified by IPC and incremental cache
that Ticket does not have), but use them as a known-good
counterweight when sync designs the variant set.

Touches `src/store/migrations.zig` schema, future
`src/remote/<adapter>.zig`, future `src/sync/engine.zig`,
`CONTEXT.md`.

---

## UTF-8-safe truncation in Diagnostic.capture

`Diagnostic.capture` byte-truncates oversize input at 256 bytes
without aligning to a UTF-8 boundary. For the current consumer
(SQLite errmsg rendered with `{s}` to stderr) this is fine — SQLite
errmsg is mostly ASCII and byte-truncation produces valid output.

When a non-stderr consumer materializes (a structured log encoder,
JSON serializer, or network-frame transport that validates UTF-8),
the partial code point at the tail will be rejected. Walk back from
`n` to the start of the last well-formed UTF-8 code point before
storing `len`, e.g. via `std.unicode.utf8ByteSequenceLength` on the
lead byte at the truncation point.

Defer until a structured consumer exists; the current single
consumer never sees the failure mode.

Touches `src/store/diagnostic.zig`.
