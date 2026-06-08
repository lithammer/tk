# tk

tk is an agent-first command-line tool for managing work items through a simple local interface and pluggable issue-tracker backends.

## Language

**tk**:
The project and domain name for the command-line tool.
_Avoid_: Ticket Project, ticket, tickets

**Ticket**:
A backend-agnostic work item managed through **tk**.
_Avoid_: Issue, Task

**Display ID**:
The identifier shown to users and agents for a **Ticket** or **Epic**.
_Avoid_: Public ID

**Alias**:
A previous **Display ID** that still resolves to the same **Ticket** or **Epic**.
_Avoid_: Old ID

**Ticket Kind**:
The category of a **Ticket**: `task` or `bug`.
_Avoid_: Type

**Local Field**:
A field stored in the **Repository Store** that is not applied to a **Backend** in v1.
_Avoid_: Unsynced Field, Sync Excluded Field

**Priority**:
A local-only ranking for a **Ticket**: `P0`, `P1`, `P2`, `P3`, or `P4`.
_Avoid_: Severity

**Effective Priority**:
The priority used to order a candidate **Ticket** in **`tk next`**, derived from its own **Priority** and the **Priorities** of items it transitively blocks within the active **Scope**. Selection-only; not stored, not displayed by **`tk show`** or **`tk list`**, not synced to **Backends**.
_Avoid_: Inherited Priority, Critical Path Priority, Derived Priority

**List Tree**:
The default **`tk list`** view that renders **Epics** and their child **Tickets** as a tree.
_Avoid_: Flat List

**Epic**:
A backend-agnostic grouping of related **Tickets** that can be tracked and worked as one unit.
_Avoid_: Batch, Ticket Group, Umbrella

**Parent Argument**:
CLI shorthand for placing a **Ticket** under a containing item.
_Avoid_: Parent Domain Model

**Item Status**:
The lifecycle state of a **Ticket** or **Epic**: `open`, `active`, or `done`.
_Avoid_: Todo, In Progress, Closed, Blocked

**Selection State**:
A local-only, **Ticket**-only intake and selection policy: `triage`, `accepted`, or `parked`. `accepted` is the default and the only state **`tk next`** selects; `triage` is captured work awaiting a human decision; `parked` is accepted work intentionally held out of automatic selection. A **Local Field**, distinct from **Item Status** lifecycle and from **Priority** ranking; not synced to a **Backend** and never recorded as a **Mutation**.
_Avoid_: Status, Triage Status, Queue State, Workflow State

**Closing Reason**:
An optional free-text explanation recorded when a **Ticket** or **Epic** is marked `done`, captured by `tk done -m`. A **Local Field** in v1, distinct from a **Comment**; it is stored current state in the **Repository Store** and is not synced to a **Backend**.
_Avoid_: Closing Comment, Done Comment, Resolution Note

**Assignee**:
A person or agent expected to work on a **Ticket**.
_Avoid_: Owner

**Label**:
A descriptive facet for loose filtering or grouping, such as `python`, `ci`, or `docs`.
_Avoid_: Priority, Ticket Kind, Epic Membership, Status, Blocker

**Dependency**:
A relationship where one **Ticket** or **Epic** cannot progress until another **Ticket** or **Epic** is done.
_Avoid_: Parent, Child

**External Blocker**:
A blocker with a human-readable reason that is not modeled as another **Ticket** or **Epic**.
_Avoid_: Blocked Status

**Blocking Item**:
The **Ticket** or **Epic** that must be done before another item can progress.
_Avoid_: Parent

**Blocked Item**:
The **Ticket** or **Epic** waiting on a **Blocking Item**.
_Avoid_: Child

**Workspace**:
A local checkout of the repository, usually a git worktree, that shares the **Repository Store** with every other checkout of the same repository.
_Avoid_: Worktree

**Scope**:
The **Epic** that narrows **`tk next`** and **`tk list`**, supplied as an explicit `<epic-id>` argument or the `TK_SCOPE` environment variable. A **Scope** is **Epic**-only and is never persisted; an absent **Scope** means the whole **Repository Store**.
_Avoid_: Workspace Scope, Workspace Binding, Inferred Workspace Scope, Filter

**Ticket Branch**:
An optional branch-naming convention, `tk/<display-id>-<slug>`, that humans and agents may follow. **`tk`** neither creates nor requires it.
_Avoid_: Work Branch

**Start**:
The **`tk`** command intent for marking a **Ticket** or **Epic** active.
_Avoid_: Claim

**Stop**:
The **`tk`** command intent for moving an active **Ticket** or **Epic** back to `open`.
_Avoid_: Reopen, Pause

**Accept**:
The **`tk`** command intent for moving a **Ticket** from `triage` **Selection State** to `accepted`, assigning a **Priority** in the same step.
_Avoid_: Approve, Triage, Promote

**Prime**:
The **`tk`** command intent for generating scope-aware agent briefing output.
_Avoid_: Memory Dump

