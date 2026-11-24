# Store Mutation Failures as flat records

CLI failures often carry little beyond a diagnostic, so the Repository Store
keeps each Mutation's latest **Mutation Failure** as a flat record from the
**Backend Adapter**. The Adapter assigns the class using observed diagnostics
or CLI source code that shows what the tool emits. The sync engine and
recovery workflows decide what to retry.

## Stored contract

tk writes `detail`, `class`, and `retry_after_s` in that order.
The class spellings are `rate_limited`, `validation`, `sync_conflict`, `auth`,
`transient`, and `unknown`. Readers default a missing or unfamiliar class to
`unknown` and a missing retry delay to `null`, and accept extra fields.

This refines the failure record in [ADR-0009](./0009-sync-failure-taxonomy.md).
