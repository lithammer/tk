# Content Mutations preserve explicit field intent

When you rename a Ticket, the Backend should keep its body. A full snapshot
cannot tell a rename from a body replacement; on Jira it could overwrite a
rich description the caller never meant to edit.

When `tk update` names a title or body on a backend-bound Item, the content
Mutation carries only the fields named, even if they match the Repository
Store: the Backend may have changed since the last Pull. GitHub writes only
those fields; an explicit empty body clears it. Jira content writes must
follow the same rule. This replaces [ADR-0051]'s full-snapshot delivery rule.

Old snapshots already name both fields and need no rewrite. Store version
23 prevents older binaries from opening a Store with the new format.
Upgrades take the [exclusive association lock][ADR-0024] to keep older
processes out.
Promotion creation still supplies both fields. Forced Promotion
Reconciliation still queues both.

During Backend Pull, the Repository Store keeps the local value of each
content field named by a pending or failed Mutation. A pending rename
protects the title but lets a new Backend body land. Full snapshots still
protect both fields. Only [ADR-0044]'s combined shield changes.

[ADR-0024]: ./0024-auto-apply-forward-migrations-on-open.md
[ADR-0044]: ./0044-backend-pull-shields-content-fields.md
[ADR-0051]: ./0051-update-edits-explicit-fields.md