**Search**:
The **`tk`** command intent for finding **Tickets** and **Epics** by a
case-insensitive substring of their title.
_Avoid_: Grep, Full-text search

**Grep**:
The **`tk`** command intent for finding *where* a pattern appears in the title
or body text of **Tickets** and **Epics**, rendering each match in context as a
**`tk show`**-style block rather than as a list of items. Where **Search**
answers "which item is it?", **Grep** answers "where does this text appear?".
_Avoid_: Search, Full-text search, Fuzzy search

**Repository Store**:
The shared SQLite-backed local state for **tk** within one version-control repository.
_Avoid_: Workspace Store, Global Store

**Backend**:
A system that can store or retrieve **Tickets**, **Epics**, and **Mutations**.
_Avoid_: Provider, Connector

**Remote**:
The CLI-facing name for a configured **Backend**.
_Avoid_: Backend Command

**Primary Backend**:
The single active **Backend** a repository syncs with by default.
_Avoid_: Active Backend

**Backend Adapter**:
A component that maps between **tk** domain concepts and a specific **Backend**.
_Avoid_: Facade, Provider, Connector

**Backend Pull**:
A **Backend Adapter** operation that imports backend state into the **Repository Store**.
_Avoid_: Fetch, Import

**Mutation Apply**:
A **Backend Adapter** operation that applies one pending **Mutation** to a **Backend**.
_Avoid_: Replay

**Origin**:
The source of authority for a **Ticket** or **Epic**.
_Avoid_: Source

**Local Ticket**:
A **Ticket** whose **Origin** is local and is not synced unless explicitly promoted.
_Avoid_: Memory, Note

**Backend Ticket**:
A **Ticket** whose **Origin** is a **Backend**.
_Avoid_: Remote Ticket, Synced Ticket

**Local Epic**:
An **Epic** whose **Origin** is local and is not synced unless explicitly promoted.
_Avoid_: Local Group

**Backend Epic**:
An **Epic** whose **Origin** is a **Backend**.
_Avoid_: Remote Epic, Synced Epic

**Promotion**:
The act of converting a **Local Ticket** or **Local Epic** into a backend-backed object through the **Primary Backend**.
_Avoid_: Publish, Export

**Promotion Children**:
The directly contained local items included by `tk promote <id> --children`.
_Avoid_: Recursive Promotion

**Mutation**:
A durable local intent to modify backend-backed **tk** domain state through a **Backend**.
_Avoid_: Local Edit, Change, Audit Entry

**Ticket Mutation**:
A **Mutation** that modifies exactly one **Backend Ticket**.
_Avoid_: Ticket Change, Change, Audit Entry

**Epic Mutation**:
A **Mutation** that modifies exactly one **Backend Epic** or its ticket membership.
_Avoid_: Epic Change, Change, Audit Entry

**Mutation Type**:
A named domain operation that describes the intent of a **Mutation**.
_Avoid_: Field Patch, JSON Patch

**V1 Mutation Type**:
A **Mutation Type** supported by the first implementation: `update_ticket`, `update_epic`, `set_item_status`, `add_ticket_to_epic`, `remove_ticket_from_epic`, `add_dependency`, `remove_dependency`, `add_external_blocker`, `resolve_external_blocker`, `promote_ticket`, or `promote_epic`.
_Avoid_: Comment Mutation, Label Mutation, Assignee Mutation

**Mutation Log**:
The ordered local record of **Mutations** waiting to be applied or already applied to a backend.
_Avoid_: Local Edit Log, Change Log, Audit Log

**Mutation Sequence**:
The monotonic local position of a **Mutation** in the **Mutation Log**.
_Avoid_: Log Sequence Number

**Sync Cursor**:
The per-**Backend** record of the latest **Mutation Sequence** successfully applied.
_Avoid_: Offset

**Mutation Receipt**:
Backend confirmation that a **Mutation** was applied.
_Avoid_: Ack

**Mutation Failure**:
The latest structured failure recorded for a **Mutation** that could not be applied.
_Avoid_: Error Log

**Adapter Failure**:
A **Mutation Failure** as produced and classified by a **Backend Adapter**, carrying a failure classification (rate-limited, validation, sync conflict, auth, transient, or unknown) used to render **Sync Log** summaries and, later, to drive retry and recovery policy.
_Avoid_: Adapter Error

**Skipped Mutation**:
A **Mutation** explicitly bypassed during sync without being applied to a **Backend**.
_Avoid_: Ignored Mutation

**Sync Skip**:
The **`tk sync`** mode that marks one failed **Mutation** as skipped and continues sync.
_Avoid_: Skip Command

**Sync Log**:
The **`tk sync log`** view of the **Mutation Log**.
_Avoid_: App Log, Ticket Log

**Sync Conflict**:
A **Mutation Failure** where a **Backend Adapter** refuses to apply a **Mutation** because backend state changed or the target is unavailable.
_Avoid_: Merge Conflict

**`tk`**:
The executable name users and agents run from the command line.
_Avoid_: ticket, tickets

## Relationships

