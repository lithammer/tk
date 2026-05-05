# Use a current-state store with a mutation outbox

Ticket keeps current **Ticket** and **Epic** state in the **Repository Store** and uses the **Mutation Log** as an append-only outbox for replayable backend intent. This avoids making normal CLI reads depend on event-sourced replay, while still preserving local intent for retries, receipts, and backend synchronization.
