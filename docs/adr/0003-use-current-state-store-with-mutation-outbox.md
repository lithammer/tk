# Use a current-state store with a mutation outbox

tk keeps current **Ticket** and **Epic** state in the **Repository Store**.
Normal CLI reads use that state directly. The append-only **Mutation Log**
preserves Backend intent for retries, receipts, and sync without making reads
replay an event history.