- **`tk`** is the command-line executable for **tk**.
- A **Ticket** may be backed by a GitHub issue, Jira issue, or another backend-specific work item.
- An **Epic** contains zero or more **Tickets**.
- An **Epic** does not contain other **Epics** in v1.
- A **Ticket** may belong to zero or one **Epic** in v1.
- The v1 **Parent Argument** must resolve to an **Epic**.
- Future versions may allow the **Parent Argument** to resolve to a **Ticket** if subtickets are introduced.
- A **Ticket** has exactly one **Ticket Kind**.
- A **Ticket** has exactly one **Priority**.
- **Priority** is a **Local Field** in v1.
- The default **Priority** is `P2`.
- Lower **Priority** numbers sort before higher **Priority** numbers.
- A candidate **Ticket**'s **Effective Priority** is the lowest of its own **Priority** and the **Effective Priorities** of every unfinished **Blocked Item** (**Item Status** `open` or `active`) it reaches through transitive `blocked_by` **Dependencies** within the active **Scope**.
- A `done` **Blocked Item** resolves the **Dependency** and does not contribute to **Effective Priority**.
- An **Epic** in the **Effective Priority** chain contributes the lowest **Effective Priority** over its unfinished child **Tickets**.
- **Effective Priority** propagation stops at the **Scope** boundary; items outside the active **Scope** do not contribute.
- **External Blockers** carry no **Priority** and do not interrupt **Effective Priority** propagation.
- **Labels** are descriptive facets and do not replace **Priority**, **Ticket Kind**, **Epic** membership, **Item Status**, **Dependencies**, or **External Blockers**.
- **Labels** are deferred from v1.
- A **Ticket** has exactly one **Item Status**.
- A **Ticket** has exactly one **Selection State**.
- An **Epic** has no **Selection State**.
- **Selection State** is a **Local Field** in v1.
- The default **Selection State** is `accepted`; newly imported **Backend Tickets** also default to `accepted`.
- A `triage` **Ticket** carries no **Priority**; `accepted` and `parked` **Tickets** carry a **Priority**.
- **Accept** moves a `triage` **Ticket** to `accepted` and assigns its **Priority**; accepting an already `accepted` **Ticket** is a harmless no-op.
- **Accept** preserves a **Ticket**'s **Dependencies** and **External Blockers**, so an accepted **Ticket** may be immediately blocked.
- **`tk next`** and **`tk list --ready`** select only `accepted` **Tickets**; `triage` and `parked` **Tickets** are excluded both as candidates and as **Effective Priority** contributors.
- **Selection State** changes are not **Mutations** and are not synced to a **Backend**; **Backend Pull** preserves a local **Selection State**.
- **Assignee** support is deferred from v1 and may be omitted entirely.
- If **Assignees** are introduced, a **Ticket** may have zero or more
  **Assignees**.
- `active` **Item Status** means the **Ticket** or **Epic** is currently being worked and does not imply assignment.
- An **Epic** has exactly one **Item Status**.
- An **Epic** is only `done` after explicit closure.
- Child **Ticket** completion may suggest closing an **Epic**, but does not close it automatically.
- A **Dependency** has exactly one **Blocking Item** and one **Blocked Item**.
- A **Ticket** or **Epic** may have zero or more **Dependencies**.
- **Dependencies** may connect **Tickets** and **Epics** in any blocking or blocked combination.
- **Dependencies** must not form cycles.
- **Dependency** is distinct from **Epic** membership.
- A **Ticket** or **Epic** may have zero or more **External Blockers**.
- A **Ticket** is ready only when its **Item Status** is `open`, it has no unresolved **Dependencies**, and it has no **External Blockers**.
- Parent **Epic** **Item Status** does not change child **Ticket** readiness;
  an open child **Ticket** under a done **Epic** remains ready when otherwise
  unblocked.
- Parent **Epic** **Dependencies** and **External Blockers** do not change
  child **Ticket** readiness.
