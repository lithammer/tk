# Slice 2 follow-ups

Holding area for review-time follow-ups that did not block slice 2. Each
section is written in `tk add -F -` format: the first paragraph is the
ticket title, subsequent paragraphs are the body. Once slice 3 ships
`tk add`, the contents move into the Repository Store and this file is
deleted. Until then, agents looking for "what's left from slice 2" read
this file.

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
Both styles co-exist within slice 2.

Pick one. Either alias `Allocator` in the modules that don't, or
unalias the modules that do.

Touches every file that takes a `std.mem.Allocator` parameter.

---

## Pick one test-name convention across src/commands/

Slice 1's `src/commands/prime.zig` uses bare scope names
(`test "prime writes ..."`). Slice 2's `src/commands/init.zig` and
every other slice-2 file use the `<scope>: <case>` colon-prefix form
(`test "init: returns exit 1 ..."`). The repo now has both styles in
the same directory.

The colon-prefix form already dominates; bring `prime.zig` in line, or
record the divergence as accepted in `AGENTS.md` / `CONTEXT.md`. Pin
this before slice 3 adds another command file and the inconsistency
spreads.

Touches `src/commands/prime.zig`.
