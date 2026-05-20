# Ticket

Ticket is an agent-first command-line tool for managing work items through a simple local interface and pluggable issue-tracker backends.

## Language

**Ticket Project**:
The project and domain name for the command-line tool.
_Avoid_: Tickets

**Ticket**:
A backend-agnostic work item managed through **Ticket Project**.
_Avoid_: Issue, Task, Bead

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
A local checkout context, usually a git worktree, scoped to a **Ticket** or **Epic**.
_Avoid_: Worktree

**Workspace Scope**:
The local-only association between a **Workspace** and a **Ticket** or **Epic**.
_Avoid_: Workspace Binding

**Worktree Config**:
Git's per-worktree configuration used to store **Workspace Scope** in v1.
_Avoid_: Workspace File

**Inferred Workspace Scope**:
A **Workspace Scope** discovered from the current branch name rather than stored configuration.
_Avoid_: Implicit Binding

**Ticket Branch**:
A git branch created or recognized by **`tk`** using the pattern `tk/<display-id>-<slug>`.
_Avoid_: Work Branch

**Start**:
The **`tk`** command intent for marking a **Ticket** or **Epic** active.
_Avoid_: Claim

**Stop**:
The **`tk`** command intent for moving an active **Ticket** or **Epic** back to `open`.
_Avoid_: Reopen, Pause

**Prime**:
The **`tk`** command intent for generating scope-aware agent briefing output.
_Avoid_: Memory Dump

**Workspace Scope Source**:
The way **`tk`** determined the active **Workspace Scope**: configured, inferred, or none.
_Avoid_: Binding Source, Scope Source

**Repository Store**:
The shared SQLite-backed local state for **Ticket Project** within one version-control repository.
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
A component that maps between **Ticket Project** domain concepts and a specific **Backend**.
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
A durable local intent to modify backend-backed **Ticket Project** domain state through a **Backend**.
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

- **`tk`** is the command-line executable for **Ticket Project**.
- A **Ticket** may be backed by a GitHub issue, Jira issue, Beads bead, or another backend-specific work item.
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
- **Labels** are descriptive facets and do not replace **Priority**, **Ticket Kind**, **Epic** membership, **Item Status**, **Dependencies**, or **External Blockers**.
- **Labels** are deferred from v1.
- A **Ticket** has exactly one **Item Status**.
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
- A **Workspace** may have zero or one **Workspace Scope**.
- A **Workspace Scope** references exactly one **Ticket** or **Epic**.
- **Workspace Scope** provides context for scoped selection and reporting; it is not an implicit target for item commands.
- Whether **`tk list`** defaults to **Workspace Scope** is deferred from v1.
- **Workspace Scope** is local-only and is not synced to backends.
- **Workspace Scope** is stored in **Worktree Config** in v1.
- Non-git **Workspace Scope** storage is deferred from v1.
- **Worktree Config** scope takes precedence over **Inferred Workspace Scope**.
- **Inferred Workspace Scope** is read-only and may come from branch names containing a **Display ID** or **Alias**.
- A **Ticket Branch** includes a **Display ID** so **Workspace Scope** can be inferred.
- **Aliases** keep old **Ticket Branches** inferable after **Promotion** replaces the **Display ID**.
- **Display IDs** are globally unique across **Tickets** and **Epics**.
- **Aliases** are globally unique across **Tickets** and **Epics**.
- **Start** sets a **Ticket** or **Epic** to `active`.
- **Stop** moves an active **Ticket** or **Epic** back to `open`.
- **`done`** is terminal in v1: once a **Ticket** or **Epic** is `done`, **Start** and **Stop** refuse to transition it back to `active` or `open`. The **Repository Store** enforces this with a schema trigger; resurrection through a dedicated `tk reopen` command is deferred from v1.
- The **`done`** terminal rule constrains **Item Status** transitions only; title, body, **Priority**, and **Epic** membership remain editable on a `done` item.
- **`tk worktree start`** creates a **Ticket Branch**, creates a git worktree, stores **Workspace Scope**, and marks the scoped item `active` by default.
- **`tk worktree start`** accepts an optional positional path for the worktree.
- Without an explicit path, **`tk worktree start`** creates a sibling worktree by default.
- Configurable worktree layout is deferred from v1.
- **`tk worktree`** reports the current **Workspace Scope** and **Workspace Scope Source**.
- **`tk worktree set <id>`** writes **Workspace Scope** to **Worktree Config**.
- **`tk worktree clear`** removes configured **Workspace Scope** without disabling **Inferred Workspace Scope**.
- **`tk show`**, **`tk update`**, **`tk start`**, **`tk stop`**, **`tk done`**, and **`tk promote`** require an explicit **Display ID** in v1.
- **Prime** provides agent workflow guidance, essential commands, and close-out reminders.
- v1 **Prime** prints static command-owned Markdown embedded from `src/commands/prime.md` with Zig `@embedFile`.
- Commands that inspect **Workspace Scope** identify the active **Workspace Scope** and **Workspace Scope Source** rather than using scope as a hidden item target.
- A **Repository Store** is shared by all **Workspaces** for the same version-control repository.
- A **Workspace Scope** belongs to one **Workspace**, not the **Repository Store**.
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
- The **Repository Store** keeps current **Ticket** and **Epic** state.
- The **Mutation Log** records replayable backend intent and is not the primary
  read model or a general local edit history.
