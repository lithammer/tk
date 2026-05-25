# Keep the architecture document compact

`ARCHITECTURE.md` is an architecture map, not a slice archive. It should describe the current module boundaries and durable implementation contracts. It should not retain detailed plans for features after those features have shipped.

Future work belongs in Local Tickets so it appears in `tk list` / `tk next` and can carry tactical implementation detail in the ticket body. Durable product language belongs in `CONTEXT.md`; user-visible command reference belongs in command help, `man/tk.1`, and tests; significant design decisions belong in ADRs; agent-facing conventions (code documentation, error handling, testing) belong in `AGENTS.md`. Once a slice lands, its checklist-style notes should be deleted from `ARCHITECTURE.md` unless they still explain an active boundary or invariant.

This keeps agent context small and prevents old slice plans from becoming a second, stale source of truth beside code, tests, tickets, and ADRs. The trade-off is that detailed history moves to git history and ticket bodies instead of remaining inline in the architecture guide.

(Originally written when the document was called `docs/implementation.md`; renamed to `ARCHITECTURE.md` at the repo root after the v1 implementation slices shipped.)
