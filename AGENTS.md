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