- **`tk next`** selects the ready **Ticket** with the lowest **Priority**, then
  lowest `created_seq`, within the active **Workspace Scope**.
- **`tk next`** is deterministic and does not randomize among candidates.
- **Ticket Kind** does not affect **`tk next`** ordering.
- **Assignees** do not affect **`tk next`** readiness or ordering.
- **`tk next`** does not explain skipped candidates or ranking reasons.
- **`tk next`** has no JSON or structured-output mode in v1.
- When there is no active **Workspace Scope**, **`tk next`** searches ready **Tickets** across the **Repository Store**.
- When **Workspace Scope** references an **Epic**, **`tk next`** searches only
  directly contained **Tickets**.
- When **Workspace Scope** references a **Ticket** that is not ready,
  **`tk next`** does not fall back to other ready **Tickets**.
- **`tk next`** does not filter by **Origin**.
- **`tk next`** does not use **Mutation Log**, **Mutation Failure**, or **Sync
  Cursor** state as readiness inputs.
- **`tk next`** does not emit sync-health warnings.
- **`tk next`** does not change **Item Status**; selecting ready work is
  separate from starting work.
- **`tk next`** has no explicit scope argument; **Workspace Scope** is the
  only scoped selection input.
- Store-facing **`tk next`** selection accepts a resolved **Workspace Scope**;
  command code owns **Workspace Scope** discovery.
- **`tk next`** is flagless in v1.
- Done-item browsing is deferred until there is a concrete workflow for old
  completed work.
- **List Tree** renders **Epics** as top-level rows, child **Tickets** nested under their **Epic**, and unparented **Tickets** as top-level rows.
- **List Tree** uses decorative tree glyphs and compact status, priority, and kind markers without column alignment.
- **List Tree** status markers render **Item Status** as `○` for `open`, `◐`
  for `active`, and `✓` for `done`.
- **`tk next`** does not select **Epics**.
- **`tk list --ready`** keeps the **List Tree** shape and includes non-empty **Epics** as containers for ready child **Tickets**.

## Example dialogue

