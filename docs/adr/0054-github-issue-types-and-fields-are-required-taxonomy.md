# GitHub issue types and issue fields are required taxonomy

GitHub's private preview of issue types and issue fields now reaches personal
accounts enrolled in it. On 2026-10-21, with `gh` 2.100.0, both
`lithammer/tk` and `lithammer/tk-gh-playground` (`isInOrganization: false`)
returned a non-null `Repository.issueTypes` (Task, Bug, Feature) and four
issue fields from the starter pack: Priority (single-select: Urgent, High,
Medium, Low), Effort (single-select: High, Medium, Low), Start date, and
Target date. The account owner then added a custom `Epic` type. Types and fields
belong to the account or organization, not the repository: both repositories
returned the same IDs.

ADR-0021 built a fallback because personal repositories had no issue types,
and gh-69 weighed labels against Projects v2 because Priority had no native
slot. Neither holds now. This
decision records what tk requires of a GitHub Backend now that the native
slots exist.

## Decision

### tk requires its taxonomy and names what is missing

The GitHub Adapter requires these by exact, case-insensitive name:

| Kind  | Name     | Required by                         |
|-------|----------|-------------------------------------|
| type  | Bug      | Bug Promotion                       |
| type  | Epic     | Epic Promotion                      |
| field | Priority | Ticket Promotion, Pull              |
| field | Effort   | Ticket Promotion, Pull              |

A missing one fails the operation with an error that names it and the owner,
for example `required issue type 'Epic' is missing on lithammer`. tk never
creates taxonomy (ADR-0049 rule 1). There is no stored capability record and
no per-field switch between Shared and Local: if tk uses it, the Backend must
have it.

Promotion reports a missing type as an Adapter read error from capability
resolution, which aborts preflight and leaves the outbox empty; the planner
does not aggregate it with other findings. Pull has no capability pass, so it checks the fields itself and reports a
missing one as an Adapter read error. A deleted Priority field reads back as
no value on every issue, and mapping that through ADR-0049 rule 3 would
overwrite every local Priority.

The repositories that pass today belong to an account enrolled in the
preview. tk is pre-v1 with one known user, so tk assumes the preview rather
than detecting it; a personal account outside it gets the missing-type error.

### Ticket Kind uses the native type only

Bug Promotion creates the issue with the `Bug` type ID in the one GraphQL
`createIssue` call ADR-0021 chose. The `bug` Label fallback leaves Promotion
and Pull, and with it the repository-ownership read that chose between them.
Pull maps a `Bug` type to Bug and every other case, untyped included, to Task.

`gh` 2.100.0 showed no field support: `gh issue create --help` lists `--type`
but no field flag, and `gh issue view --json issueFieldValues` is rejected as
an unknown field. GraphQL `CreateIssueInput` takes
`issueTypeId`, `issueFields`, and `parentIssueId` together, so one mutation
creates the issue with its type and fields. A probe created playground #25 as
a Task with Priority and Effort set in one call.

Feature stays unmapped. Its only difference from Task on GitHub is the two
date fields, which tk does not model, and asking the user to choose between
Task and Feature buys no behaviour. Pull reads Feature as Task, and tk never
pushes it.

### Item Class rides on the Epic type, push-only by structure

Epic Promotion sets the `Epic` type at creation. Pull ignores the type on an
Epic. This is push-only, which ADR-0049 refuses for a field, but Item Class is
not one: tk cannot change an Item's class, so Pull has nothing to be
authoritative over. ADR-0049 exempts relationships on different structural
grounds, an edge with no local Item at its far end. A human who retypes an
Epic issue on GitHub changes nothing locally, and a Ticket retyped to Epic
reads as Task.

The type does not let a Ticket contain Tickets. GitHub allows sub-issues on
any type; tk's own model allows only an Epic as a container. The type is the
precondition for lifting that: it lets GitHub tell an Epic from a Task with
sub-tasks.

### Priority has four levels and crosses as a Shared Field

Priority shrinks from `P0`..`P4` to `P0`..`P3`, and an existing `P4` becomes
`P3`. A tk level must map to its own Backend level, or a pushed value comes
back changed. GitHub has four levels, Embark's Jira scheme has four (Low,
Medium, High, Critical), and Jira's default has five (Lowest to Highest).
Four is the most that maps cleanly to all three. Work held for "someday"
already has a home in `triage`.

| tk | GitHub |
|----|--------|
| P0 | Urgent |
| P1 | High   |
| P2 | Medium |
| P3 | Low    |

Priority becomes a Shared Field (ADR-0049): Promotion sets it, a local change
pushes it, and Pull imports it, shielded while a local Priority Mutation is
pending. A single-select holds one value, so the ambiguous label sets rule 3
covers cannot arise. An issue with no Priority, or with an option
tk does not name, maps to `P2`, the default a new Ticket gets. Epics have no
Priority, so tk neither sets nor reads it on an Epic issue.

