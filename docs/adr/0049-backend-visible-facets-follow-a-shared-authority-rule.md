# Backend-visible facets follow a shared-authority rule

gh-68 asked whether a Ticket should carry comments, labels, and assignees, and
whether Work State should show on a Backend. Six open Tickets hang on the
answer, and three of them — Priority on GitHub (gh-69), execution mode
(gh-77), and release grouping (tk-214) — say in their own bodies that they are
the same family of question. A verdict per concept would leave each of them to
argue its own crossing. This decision records the rule instead, then applies
it to the concepts gh-68 named.

## Decision

### Two classes of field, and one test between them

A field on a Ticket or Epic is one of two classes.

The Repository Store owns a **Local Field** outright. No Backend reads or
writes it. Selection State, Work State, Closing Reason, and today Priority
are Local Fields.

A **Shared Field** is one whose authority tk shares with a Backend: Backend
Pull imports it, and a Mutation may push it. Title, body, Lifecycle, and Ticket
Kind are Shared Fields.

A field is Shared — whether it is new or crossing from Local — exactly when
both hold:

1. the Backend has a slot for it: native, or a Reserved Representation as
   defined below; and
2. tk accepts the Backend as authoritative for it on Pull, the way it does for
   Ticket Kind after creation.

tk refuses push-only visibility — writing a value to the Backend that Pull
never reads back. It is the half-sync that ADR-0021 declined for relationship
read-back, ADR-0023 for Closing Reason, and ADR-0027 for Selection State: the
two sides drift, and nothing says which is right.

Relationships sit outside this rule and stay push-only for a structural reason,
not a visibility choice. Dependencies and Epic Membership are edges, and an
edge to a Backend object nobody has Adopted has no local Item to point at
(ADR-0021, ADR-0034). gh-66 owns read-back for the edges that can round-trip.

### The Reserved Representation contract

tk has no Label field, and this rule gives it none: a free-text facet has no
typed authority tk could share. The glossary's earlier label rule — labels
must not replace Priority, Ticket Kind, Item Status, Dependencies, or External
Blockers — constrained a tk Label concept that does not exist. The `bug` label
gh-48 reserved is the shape that replaces it. A **Reserved Representation** is
a pre-existing Backend label or type that a Backend Adapter uses as private
encoding of a typed tk field when the Backend has no native slot. The Adapter
translates in both directions; tk never stores or shows the label as a label.

Generalized from gh-48 (ADR-0021), a Reserved Representation obeys six rules:

1. It names exact, case-insensitive labels or types that already exist on the
   Backend. tk never creates repository taxonomy; Promotion preflight refuses
   when a name is missing.
2. tk owns no prefix. tk can only reserve names the user already has, so
   whether Priority spells `P0` or `priority:P0` is the consumer's call.
3. The mapping from any label set to the typed field is total. Backend Pull is
   all-or-nothing (ADR-0034), so an ambiguous set — two Priority labels, a
   `bug` label beside a native type — maps to a value the consumer names in
   its ADR, never to a Pull failure and never to a guess.
4. Pull is authoritative after creation, shielded while a local Mutation for
   the same field is pending, in the shape ADR-0044 gives title and body.
5. The Adapter adds or removes only the labels it reserves. Every other label
   on the object is untouched, which is what lets third-party vocabularies —
   the `ready-for-agent` and `wayfinder:map` labels that Matt Pocock's skills
   write — coexist with tk on the same issue.
6. The consumer's ADR fixes the names. Per-repository configurable names
   need an ADR of their own, with the consumer that produces the evidence;
   ADR-0033 keeps the GitHub Adapter config-free until then.

### Work State stays Local, on every Backend

Work State fails the second clause, and the reason is whose fact it is, not
how Pull is built. tk's `active` means *this Repository Store* is working the
Item. A Backend's in-progress state — Jira's `indeterminate` status category,
a Projects v2 "In Progress" — means *someone* is. A Backend cannot be
authoritative for a fact only the store holds, so tk accepts no Backend
representation for it, native slot or not. ADR-0043's rule that Pull never
writes Work State follows from this; it is not the cause. tk-35's mapping of
Jira `indeterminate` to `open` is therefore permanent, and its "gh-68 owns any
revision" caveat is withdrawn.

### Assignee leaves the model

This decision removes Assignee from CONTEXT.md and man/tk.1; no code held it.
The one live consumer of an assignee is the wayfinder skill, which claims a
ticket by assigning it to the human driving the map. That claim is a
tracker-level operation, not an assignee: GitHub spells it `--add-assignee
@me`, the skills' local-markdown tracker spells it `Status: claimed`, and tk
already spells it `tk start`. tk-111, widened from the triage skill to Matt
Pocock's skills as a whole, owns the tracker document that maps the skills'
operations onto tk commands.

### The shape a later "someone else has it" field takes

Assignee and Backend progress fail as Work State for the same reason, and they
would pass the test in the same way: as a Shared Field with a native slot,
Backend-authoritative on Pull, that enters **readiness** beside Dependencies
and External Blockers — a Ticket assigned to, or in progress for, someone else
is not ready — and never writes Work State. `tk start` on such a field would
push the store's own identity. Nothing live needs it. wayfinder claims one
HITL ticket at a time for the human driving it, leaves research tickets
unassigned, and the `implement` skill never claims at all, so no
concurrent-session claim exists on GitHub for tk to read. A consumer that needs
one amends this ADR alongside the field's own ADR; it does not re-derive the
shape.

