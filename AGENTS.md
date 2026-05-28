# Agent Notes

- Read [README.md](./README.md) for the project overview.
- Read [ARCHITECTURE.md](./ARCHITECTURE.md) before changing module boundaries
  or repository-store invariants.
- Read [CONTEXT.md](./CONTEXT.md) before changing domain language.
- Read [docs/adr/](./docs/adr/) before revisiting recorded design decisions.

## Porting from Zig

The Zig tree on `main` is the frozen oracle; active development is the Rust
port on `rust-main`. ADR-0018 settles the method.

**Always prefer idiomatic Rust.** Only user-facing behaviour is relevant
to keep — CLI bytes, exit codes, SQL schema, and the ADR-0017 verbatim
messages. Everything else can be refactored or rewritten in idiomatic and
modern Rust; the Zig shapes were workarounds for language constraints
Rust does not share, so do not carry them across.

**Lean into the type system.** Prefer enums over raw strings, traits over
function pointers, `Result<T, E>` over flag/buffer pairs. If a SQL column
has a CHECK constraint, the domain type is a Rust enum with a `text()`
method returning the SQL spelling; typed values flow through the code,
`text()` is the storage contract.

**Defer evidence-determined types.** When a type's shape depends on what
a future Backend Adapter observes rather than the schema or an existing
ADR, follow ADR-0016's precedent and defer until the consumer ticket
lands.

**Do not reference the Zig implementation in Rust code.** No "Ported from
`src/...zig`", no "the Zig oracle's X", no "mirrors / replaces / matches
Zig's Y". This applies to module-level doc comments, function and field
doc comments, inline comments, test names, and `Cargo.toml` comments.

The Zig tree is temporary scaffolding; once the port is done it will not
exist, and these references rot into dead shorthand for a reader who
never saw the Zig source. Anchor comments in the actual contract — the
ADR, the CONTEXT.md vocabulary, the invariant being preserved — and
write the Rust type as if it had always existed. ADR pointers stay;
statements about the ADR's *mechanism* being Zig-shaped do not.

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
