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

**Epic**:
A backend-agnostic grouping of related **Tickets** that can be tracked and worked as one unit.
_Avoid_: Batch, Ticket Group, Umbrella

**Parent Argument**:
CLI shorthand for placing a **Ticket** under a containing item.
_Avoid_: Parent Domain Model

**Ticket Status**:
The lifecycle state of a **Ticket**: `open`, `active`, `blocked`, or `done`.
_Avoid_: Todo, In Progress, Closed

**Assignee**:
A person or agent expected to work on a **Ticket**.
_Avoid_: Owner

**Epic Status**:
The lifecycle state of an **Epic**: `open`, `active`, or `done`.
_Avoid_: Closed, Blocked

**Dependency**:
A relationship where one **Ticket** or **Epic** cannot progress until another **Ticket** or **Epic** is done.
_Avoid_: Parent, Child

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

**Mutation**:
A durable local intent to modify **Ticket Project** domain state.
_Avoid_: Change, Audit Entry

**Ticket Mutation**:
A **Mutation** that modifies exactly one **Ticket**.
_Avoid_: Ticket Change, Change, Audit Entry

**Epic Mutation**:
A **Mutation** that modifies exactly one **Epic** or its ticket membership.
_Avoid_: Epic Change, Change, Audit Entry

**Mutation Type**:
A named domain operation that describes the intent of a **Mutation**.
_Avoid_: Field Patch, JSON Patch

**V1 Mutation Type**:
A **Mutation Type** supported by the first implementation: `create_ticket`, `update_ticket`, `set_ticket_status`, `create_epic`, `update_epic`, `set_epic_status`, `add_ticket_to_epic`, `remove_ticket_from_epic`, `add_dependency`, `remove_dependency`, `promote_ticket`, or `promote_epic`.
_Avoid_: Comment Mutation, Label Mutation, Assignee Mutation

**Mutation Log**:
The ordered local record of **Mutations** waiting to be applied or already applied to a backend.
_Avoid_: Change Log, Audit Log

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
- A **Ticket** has exactly one **Ticket Status**.
- A **Ticket** may have zero or more **Assignees**.
- `active` **Ticket Status** means the **Ticket** is currently being worked and does not imply assignment.
- An **Epic** has exactly one **Epic Status**.
- An **Epic** is only `done` after explicit closure.
- Child **Ticket** completion may suggest closing an **Epic**, but does not close it automatically.
- A **Dependency** has exactly one **Blocking Item** and one **Blocked Item**.
- A **Ticket** or **Epic** may have zero or more **Dependencies**.
- **Dependencies** may connect **Tickets** and **Epics** in any blocking or blocked combination.
- **Dependencies** must not form cycles.
- **Dependency** is distinct from **Epic** membership.
- A **Workspace** may have zero or one **Workspace Scope**.
- A **Workspace Scope** references exactly one **Ticket** or **Epic**.
- **`tk`** commands inside a scoped **Workspace** default to the current **Ticket** or **Epic**.
- **Workspace Scope** is local-only and is not synced to backends.
- **Workspace Scope** is stored in **Worktree Config** in v1.
- Non-git **Workspace Scope** storage is deferred from v1.
- **Worktree Config** scope takes precedence over **Inferred Workspace Scope**.
- **Inferred Workspace Scope** is read-only and may come from branch names containing a **Display ID** or **Alias**.
- A **Ticket Branch** includes a **Display ID** so **Workspace Scope** can be inferred.
- **Aliases** keep old **Ticket Branches** inferable after **Promotion** replaces the **Display ID**.
- **Start** sets a **Ticket** or **Epic** to `active`.
- **Stop** moves an active **Ticket** or **Epic** back to `open`.
- **`tk worktree start`** creates a **Ticket Branch**, creates a git worktree, stores **Workspace Scope**, and marks the scoped item `active` by default.
- **`tk worktree start`** accepts an optional positional path for the worktree.
- Without an explicit path, **`tk worktree start`** creates a sibling worktree by default.
- Configurable worktree layout is deferred from v1.
- **`tk worktree`** reports the current **Workspace Scope** and **Workspace Scope Source**.
- **`tk worktree set <id>`** writes **Workspace Scope** to **Worktree Config**.
- **`tk worktree clear`** removes configured **Workspace Scope** without disabling **Inferred Workspace Scope**.
- **Prime** provides agent workflow guidance, essential commands, and close-out reminders.
- v1 **Prime** prints static Markdown embedded from `docs/prime.md` with Zig `@embedFile`.
- Scoped **`tk`** command output identifies the active **Workspace Scope**.
- A **Repository Store** is shared by all **Workspaces** for the same version-control repository.
- A **Workspace Scope** belongs to one **Workspace**, not the **Repository Store**.
- A **Repository Store** is untracked local state by default.
- A **Backend Adapter** maps **Tickets**, **Epics**, and **Mutations** to one **Backend**.
- **Remote** is the CLI-facing name for **Backend** configuration.
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
- **Promotion** changes a **Local Ticket** or **Local Epic** into a backend-backed object in place.
- **Promotion** replaces a local **Display ID** with the backend **Display ID**.
- The replaced local **Display ID** remains an **Alias**.
- Newly created **Tickets** and **Epics** are local by default, even when a **Primary Backend** exists.
- Default ticket views include both **Local Tickets** and **Backend Tickets**.
- Ticket views identify each **Ticket's** **Origin**.
- A **Ticket Mutation** is a **Mutation**.
- An **Epic Mutation** is a **Mutation**.
- A **Ticket Mutation** modifies exactly one **Ticket**.
- An **Epic Mutation** modifies exactly one **Epic** or its ticket membership.
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
- The **Mutation Log** records replayable backend intent and is not the primary read model.

