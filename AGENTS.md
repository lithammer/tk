# Agent Notes

- Read [README.md](./README.md) for the project overview.
- Read [ARCHITECTURE.md](./ARCHITECTURE.md) before changing module boundaries
  or repository-store invariants.
- Read [CONTEXT.md](./CONTEXT.md) before changing domain language.
- Read [docs/adr/](./docs/adr/) before revisiting recorded design decisions.

## Porting from Zig

The Zig tree on `main` is the frozen oracle; active development is the Rust
port on `rust-main`. Preserve observable contracts (CLI bytes, exit codes,
SQL schema, ADR-0017 verbatim messages) but collapse Zig shapes into
idiomatic Rust. ADR-0018 settles the method; this section is the checklist.

**Always collapse these Zig shapes:**

- **Typed `Outcome` tagged union** (`union(enum) { success, failure }`) →
  `Result<T, E>`. The Zig union exists because Zig error tags can't carry
  payloads; Rust's `Result` does.
- **`?*Diagnostic` out-param** → `Result<T, E>` with the diagnostic captured
  in the error variant (typically a `thiserror` enum carrying a `String`).
  The single-producer scratch buffer was the same workaround.
- **`*anyopaque` vtable seams** → traits.
- **Manual `deinit` / allocator threading** → ownership + `Drop`.
- **Out-param init helpers** (`buildItemDetail(target: *T, ...)`) → return
  the value, or use a builder.
- **Comptime-evaluated `pub const`** that prebakes byte sequences → `const`
  with the composition done in a `const fn` or inline; the *contract*
  (prebaked at definition time, no runtime cost) survives.

**Lean into the type system. Prefer enums over raw strings.**

If a SQL column has a CHECK constraint (`status in ('open','active','done')`),
the domain type is a Rust enum with `text()` returning the SQL spelling —
not a `&str` passed through the store. The Zig store layer sometimes passed
raw string literals (`origin = "backend"`); the Rust port replaces those
with `Origin::Backend` so the type system catches typos at compile time.
Same goes for `MutationType`, `TicketKind`, `ItemClass`, `Priority`, and
`ItemStatus`. The `text()` method on each is the storage contract; the
typed value is what flows through the code.

**Before porting any new type from `src/domain/`, ask:**

1. Does ADR-0018 list this shape as one to collapse? If yes, collapse it.
2. Is the type's existence in Zig only because Zig lacks `Result` / `Drop` /
   traits? If yes, the scaffolding doesn't transfer.
3. Is the type's shape *schema-determined* (the SQL schema or an existing
   ADR pins it) or *evidence-determined* (it depends on what a future
   Backend Adapter observes)?
   - Schema-determined → port now as a typed enum / struct.
   - Evidence-determined → defer per ADR-0016's precedent until the
     consumer ticket lands; let real consumer pressure settle the shape.

**When an ADR mentions a Zig mechanism** (`?*Diagnostic`, `Outcome.failure`,
`@embedFile`, `std.Io.Writer`, comptime builder), the *mechanism* is usually
collapsed per ADR-0018, but the *contract* the ADR records usually still
applies. Check whether ADR-0018 names the mechanism before porting it
mechanically.

## Code Documentation

- Add doc comments for public functions, structs, methods, constants, and
  important private boundaries as code is introduced or changed.
- Anchor comments in the project docs: [CONTEXT.md](./CONTEXT.md),
  [ARCHITECTURE.md](./ARCHITECTURE.md), and [docs/adr/](./docs/adr/).
- Use tk's domain vocabulary in comments instead of generic terms. For
  example, prefer Repository Store, Workspace Scope, Display ID, Remote,
  Backend Adapter, Mutation, and Mutation Log where those concepts apply.
- Comments should explain contracts, ownership, lifetimes, invariants, and why
  a boundary exists. Avoid comments that only restate the next line of code.