- A **Scope** references exactly one **Epic**; a **Ticket** supplied as a **Scope** is a typed error.
- **Scope** narrows **`tk next`** and **`tk list`** to an **Epic** and its child **Tickets**; it is not an implicit target for item commands.
- **Scope** is supplied as an explicit `<epic-id>` argument or the `TK_SCOPE` environment variable; the argument wins when both are present.
- An absent **Scope** means **`tk next`** and **`tk list`** consider the whole **Repository Store**.
- **Scope** is never persisted; it is read per invocation from the argument or environment.
- **Scope** is local-only and is not synced to backends.
- **`tk`** does not store, infer, or report **Scope** from git state.
- **Display IDs** are globally unique across **Tickets** and **Epics**.
- **Aliases** are globally unique across **Tickets** and **Epics**.
- **Start** sets a **Ticket** or **Epic** to `active`.
- **Stop** moves an active **Ticket** or **Epic** back to `open`.
- **`done`** is terminal in v1: once a **Ticket** or **Epic** is `done`, **Start** and **Stop** refuse to transition it back to `active` or `open`. The **Repository Store** enforces this with a schema trigger; resurrection through a dedicated `tk reopen` command is deferred from v1.
- The **`done`** terminal rule constrains **Item Status** transitions only; title, body, **Priority**, and **Epic** membership remain editable on a `done` item.
- **`tk`** does not manage git worktrees; checkout creation is the harness's or the user's responsibility via `git worktree`.
- **`tk show`**, **`tk update`**, **`tk start`**, **`tk stop`**, **`tk done`**, and **`tk promote`** require an explicit **Display ID** in v1.
- **Prime** provides agent workflow guidance, essential commands, and close-out reminders.
- v1 **Prime** prints static command-owned Markdown embedded from `crates/tk/src/commands/prime.md` via Rust `include_str!`.
- **Prime** prints its briefing only when a **Repository Store** is initialized and openable in the current directory; in every other case — no store, outside a git repository, or any store-open failure — it exits 0 with empty stdout and empty stderr so a global agent hook can run it in any directory without noise.
- A **Repository Store** is shared by all **Workspaces** for the same version-control repository.
- A **Repository Store** is untracked local state by default.
- A **Backend Adapter** maps **Tickets**, **Epics**, and **Mutations** to one **Backend**.
- **Remote** is the CLI-facing name for **Backend** configuration.
- v1 supports zero or one configured **Remote**.
- **`tk remote`** shows the configured **Remote**.
- **`tk remote set <kind>`** configures or replaces the **Remote**.
- **`tk remote clear`** removes the configured **Remote** when no pending remote **Mutations** would be orphaned.
- **Remote** authentication is delegated to backend-specific external CLIs.
- A **Backend Adapter** exposes **Backend Pull** and **Mutation Apply** operations.
- Sync runs **Backend Pull** before applying pending **Mutations** in v1.
- A **Mutation Apply** returns a **Mutation Receipt** or a failure.
- The sync engine owns mutation ordering, cursors, retries, and failure policy.
- The sync engine applies pending **Mutations** in global **Mutation Sequence** order in v1.
- The sync engine stops at the first failed **Mutation** in v1.
- A failed **Mutation** keeps a **Mutation Failure** and is retried by the next sync.
- A **Sync Conflict** is a kind of **Mutation Failure**.
- v1 has no automatic merge or local conflict resolution model.
- A failed **Mutation** may become a **Skipped Mutation** only through **Sync Skip**.
- Sync output warns when **Skipped Mutations** exist.
- **Sync Log** inspects pending, failed, skipped, and applied **Mutations**.
- Force-applying conflicting **Mutations** is deferred from v1.
- **Backend Adapters** use injectable subprocess runners for external CLIs such as `gh` and `acli`.
- A repository may have zero or one **Primary Backend**.
- A **Ticket** has exactly one **Origin**.
- An **Epic** has exactly one **Origin**.
- A **Local Ticket** is a **Ticket**.
- A **Backend Ticket** is a **Ticket**.
- A **Local Epic** is an **Epic**.
- A **Backend Epic** is an **Epic**.
- A **Local Ticket** is not synced to the **Primary Backend** unless explicitly promoted.
- A **Local Epic** is not synced to the **Primary Backend** unless explicitly promoted.
- A **Local Ticket** may belong to a **Local Epic** or **Backend Epic** without
  changing its **Origin**.
- Adding a **Local Ticket** to an **Epic** does not imply **Promotion**.
- **Promotion** changes a **Local Ticket** or **Local Epic** into a backend-backed object in place.
- **Promotion** replaces a local **Display ID** with the backend **Display ID**.
- The replaced local **Display ID** remains an **Alias**.
- **Promotion** does not include contained **Tickets** unless `--children` is used.
- v1 **Promotion Children** are directly contained **Local Tickets** only.
- **Promotion Children** do not follow **Dependencies**.
- Newly created **Tickets** and **Epics** are local by default, even when a **Primary Backend** exists.
- Default ticket views include both **Local Tickets** and **Backend Tickets**.
- Ticket views do not render **Origin** as a separate field; common origin is
  inferred from the **Display ID** shape.
- A **Ticket Mutation** is a **Mutation**.
- An **Epic Mutation** is a **Mutation**.
- A **Ticket Mutation** modifies exactly one **Backend Ticket**.
- An **Epic Mutation** modifies exactly one **Backend Epic** or its ticket membership.
- Pre-Promotion edits to **Local Tickets** and **Local Epics** are Repository
  Store current-state changes, not **Mutations**.
- **Promotion** is the boundary where current local state becomes backend
  intent.