## Example dialogue

> **Dev:** "When an agent needs the next **Ticket**, should it call **`tk`** directly?"
> **Domain expert:** "Yes — **`tk`** is the stable command-line interface for **Ticket Project**, regardless of which backend stores the work item."
>
> **Dev:** "When **`tk`** marks a **Ticket** done while offline, where does that intent live?"
> **Domain expert:** "It is recorded as a **Ticket Mutation** in the **Mutation Log** until a backend can apply it."
>
> **Dev:** "Should **`tk list`** rebuild current state by replaying the **Mutation Log**?"
> **Domain expert:** "No — the **Repository Store** keeps current state; the **Mutation Log** records replayable backend intent."
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
> **Dev:** "If a branch is named `tk/TK-123-fix-login`, should **`tk`** infer scope?"
> **Domain expert:** "Yes — if **Worktree Config** has no scope, **`tk`** may use **Inferred Workspace Scope** from the branch name."
>
> **Dev:** "What should a **Ticket Branch** look like?"
> **Domain expert:** "Use `tk/<display-id>-<slug>` so the branch is recognizable and scope can be inferred."
>
> **Dev:** "What should **`tk start TK-123`** do?"
> **Domain expert:** "It should mark **TK-123** `active`; **`tk worktree start TK-123`** creates a scoped git worktree."
>
> **Dev:** "How should an agent know whether scope was configured or inferred?"
> **Domain expert:** "**`tk worktree`** reports the **Workspace Scope** and **Workspace Scope Source**."
>
> **Dev:** "How should an agent recover workflow context after compaction or a new session?"
> **Domain expert:** "Run **Prime** to get Ticket's agent workflow guidance and essential commands."
>
> **Dev:** "How does an agent know whether **`tk list`** returned global or scoped results?"
> **Domain expert:** "Scoped output identifies the active **Workspace Scope**, and global output is requested explicitly."
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
> **Dev:** "Should the GitHub adapter decide which pending **Mutations** to apply next?"
> **Domain expert:** "No — the sync engine owns ordering and cursors; the **Backend Adapter** applies one **Mutation** at a time."
>
> **Dev:** "When an agent finds a follow-up that should not interrupt current work, is that memory?"
> **Domain expert:** "No — it is a **Local Ticket** unless it is explicitly promoted to the **Primary Backend**."
>
> **Dev:** "After **Promotion**, should the local follow-up and the backend issue appear as separate tickets?"
> **Domain expert:** "No — **Promotion** converts the same **Ticket** in place."
>
> **Dev:** "After promoting **TK-123** to GitHub issue **GH-456**, which ID should users see?"
> **Domain expert:** "They should see **GH-456** as the **Display ID**, while **TK-123** remains an **Alias** for lookup and structured references."
>
> **Dev:** "Should an agent creating a short-lived follow-up automatically create a GitHub or Jira issue?"
> **Domain expert:** "No — newly created **Tickets** and **Epics** are local by default and require explicit **Promotion** before they reach the **Primary Backend**."
>
> **Dev:** "Should **`tk list`** hide **Local Tickets** when a **Primary Backend** exists?"
> **Domain expert:** "No — default ticket views include both **Local Tickets** and **Backend Tickets**, and each row identifies its **Origin**."
>
> **Dev:** "If **TK-12** belongs to **EP-3**, does that mean **TK-12** is blocked by **EP-3**?"
> **Domain expert:** "No — **Epic** membership groups work, while a **Dependency** says one item cannot progress until another is done."
>
> **Dev:** "Does `--parent EP-3` introduce a parent-child domain model?"
> **Domain expert:** "No — in v1, the **Parent Argument** is CLI shorthand for **Epic** membership."
>
> **Dev:** "Is an **Epic** a type of **Ticket**?"
> **Domain expert:** "No — **Epic** is separate from **Ticket**; only **Tickets** have **Ticket Kind**."
>
> **Dev:** "If every **Ticket** in an **Epic** is done, should the **Epic** close automatically?"
> **Domain expert:** "No — **Epic** closure is explicit because completion criteria may exist outside the current child tickets."
>
> **Dev:** "Does an `active` **Ticket** have to be assigned to someone?"
> **Domain expert:** "No — `active` means current work; **Assignees** are tracked separately."

