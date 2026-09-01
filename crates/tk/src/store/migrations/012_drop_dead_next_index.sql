-- Drop `items_next_idx` (ADR-0045). No query plan in the crate reaches it, and
-- its `(priority, created_seq)` key cannot serve the Effective-Priority-led
-- ordering ADR-0015 gave `tk next`.
--
-- A plain index drop, so foreign keys stay enforced: ADR-0028's rebuild governs
-- CHECK changes, not indexes. No `if exists` — every path to version 11 creates
-- the index, so a store missing it is a rebuild that lost an object, and this
-- statement should fail loudly rather than pass over it.
drop index items_next_idx;
