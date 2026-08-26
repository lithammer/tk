# Keep the architecture document compact

`ARCHITECTURE.md` maps current module boundaries and durable implementation
contracts. It does not keep detailed plans after a feature ships.

Future work belongs in Local Tickets so it appears in `tk list` and `tk next`
and can carry implementation detail in the Ticket body. Durable product
language belongs in `CONTEXT.md`. Command reference belongs in command help,
`man/tk.1`, and tests. Design decisions belong in ADRs. Agent-facing rules
belong in `AGENTS.md`. Once a slice lands, remove its checklist from
`ARCHITECTURE.md` unless it still explains an active boundary or invariant.

This keeps agent context small and stops old slice plans from becoming a stale
source beside code, tests, Tickets, and ADRs. Detailed history moves to git
history and Ticket bodies instead of staying in the architecture guide.

## History

This ADR was written when the document was called `docs/implementation.md`.
The document moved to the repository root as `ARCHITECTURE.md` after the v1
implementation slices shipped.
