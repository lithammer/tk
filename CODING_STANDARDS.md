# Coding Standards

## Rust

Use idiomatic Rust. Prefer enums for domain states and `Result<T, E>` for
fallible operations. When a SQL column has a CHECK constraint, represent
its values with a Rust enum whose `text()` method owns the SQL spelling.

When a type depends on evidence from a future Backend Adapter, defer its
shape until that consumer exists (ADR-0016).

## Shared APIs

When a shared API changes, including a test helper, inspect callers for
each distinct setup pattern. For optional dependencies and setup builders,
identify a caller that needs the default state. Make dependencies required
at construction when every caller needs them.

For each new mutable borrow, trace the state change and check whether it
belongs to construction or the called operation. Keep required setup in
construction and helper effects within the helper's stated purpose.

## Contracts

Review behavior against CONTEXT.md, ARCHITECTURE.md, and the relevant ADRs.

CLI output, exit codes, SQL schema, and ADR-0017 messages are contracts.
Change them deliberately. When a change revises a recorded decision,
update its ADR in the same change.

## Code Documentation

Document public APIs and important private boundaries where they carry
contracts, ownership, lifetimes, invariants, or external effects.

Use the project's domain vocabulary. Keep ADR pointers and comments that
explain constraints the code cannot show. Remove comments that narrate
implementation history or restate the code.

Prefer a clear name, type, or structure over an explanatory comment.
Write comments in short, direct sentences.

## CLI Output

When a change affects CLI output, inspect a representative rendered example
alongside the diff. Check wording, wrapping, section structure, and whether
the output serves the command's documented purpose.

For conditional output, inspect examples that exercise the changed
conditions. Distinguish prose wrapping from intentionally unwrapped data
rows.

Use existing rendering helpers where the output contract matches.
