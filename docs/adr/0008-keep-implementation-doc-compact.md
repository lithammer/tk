# Keep the implementation document compact

`docs/implementation.md` is an implementation map, not a slice archive. It should describe the current module boundaries, durable implementation contracts, and the small current implementation queue. It should not retain detailed plans for features after those features have shipped.

Future work belongs in Local Tickets so it appears in `tk list` / `tk next` and can carry tactical implementation detail in the ticket body. Durable product language belongs in `CONTEXT.md`; user-visible CLI contracts belong in `docs/cli.md`; significant design decisions belong in ADRs. Once a slice lands, its checklist-style notes should be deleted from `docs/implementation.md` unless they still explain an active boundary or invariant.

This keeps agent context small and prevents old slice plans from becoming a second, stale source of truth beside code, tests, tickets, and ADRs. The trade-off is that detailed history moves to git history and ticket bodies instead of remaining inline in the implementation guide.
