# Agent Notes

This repository is still in design. Nothing has been implemented yet.

- Read [README.md](./README.md) for the current project overview.
- Read [CONTEXT.md](./CONTEXT.md) before changing domain language.
- Read [docs/adr/](./docs/adr/) before revisiting recorded design decisions.

## Code Documentation

- Add doc comments for public functions, structs, methods, constants, and
  important private boundaries as code is introduced or changed.
- Anchor comments in the project docs: [CONTEXT.md](./CONTEXT.md),
  [docs/implementation.md](./docs/implementation.md),
  [docs/design-questions.md](./docs/design-questions.md), and
  [docs/adr/](./docs/adr/).
- Use Ticket's domain vocabulary in comments instead of generic terms. For
  example, prefer Repository Store, Workspace Scope, Display ID, Remote,
  Backend Adapter, Mutation, and Mutation Log where those concepts apply.
- Comments should explain contracts, ownership, lifetimes, invariants, and why
  a boundary exists. Avoid comments that only restate the next line of code.

## Error Handling

Zig errors are bare enum tags with no payload. The codebase uses three
shapes; pick the one that matches the call site:

- **Bare `error.Foo`** for distinguishing-only failures where the caller
  switches on the tag and renders a stable templated message. Example:
  `proc.Runner.Error` (`ExecutableNotFound`, `SpawnFailed`, `OutOfMemory`).
  The tag is the contract; no payload type.
- **`?*Diagnostic` out-param** when a failure carries a transient string
  the caller wants to surface (e.g. SQLite's `errmsg` captured before
  `rollback` clears it). Stack-allocate `var diag: Diagnostic = .{};`,
  pass `&diag` into the fallible operation, read `diag.message()` after
  observing the error union. Canonical use: `migrations.applyAll` and
  `src/store/diagnostic.zig`. The pattern mirrors
  `std.json.Scanner.Diagnostics` and `std.zon.parse.Diagnostics`.
- **Typed `Outcome` tagged union** when callers dispatch on failure kind
  across multiple stable rendering branches. Each switch arm frees its
  own payload — do **not** add an `Outcome.deinit` that handles some
  arms but not others. Canonical use: `git.discovery.Outcome` in
  `src/git/discovery.zig`.

A fourth shape — a **Mutation Failure** record (per CONTEXT.md) —
arrives with slice 9 Backend Adapters. It is persisted JSON with a
stable schema, distinct from the ephemeral `Diagnostic`. Don't conflate
the two; see [docs/followups.md](./docs/followups.md) for the design
holding area.

Anti-patterns that have been ruled out by review and should not be
reintroduced without a concrete forcing constraint:

- **Module-level `var last_error_buf` or sidecar `lastError()` on a
  long-lived handle.** A research pass over Ghostty, TigerBeetle, Bun,
  ZLS, and the Zig stdlib found no mature codebase using this shape.
  Use `?*Diagnostic` instead.
- **`null`-or-empty-as-sentinel return** where the sentinel encodes
  "I already wrote stderr." Mixes a control-flow signal into a returned
  value; surface the failure as a typed `Outcome` so the caller owns
  stderr.
- **Asymmetric `Outcome.deinit`** that no-ops on some arms but frees on
  others. Per-variant cleanup pushes ownership tracking onto every
  caller and requires a comment at every call site. Have each switch
  arm free its own payload directly.
