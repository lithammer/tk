# Agent Notes

- Read [README.md](./README.md) for the project overview.
- Read [ARCHITECTURE.md](./ARCHITECTURE.md) before changing module boundaries
  or repository-store invariants.
- Read [CONTEXT.md](./CONTEXT.md) before changing domain language.
- Read [docs/adr/](./docs/adr/) before revisiting recorded design decisions.

## Rust Coding Standards

tk is a Rust project. The canonical implementation lives under `crates/tk/`.
ADR-0018 records the migration method; ADR-0017 defines the verbatim
user-facing messages that must be preserved exactly.

**Always prefer idiomatic Rust.** User-facing behaviour is the contract —
CLI bytes, exit codes, SQL schema, and the ADR-0017 verbatim messages.
Everything else should be written in idiomatic, modern Rust.

**Lean into the type system.** Prefer enums over raw strings, traits over
function pointers, `Result<T, E>` over flag/buffer pairs. If a SQL column
has a CHECK constraint, the domain type is a Rust enum with a `text()`
method returning the SQL spelling; typed values flow through the code,
`text()` is the storage contract.

**Defer evidence-determined types.** When a type's shape depends on what
a future Backend Adapter observes rather than the schema or an existing
ADR, follow ADR-0016's precedent and defer until the consumer ticket
lands.

**Anchor comments in the actual contract.** Reference the ADR, the
CONTEXT.md vocabulary, or the invariant being preserved — not
implementation history. ADR pointers stay; prose reconstructing past
decisions does not.

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