## Flagged ambiguities

- "ticket" and "tickets" were both considered for the project name — resolved: **Ticket** is the canonical project name, and **`tk`** is the executable.
- "issue" and "task" were considered for the core work-item object — resolved: **Ticket** is the canonical backend-agnostic object.
- "type" was considered for ticket category — resolved: **Ticket Kind** is the canonical term, and `task` is a kind rather than the work-item object.
- "group", "batch", and "umbrella" were considered for related work — resolved: **Epic** is the canonical backend-agnostic grouping term.
- "parent" and "child" were considered for blocking relationships — resolved: **Dependency** links a **Blocking Item** to a **Blocked Item**.
- "worktree" was considered for local checkout scope — resolved: **Workspace** is the domain term because git worktrees are the main implementation, not the concept itself.
- "workspace binding" was considered for local checkout association — resolved: **Workspace Scope** is the local-only domain term.
- Working-tree files were considered for **Workspace Scope** storage — resolved: v1 uses **Worktree Config** and defers non-git storage.
- Implicit branch scope was considered — resolved: **Inferred Workspace Scope** is read-only and lower precedence than **Worktree Config**.
- Branch names without a Ticket-specific prefix were considered — resolved: **Ticket Branches** use `tk/<display-id>-<slug>`.
- Combining status changes and worktree creation in **Start** was considered — resolved: **Start** marks work active, while **`tk worktree start`** creates scoped git worktrees.
- Static agent workflow dumps were considered — resolved: v1 **Prime** prints reviewed project-specific workflow guidance.
- Dynamic **Prime** output was considered for v1 — resolved: v1 prints static Markdown from `docs/prime.md`.
- Configurable worktree root and layout were considered for v1 — resolved: **`tk worktree start`** supports default sibling worktrees and explicit paths only.
- Hiding scope origin was considered — resolved: **`tk worktree`** reports **Workspace Scope Source**.
- "workspace store" and "global store" were considered for local state — resolved: a **Repository Store** is shared across all **Workspaces** for one repository.
- Checked-in ticket state was considered for portability — resolved: the **Repository Store** is untracked local state by default.
- "facade", "provider", and "connector" were considered for integrations — resolved: **Backend Adapter** maps domain concepts to a **Backend**.
- "backend" was considered for the CLI command — resolved: **Remote** is the CLI-facing name for backend configuration.
- Backend orchestration inside adapters was considered — resolved: the sync engine owns orchestration, and **Backend Adapters** expose **Backend Pull** and **Mutation Apply**.
- Force sync was considered for conflicts — resolved: v1 does not force-apply conflicting **Mutations**.
- "memory" and "note" were considered for agent follow-ups — resolved: follow-ups are **Local Tickets**.
- "publish" and "export" were considered for moving local work to a backend — resolved: **Promotion** converts a **Local Ticket** or **Local Epic** into a backend-backed object.
- Keeping local IDs as visible IDs after **Promotion** was considered — resolved: **Promotion** replaces the **Display ID** and preserves the old local ID as an **Alias**.
- Backend-intended creation by default was considered when a **Primary Backend** exists — resolved: new **Tickets** and **Epics** are local by default to avoid upstream tracker noise.
- "change log" and "audit log" were considered for replayable local intent — resolved: **Mutation Log** stores **Mutations**.
- Generic field patches were considered for **Mutations** — resolved: **Mutations** use named domain operations.
- Comment, label, and assignee mutations were considered for v1 — resolved: they are deferred.
- Event sourcing was considered for current state — resolved: the **Repository Store** stores current state, and the **Mutation Log** acts as an outbox for backend replay.