A local Priority change queues a `set_ticket_priority` Mutation. Its payload is the target
level, and Apply sends it as a GraphQL `setIssueFieldValue`, which a probe
showed replaces an existing value (playground #26: unset, then Medium, then
Low). Pull shields Priority per Ticket while that Mutation is `pending` or
`failed`, in the shape ADR-0044 gives title and body. A triage Ticket has no
Priority, so Pull leaves it unset. Promotion refuses triage and no command
returns a Ticket to triage, so a backend-bound triage Ticket does not arise;
if one ever does, `tk accept` assigns its Priority locally without a push.

Pull treats an issue typed `Epic` as an Epic: it reads no Priority and needs
no Priority field. For any other issue it requires the field and all four
options, and fails with the named error otherwise. Adopt does not read
Priority, because `gh` 2.100.0 cannot (`gh issue view --json issueFieldValues`
is rejected); an Adopted Ticket starts at `P2` and the next Pull imports the
Backend value.

### Effort replaces the planned Ticket Size

tk-194 planned a local Size: how much work a Ticket is expected to take,
apart from how much it matters. GitHub's Effort field is that concept, and it
sits only on Task and Bug in the starter pack, which matches tk-194's rule
that Epics carry none. Effort is optional, takes `low`, `medium`, or `high`,
and crosses as a Shared Field. An issue with no Effort, or an option tk does
not name, maps to unset.

A local change queues a `set_ticket_effort` Mutation whose payload is the
target value or none. Apply sends a value as `setIssueFieldValue` and none as
`deleteIssueFieldValue`. A probe (playground #26, 2026-10-24) showed the delete
reports `success` both when it removes a value and when the field is already
unset, so a replayed clear converges. Pull shields Effort per Ticket while that
Mutation is `pending` or `failed`, as it does Priority.

## Considered Options

**Record capabilities at `tk remote set`.** `tk remote set github` would probe
the repository and store which fields it shares, so a field enabled later
would not start syncing silently. Rejected for now: requiring every field
removes that hazard with less machinery, and ADR-0033 keeps `tk remote set`
free of Backend calls. Revisit when a second user needs a partial taxonomy.
Jira may need set-time checks anyway, since its Remote carries a site and
project.

**Optional fields, Shared where present and Local where absent.** Rejected.
An admin enabling Priority later would make the next Pull read "no value" on
every promoted Ticket and overwrite each local Priority.

**Keep five levels and edit GitHub's options to `P0`..`P4`.** Rejected. tk
would then depend on one customization of the starter pack, and a five-level
tk still cannot map cleanly onto a four-level Jira scheme.

**Map Feature as a third Ticket Kind.** Rejected; see *Ticket Kind uses the
native type only*.

**An `epic` Label fallback for an organization that will not create the type.**
Deferred. Anyone with triage access to a repository can create a label;
only an owner or admin can create a type. Because Pull ignores Item Class, a
label costs one more branch in Epic Promotion and nothing in Pull, so it is
the planned answer for the first user who hits the missing-type error in an
organization they do not administer.

## Consequences

- Amends ADR-0021: the `bug` Label fallback, the repository-ownership read,
  and the gh-49 revisit condition are gone; Epic Promotion moves to the
  GraphQL `createIssue` path Bug already uses.
- Amends ADR-0036: every GitHub Promotion creates through `createIssue`, and
  the `gh issue create` receipt and 401 rules go.
- Amends ADR-0049: Item Class joins relationships as push-only by structure,
  and Priority crosses from Local to Shared on GitHub.
- Backend data is already migrated: `lithammer/tk` issues #52, #53, and #54,
  the only ones that carried the `bug` Label, now carry the `Bug` type
  instead, and the open Epics (gh-43, gh-61, gh-65) carry the `Epic` type.
- Before Pull reads Priority, tk must push the current Priority of every
  Adopted Ticket that is not `done`. Otherwise the first Pull maps each
  unset issue to `P2`. Migration 019 does it: it queues one `pending`
  `set_ticket_priority` Mutation per Backend Ticket Pull can reach (open, or
  done with an unresolved close), and that row is the
  Pull shield, so no Pull can run between the upgrade and the push. Skipping a
  failed push in `tk sync skip` gives up the shield. Effort needs no backfill:
  unset maps to unset.
- GitHub did not enforce which types a field is pinned to through the API: an
  untyped probe issue (#26) accepted Effort. tk sets Effort only on Tickets.
  This rests on one probe; GitHub's server is not open source.
- The Jira Adapter (tk-35) must map Priority by rank, not by name: priority
  schemes differ per space, and Embark's and Jira's default spell their
  levels differently.
- GLOSSARY.md changes with each implementing Ticket: Priority loses `P4` and
  "local-only", Effort enters, and the Reserved Representation example and
  the GitHub Adapter invariant stop citing the `bug` Label.