- A **Mutation** has exactly one **Mutation Type**.
- The first implementation supports only **V1 Mutation Types**.
- `update_ticket` and `update_epic` modify title and body only.
- Comments, labels, and assignees are deferred from v1.
- A **Mutation Log** contains zero or more **Mutations**.
- A **Mutation** has exactly one **Mutation Sequence**.
- A **Sync Cursor** belongs to one **Backend**.
- A **Mutation Receipt** belongs to one **Mutation**.
- A **Mutation Failure** belongs to one **Mutation**.
- An **Adapter Failure** is a **Mutation Failure** produced by a **Backend Adapter**, which assigns its failure classification.
- The failure classification is one of rate-limited, validation, sync conflict, auth, transient, or unknown; **Backend Adapters** populate it from real failure modes, defaulting to unknown.
- The **Backend Adapter** assigns the classification; the sync engine and recovery workflows own retry and recoverability policy.
- The **Repository Store** keeps current **Ticket** and **Epic** state.
- The **Mutation Log** records replayable backend intent and is not the primary
  read model or a general local edit history.
- **`tk next`** selects the ready **Ticket** with the lowest **Effective Priority**, then lowest own **Priority**, then lowest `created_seq`, within the active **Scope**.
- **`tk next`** is deterministic and does not randomize among candidates.
- **Ticket Kind** does not affect **`tk next`** ordering.
- **Assignees** do not affect **`tk next`** readiness or ordering.
- **`tk next`** does not explain skipped candidates, but may render a rationale for the selected **Ticket** when its **Effective Priority** comes from a **Blocked Item** rather than its own **Priority**. The rationale names the **Ticket** whose **Priority** drives the **Effective Priority** signal, which is not always the item the candidate directly unblocks — in an **Epic**-mediated chain, the named **Ticket** is a child of an **Epic** the candidate blocks, not a direct **Blocked Item**.
- **`tk next`** has no JSON or structured-output mode in v1.
- When there is no active **Scope**, **`tk next`** searches ready **Tickets** across the **Repository Store**.
- When a **Scope** is active, **`tk next`** searches only **Tickets** directly
  contained by that **Epic**.
- **`tk next`** does not filter by **Origin**.
- **`tk next`** does not use **Mutation Log**, **Mutation Failure**, or **Sync
  Cursor** state as readiness inputs.
- **`tk next`** does not emit sync-health warnings.
- **`tk next`** does not change **Item Status**; selecting ready work is
  separate from starting work.
- **`tk next`** takes an optional positional `<epic-id>` argument; absent it,
  **`tk next`** reads `TK_SCOPE`, then falls back to the whole **Repository
  Store**.
- **`tk next`** and **`tk list`** reject a **Ticket** **Scope** argument with a
  typed error rather than narrowing to a single **Ticket**.
- Store-facing **`tk next`** selection accepts a resolved **Scope**; command
  code owns **Scope** resolution from argument or environment.
- **`tk next`** takes only the optional **Scope** argument; it has no other
  flags in v1.
- Done-item browsing through **`tk next`** / **`tk list`** is deferred; those
  views do not surface `done` items. **`tk search`** is the sanctioned path for
  finding a specific `done` **Ticket** or **Epic** by title, since it matches
  every **Item Status**.
- **`tk list`** takes an optional positional `<epic-id>` argument; absent it, **`tk list`** reads `TK_SCOPE`, then renders the whole **Repository Store**.
- When a **Scope** is active, **`tk list`** renders only that **Epic** and its child **Tickets**, and prints a hint that the view is filtered.
- **List Tree** renders **Epics** as top-level rows, child **Tickets** nested under their **Epic**, and unparented **Tickets** as top-level rows.
- **List Tree** uses decorative tree glyphs and compact status, priority, and kind markers without column alignment.
- **List Tree** status markers render **Item Status** as `○` for `open`, `◐`
  for `active`, and `✓` for `done`.
- **`tk next`** does not select **Epics**.
- **`tk list --ready`** keeps the **List Tree** shape and includes non-empty **Epics** as containers for ready child **Tickets**.
- **`tk search`** finds **Tickets** and **Epics** whose title contains the query as a case-insensitive literal substring.
- **`tk search`** covers the whole **Repository Store** and every **Item Status**, including `done`; it ignores **Scope** and is never narrowed by `TK_SCOPE`.
- **`tk search`** matches title text only. Exact **Display ID** / **Alias** lookup is **`tk show`**; title-or-body content search is **`tk grep`**.
- **`tk search`** renders matches reusing **`tk list`** row rendering and chrome, laid out flat without **List Tree** nesting.
- **`tk search`** takes a single required positional query and has no flags in v1; result limiting, **Origin** / **Ticket Kind** / **Priority** / status filtering, and sorting are deferred.
- **`tk grep`** finds **Tickets** and **Epics** whose title or body text matches a regular expression, rendering each match as a **`tk show`**-style block with the body collapsed to the matching lines plus surrounding context.
- **`tk grep`** covers the whole **Repository Store** and every **Item Status**; like **`tk search`** it ignores **Scope** and is never narrowed by `TK_SCOPE`, because a lookup must not be silently narrowed.
- **`tk grep`** matches title and body text; it is content search, distinct from **`tk search`** (title-only item lookup) and **`tk show`** (exact identifier lookup).
- **`tk grep`** is case-sensitive by default, where **`tk search`** is case-insensitive; the divergence is deliberate, matching the `grep` namesake.
- **`tk grep`** renders matches in creation order and never ranks by relevance; ranked recall — full-text and fuzzy — is reserved for future **`tk search`** modes.