> **Dev:** "When an agent needs the next **Ticket**, should it call **`tk`** directly?"
> **Domain expert:** "Yes — **`tk`** is the stable command-line interface for **Ticket Project**, regardless of which backend stores the work item."
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
> **Domain expert:** "No — the **Workspace Scope** should point at the relevant **Epic** or **Ticket** unless the user asks for all work."
>
> **Dev:** "Should **Workspace Scope** be stored in an untracked file in every worktree?"
> **Domain expert:** "No — v1 stores **Workspace Scope** in **Worktree Config** to avoid working-tree litter."
>
> **Dev:** "If a branch is named `tk/src-123-fix-login`, should **`tk`** infer scope?"
> **Domain expert:** "Yes — if **Worktree Config** has no scope, **`tk`** may use **Inferred Workspace Scope** from the branch name."
>
> **Dev:** "What should a **Ticket Branch** look like?"
> **Domain expert:** "Use `tk/<display-id>-<slug>` so the branch is recognizable and scope can be inferred."
>
> **Dev:** "What should **`tk start src-123`** do?"
> **Domain expert:** "It should mark **src-123** `active`; **`tk worktree start src-123`** creates a scoped git worktree."
>
> **Dev:** "How should an agent know whether scope was configured or inferred?"
> **Domain expert:** "**`tk worktree`** reports the **Workspace Scope** and **Workspace Scope Source**."
>
> **Dev:** "How should an agent recover workflow context after compaction or a new session?"
> **Domain expert:** "Run **Prime** to get Ticket's agent workflow guidance and essential commands."
>
> **Dev:** "How does an agent inspect the active **Workspace Scope**?"
> **Domain expert:** "It runs **`tk worktree`**, which reports the **Workspace Scope** and **Workspace Scope Source**."
>
> **Dev:** "In an Epic-scoped **Workspace**, should **`tk done`** without an ID close the **Epic** or guess a child **Ticket**?"
> **Domain expert:** "No — **Workspace Scope** is context for selection and reporting. Item commands require an explicit **Display ID**; agents should pass the ID from **`tk next`** or **`tk list`**."
>
> **Dev:** "Should each git worktree have its own ticket database?"
> **Domain expert:** "No — all **Workspaces** for a repository share one **Repository Store**, while each **Workspace** has its own **Workspace Scope**."
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
> **Domain expert:** "It uses local **Priority** first, then creation order, within the active **Workspace Scope**."
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

- "ticket" and "tickets" were both considered for the project name — resolved: **Ticket** is the canonical project name, and **`tk`** is the executable.
- "issue" and "task" were considered for the core work-item object — resolved: **Ticket** is the canonical backend-agnostic object.
- "type" was considered for ticket category — resolved: **Ticket Kind** is the canonical term, and `task` is a kind rather than the work-item object.
- Backend priority mapping was considered for v1 — resolved: **Priority** is a local-only **Local Field**.
- "group", "batch", and "umbrella" were considered for related work — resolved: **Epic** is the canonical backend-agnostic grouping term.
- "parent" and "child" were considered for blocking relationships — resolved: **Dependency** links a **Blocking Item** to a **Blocked Item**.
- "worktree" was considered for local checkout scope — resolved: **Workspace** is the domain term because git worktrees are the main implementation, not the concept itself.
- "workspace binding" was considered for local checkout association — resolved: **Workspace Scope** is the local-only domain term.
- Working-tree files were considered for **Workspace Scope** storage — resolved: v1 uses **Worktree Config** and defers non-git storage.
- Implicit branch scope was considered — resolved: **Inferred Workspace Scope** is read-only and lower precedence than **Worktree Config**.
- Branch names without a Ticket-specific prefix were considered — resolved: **Ticket Branches** use `tk/<display-id>-<slug>`.
- Combining status changes and worktree creation in **Start** was considered — resolved: **Start** marks work active, while **`tk worktree start`** creates scoped git worktrees.
- Static agent workflow dumps were considered — resolved: v1 **Prime** prints reviewed project-specific workflow guidance.
- Dynamic **Prime** output was considered for v1 — resolved: v1 prints static command-owned Markdown from `src/commands/prime.md`.
- Configurable worktree root and layout were considered for v1 — resolved: **`tk worktree start`** supports default sibling worktrees and explicit paths only.
- Hiding scope origin was considered — resolved: **`tk worktree`** reports **Workspace Scope Source**.
- Defaulting item commands from **Workspace Scope** was considered — resolved: **Workspace Scope** is not an implicit item target; commands that inspect, update, or promote a specific item require an explicit **Display ID** in v1.
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