### Comments and External Blockers

Comments are in scope after v1 and owned by gh-43. This decision classifies
nothing about them, because a Comment's class turns on Origin — a Comment on a
Local Ticket has no Backend — and that is gh-44's design. gh-43 records its
three known consumers: Closing Reason (gh-47), the triage skill's agent brief
(tk-111), and the External Blocker reason (tk-19).

External Blockers are Local. The reason is free text with no native slot,
GitHub's own "blocked by" is a Dependency tk already pushes, and a push-only
comment fails the test the way Closing Reason did in ADR-0023.
`add_external_blocker` and `resolve_external_blocker` leave the V1 Mutation
Type list. Neither has ever had a payload or `BackendEdit` variant, and one
such row fails the entire sync run at load, so removing them removes a trap,
not a capability.

## Considered Options

**Keeping the label rule as a rule about GitHub labels**, constraining what
any label on the Backend may mean. Then `bug` is a standing exception, and
every reserved label — gh-69's `P0`..`P4`, an execution-mode label — revises
the rule again. Rejected: the code already encodes Ticket Kind as a label, and
a rule that bends at every consumer is not a rule.

**A general Label field.** Rejected. tk-214 asked whether "a general label
facet gh-68 might introduce anyway" could hold release grouping. It cannot: a
free-text facet has no typed authority to share, and it would duplicate what
Epics, Kind, and Priority already type. A grouping is born Shared or Local by
the test above.

**Per-field crossings with push-only allowed**, each consumer deciding on
"visibility" grounds. Rejected: it is the half-sync three ADRs already
declined, and it hands four consumers four chances to reintroduce it.

**Work State on GitHub through a Projects v2 Status field.** Rejected for the
reason above: the fact is the store's. The facts about Projects v2 are
recorded so nobody re-derives them. From
`cli/cli` at v2.100.0 and GraphQL introspection on 2026-09-04: `gh project
item-add` then `gh project item-edit --field Status --value "In Progress"` is
two writes per start or stop; the value lives on the `ProjectV2Item`, so an
issue in two projects has two answers and an issue in none has none; writes
need the `project` OAuth scope, which `repo`, the scope Promotion runs on,
does not include, so every user would re-authenticate `gh`; and whether the
built-in Status field can be renamed or deleted has no primary source either
way.

**Work State on GitHub through a reserved `active` label.** Rejected for the
same reason. A label would otherwise satisfy the contract's first rule,
which is why the label rule alone could never have carried this refusal.

**Assignee as Work State.** Rejected. gh-68's body gave three objections:
assignment is a planning signal set before work, so Pull would import
assigned-but-untouched backlog as `active`; a Ticket may carry several
Assignees, so "assigned to someone else" has no single `active` answer; and a
teammate unassigning you would stop local work, the clobber class tk-108
fixed. Under wayfinder's convention, where the assignee *is* the claim, the
first two do not arise. The third does, and it is the same problem: the
Backend's fact is about someone, the store's is about itself. That is what
makes a readiness input the right shape and a Work State write the wrong one.

**Classifying Comments as Shared here.** Rejected: gh-46 already presumes sync,
restating it adds nothing, and a Comment's class turns on Origin in ways gh-44
has to work out.

**Deleting the two External Blocker Mutation Types in this change.** Deferred
to tk-19. The `mutations` CHECK enumerates every Mutation Type, so the deletion
is a table rebuild in the ADR-0028 shape, with the Store Backup ADR-0048
requires, and tk-19 touches exactly that code.

## Consequences

- CONTEXT.md: Local Field loses "in v1"; Shared Field and Reserved
  Representation enter the glossary; Label and Assignee leave it, together
  with the Relationships, dialogue, and flagged-ambiguity lines that carried
  them; the V1 Mutation Type list drops the two External Blocker types.
  ARCHITECTURE.md is unchanged: its "Priority remains a Local Field" is
  present fact.
- Amends ADR-0021: the contract above supersedes its label paragraph and its
  "tk-21 retains ownership" sentence. Closes the question ADR-0043 left open.
- Unblocks gh-69. Priority crosses only if tk accepts a human relabeling
  on GitHub as changing local Priority (rule 4); gh-69 names the two-labels
  value (rule 3) and the spelling (rule 2).
- tk-111 widens to Matt Pocock's skills, with the tk tracker document as its
  deliverable. tk-111 weighs its label-bridge reading against this test: a
  Reserved Representation of Selection State with Pull authoritative, or
  nothing.
- gh-77 inherits the contract for any label representation of execution mode,
  and the fact that wayfinder's own HITL/AFK is a per-ticket-type attribute.
- tk-214's grouping is born Shared if GitHub Milestones are its slot and tk
  accepts Backend authority for it, else Local.
- tk-19 shrinks to CLI and rendering, and owns the migration that drops the
  two Mutation Types from the enum and the `mutations` CHECK.
- tk-35 drops its gh-68 caveat.