## Example dialogue

> **Dev:** "When an agent needs the next **Ticket**, should it call **`tk`** directly?"
> **Domain expert:** "Yes — **`tk`** is the stable command-line interface, regardless of which backend stores the work item."
>
> **Dev:** "When **`tk`** marks a **Backend Ticket** done while offline, where does that intent live?"
> **Domain expert:** "It is recorded as a **Ticket Mutation** in the **Mutation Log** until a **Backend** can apply it."
>
> **Dev:** "When **`tk`** edits a **Local Ticket** before **Promotion**, where does that intent live?"
> **Domain expert:** "It updates current state in the **Repository Store**. It is not a **Mutation** until **Promotion** creates backend intent."
>
> **Dev:** "Should **`tk list`** rebuild current state by replaying the **Mutation Log**?"
> **Domain expert:** "No — the **Repository Store** keeps current state; the **Mutation Log** records replayable backend intent, not local edit history."
>
> **Dev:** "Should **`tk`** store that as a generic status field patch?"
> **Domain expert:** "No — it should store a **Mutation Type** like `set_status` so the backend adapter can preserve intent."
>
> **Dev:** "Should comments, labels, or assignees be **V1 Mutation Types**?"
> **Domain expert:** "No — v1 only supports creation, title/body updates, status, epic membership, dependencies, and promotion."
>
> **Dev:** "When **`tk`** runs inside a git worktree for a Jira backend feature, should it show unrelated work by default?"
> **Domain expert:** "Pass the relevant **Epic** as a **Scope** — `tk list <epic-id>` or `tk next <epic-id>` — when you want to stay inside that feature; otherwise it considers all work."
>
> **Dev:** "Should **`tk`** infer a **Scope** from the current branch name?"
> **Domain expert:** "No — **`tk`** does not infer **Scope** from git state. Branches are usually named after a single **Ticket**, so inference produced a useless one-**Ticket** narrowing and was invisible ambient state with no off-switch."
>
> **Dev:** "Then how does an AFK agent stay inside one **Epic** without restating the ID every call?"
> **Domain expert:** "The launcher exports `TK_SCOPE=<epic-id>`; every **`tk`** subprocess inherits it, so the agent loops bare **`tk next`**. An explicit `<epic-id>` argument overrides it."
>
> **Dev:** "What should a **Ticket Branch** look like?"
> **Domain expert:** "`tk/<display-id>-<slug>` is a fine convention, but it is optional — **`tk`** neither creates nor requires it, and nothing infers **Scope** from it."
>
> **Dev:** "Should **`tk`** own creating the git worktree for a feature?"
> **Domain expert:** "No — `git worktree add` plus a branch is the harness's job. **`tk`** has no worktree command; it only resolves a **Scope** from the argument or `TK_SCOPE`."
>
> **Dev:** "How should an agent recover workflow context after compaction or a new session?"
> **Domain expert:** "Run **Prime** to get **tk**'s agent workflow guidance and essential commands."
>
> **Dev:** "How does an agent inspect the active **Scope**?"
> **Domain expert:** "There is no stored **Scope** to inspect — it is whatever `<epic-id>` argument or `TK_SCOPE` value the command was given (`echo $TK_SCOPE`)."
>
> **Dev:** "In an **Epic** **Scope**, should **`tk done`** without an ID close the **Epic** or guess a child **Ticket**?"
> **Domain expert:** "No — **Scope** is context for selection and reporting. Item commands require an explicit **Display ID**; agents should pass the ID from **`tk next`** or **`tk list`**."
>
> **Dev:** "Should each git worktree have its own Repository Store database?"
> **Domain expert:** "No — all **Workspaces** for a repository share one **Repository Store**."
>
> **Dev:** "Should the **Repository Store** be committed to git so agents can review ticket state in diffs?"
> **Domain expert:** "No — the **Repository Store** is untracked local state by default; portability comes from backend sync or explicit import/export."
>
> **Dev:** "Is Jira a facade or a backend?"
> **Domain expert:** "Jira is a **Backend**; the Jira integration is a **Backend Adapter**."
>
> **Dev:** "Should the CLI command be `tk backend`?"
> **Domain expert:** "No — the domain term is **Backend**, but the CLI-facing command is **Remote**."
>
> **Dev:** "Should **`tk remote add`** support multiple remotes?"
> **Domain expert:** "No — v1 has at most one configured **Remote**, managed with **`tk remote set`** and **`tk remote clear`**."
>
> **Dev:** "Should the GitHub adapter decide which pending **Mutations** to apply next?"
> **Domain expert:** "No — the sync engine owns ordering and cursors; the **Backend Adapter** applies one **Mutation** at a time."
>
> **Dev:** "When an agent finds a follow-up that should not interrupt current work, is that memory?"
> **Domain expert:** "No — it is a **Local Ticket** unless it is explicitly promoted to the **Primary Backend**."
>
> **Dev:** "After **Promotion**, should the local follow-up and the backend issue appear as separate tickets?"
> **Domain expert:** "No — **Promotion** converts the same **Ticket** in place."
>
> **Dev:** "Should promoting an **Epic** automatically promote every local **Ticket** in it?"
> **Domain expert:** "No — use `--children` to include directly contained **Local Tickets** explicitly."
>
> **Dev:** "After promoting **src-123** to GitHub issue **GH-456**, which ID should users see?"
> **Domain expert:** "They should see **GH-456** as the **Display ID**, while **src-123** remains an **Alias** for lookup and structured references."
>
> **Dev:** "Should an agent creating a short-lived follow-up automatically create a GitHub or Jira issue?"
> **Domain expert:** "No — newly created **Tickets** and **Epics** are local by default and require explicit **Promotion** before they reach the **Primary Backend**."
>
> **Dev:** "Should **`tk list`** hide **Local Tickets** when a **Primary Backend** exists?"
> **Domain expert:** "No — default ticket views include both **Local Tickets** and **Backend Tickets**. **Origin** is not a separate rendered field; it is normally inferred from the **Display ID** shape."
>
> **Dev:** "If **src-12** belongs to **src-3**, does that mean **src-12** is blocked by **src-3**?"
> **Domain expert:** "No — **Epic** membership groups work, while a **Dependency** says one item cannot progress until another is done."
>
> **Dev:** "Does `--parent src-3` introduce a parent-child domain model?"
> **Domain expert:** "No — in v1, the **Parent Argument** is CLI shorthand for **Epic** membership."
>
> **Dev:** "Is an **Epic** a type of **Ticket**?"
> **Domain expert:** "No — **Epic** is separate from **Ticket**; only **Tickets** have **Ticket Kind**."
>
> **Dev:** "How can **`tk next`** choose work without guessing?"
> **Domain expert:** "It uses **Effective Priority** first, then own **Priority**, then creation order, within the active **Scope**. **Effective Priority** lets a ready **Blocking Item** outrank lower-priority direct work when finishing it would unblock a higher-priority **Ticket**."
>
> **Dev:** "Should **`tk list`** be a flat table?"
> **Domain expert:** "No — **`tk list`** uses a **List Tree** so **Epics** and child **Tickets** are visible together."
>
> **Dev:** "If every **Ticket** in an **Epic** is done, should the **Epic** close automatically?"
> **Domain expert:** "No — **Epic** closure is explicit because completion criteria may exist outside the current child tickets."
>
> **Dev:** "Does an `active` **Ticket** have to be assigned to someone?"
> **Domain expert:** "No — `active` means current work; **Assignee** support is deferred and may be omitted entirely."

## Flagged ambiguities

- "ticket" and "tickets" were both considered for the project name — resolved: **tk** is the canonical project name, and **`tk`** is the executable.
- "issue" and "task" were considered for the core work-item object — resolved: **Ticket** is the canonical backend-agnostic object.
- "type" was considered for ticket category — resolved: **Ticket Kind** is the canonical term, and `task` is a kind rather than the work-item object.
- Backend priority mapping was considered for v1 — resolved: **Priority** is a local-only **Local Field**.
- Modelling captured-but-unaccepted and accepted-but-held work as a low **Priority** or a fourth **Item Status** was considered (tk-72) — resolved: **Selection State** is a separate local-only, **Ticket**-only field, because lifecycle, ranking, and selection policy each answer a different question; it is not synced and never recorded as a **Mutation** (ADR-0027).
- Sorting **`tk next`** by own **Priority** only was considered — resolved: **`tk next`** uses **Effective Priority** so a ready blocker can bubble above lower-priority direct work when it gates a higher-priority **Blocked Item**.
- Propagating **Effective Priority** across **Scope** boundaries was considered — resolved: propagation stops at the **Scope** boundary so an **Epic**-scoped run stays ordered by what is internal to that **Epic**.
- "group", "batch", and "umbrella" were considered for related work — resolved: **Epic** is the canonical backend-agnostic grouping term.
- "parent" and "child" were considered for blocking relationships — resolved: **Dependency** links a **Blocking Item** to a **Blocked Item**.
- "worktree" was considered for local checkout scope — resolved: **Workspace** is the domain term because git worktrees are the main implementation, not the concept itself.
- A persisted "workspace binding" was considered for local checkout association — resolved: **Scope** is not persisted; it is an explicit `<epic-id>` argument or `TK_SCOPE`, supplied per invocation (ADR-0022).
- Storing **Scope** in working-tree files or **Worktree Config** was considered — resolved: **Scope** is never stored; persisted scope has the same hidden-state smell as inference (ADR-0022).
- Inferring **Scope** from branch names was considered, and shipped against the frozen oracle — resolved: removed; branch names target a single **Ticket**, so inference produced a useless narrowing and could not be turned off (ADR-0022).
- A `tk/<display-id>-<slug>` **Ticket Branch** contract was considered — resolved: kept only as an optional convention; **`tk`** neither creates nor requires it.
- Combining status changes and worktree creation in **Start** was considered — resolved: **Start** marks work active; **`tk`** does not create worktrees, leaving `git worktree` to the harness (ADR-0022).
- Static agent workflow dumps were considered — resolved: v1 **Prime** prints reviewed project-specific workflow guidance.
- Dynamic **Prime** output was considered for v1 — resolved: v1 prints static command-owned Markdown from `src/commands/prime.md`.
- Configurable worktree root and layout were considered for v1 (ADR-0007) — superseded: **`tk`** no longer creates worktrees (ADR-0022).
- Naming the **Epic**-narrowing concept **Filter** was considered — resolved: **Scope** is the term, because it bounds **Effective Priority** propagation and changes which **Ticket** **`tk next`** picks, not merely which rows **`tk list`** shows; "filter" is the verb **`tk list`** performs (ADR-0022).
- Defaulting item commands from **Scope** was considered — resolved: **Scope** is not an implicit item target; commands that inspect, update, or promote a specific item require an explicit **Display ID** in v1.
- A separate `tk list --parent <id>` filter flag was considered — resolved: the positional `<epic-id>` **Scope** subsumes it in v1, since the **Parent Argument** resolves only to an **Epic** and both return the same rows; `--parent` stays the **Parent Argument** flag on `tk add` (ADR-0022).
- "workspace store" and "global store" were considered for local state — resolved: a **Repository Store** is shared across all **Workspaces** for one repository.
- Checked-in ticket state was considered for portability — resolved: the **Repository Store** is untracked local state by default.
- "facade", "provider", and "connector" were considered for integrations — resolved: **Backend Adapter** maps domain concepts to a **Backend**.
- "backend" was considered for the CLI command — resolved: **Remote** is the CLI-facing name for backend configuration.
- Backend orchestration inside adapters was considered — resolved: the sync engine owns orchestration, and **Backend Adapters** expose **Backend Pull** and **Mutation Apply**.
- Force sync was considered for conflicts — resolved: v1 does not force-apply conflicting **Mutations**.
- "memory" and "note" were considered for agent follow-ups — resolved: follow-ups are **Local Tickets**.
- "publish" and "export" were considered for moving local work to a backend — resolved: **Promotion** converts a **Local Ticket** or **Local Epic** into a backend-backed object.
- Keeping local IDs as visible IDs after **Promotion** was considered — resolved: **Promotion** replaces the **Display ID** and preserves the old local ID as an **Alias**.
- Recursive promotion was considered — resolved: `--children` includes direct **Promotion Children** only in v1.
- Backend-intended creation by default was considered when a **Primary Backend** exists — resolved: new **Tickets** and **Epics** are local by default to avoid upstream tracker noise.
- "local edit log", "change log", and "audit log" were considered for
  replayable backend intent — resolved: **Mutation Log** stores **Mutations**,
  not pre-Promotion local edit history.
- Generic field patches were considered for **Mutations** — resolved: **Mutations** use named domain operations.
- Comment, label, and assignee mutations were considered for v1 — resolved: they are deferred.
- Event sourcing was considered for current state — resolved: the **Repository Store** stores current state, and the **Mutation Log** acts as an outbox for backend replay.
- Searching body text in **`tk search`**, by default or behind a `--body` flag, was considered (tk-79) — resolved: **`tk search`** matches title text only; body/content search is a separate, deferred **`tk grep`** that renders **`tk show`**-style match context. A body-only hit has no provenance slot in a reused **`tk list`** row and would read as a false positive (ADR-0025).
- Matching **Display IDs** and **Aliases** in **`tk search`** (exact + prefix, the original tk-79 framing) was considered — resolved: dropped. Exact-identifier lookup duplicates **`tk show`** and prefix recall is thin against short sequential **Display IDs**; search's distinct value is fuzzy title recall (ADR-0025).
- Honouring `TK_SCOPE` in **`tk search`** was considered — resolved: search is a whole-**Repository Store** lookup; silently narrowing it to an ambient Epic would hide the searched-for item, the hidden-state smell ADR-0022 rejected.
- The **`tk grep`** matching model — literal substring, regular expression, FTS5 full-text, or Levenshtein/fuzzy — was considered (tk-113) — resolved: **regular expression** by default. Literal is a trap (the default cannot be flipped to regex later without changing the meaning of `.`/`*`/`(`/`|`); regex is a strict superset that degrades to literal for metacharacter-free patterns. FTS5 and fuzzy produce ranked/scored output that cannot inhabit **`tk grep`**'s deterministic line-context block, so both are reserved for future ranked **`tk search`** recall modes (ADR-0026).
- A relevance/edit-distance ordering for **`tk grep`** was considered — resolved: **`tk grep`** orders matches by creation and never ranks, because ranked output is incompatible with streaming one matched item to stdout at a time and belongs to the **Search** family, not **Grep** (ADR-0026).
