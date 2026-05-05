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

**Repository Store**:
The shared SQLite-backed local state for **Ticket Project** within one version-control repository.
_Avoid_: Workspace Store, Global Store

**Backend**:
A system that can store or retrieve **Tickets**, **Epics**, and **Mutations**.
_Avoid_: Provider, Connector

**Primary Backend**:
The single active **Backend** a repository syncs with by default.
_Avoid_: Active Backend

**Backend Adapter**:
A component that maps between **Ticket Project** domain concepts and a specific **Backend**.
_Avoid_: Facade, Provider, Connector

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

**`tk`**:
The executable name users and agents run from the command line.
_Avoid_: ticket, tickets

## Relationships

- **`tk`** is the command-line executable for **Ticket Project**.
- A **Ticket** may be backed by a GitHub issue, Jira issue, Beads bead, or another backend-specific work item.
- An **Epic** contains zero or more **Tickets**.
- An **Epic** does not contain other **Epics** in v1.
- A **Ticket** may belong to zero or one **Epic** in v1.
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
- Scoped **`tk`** command output identifies the active **Workspace Scope**.
- A **Repository Store** is shared by all **Workspaces** for the same version-control repository.
- A **Workspace Scope** belongs to one **Workspace**, not the **Repository Store**.
- A **Repository Store** is untracked local state by default.
- A **Backend Adapter** maps **Tickets**, **Epics**, and **Mutations** to one **Backend**.
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
- "workspace store" and "global store" were considered for local state — resolved: a **Repository Store** is shared across all **Workspaces** for one repository.
- Checked-in ticket state was considered for portability — resolved: the **Repository Store** is untracked local state by default.
- "facade", "provider", and "connector" were considered for integrations — resolved: **Backend Adapter** maps domain concepts to a **Backend**.
- "memory" and "note" were considered for agent follow-ups — resolved: follow-ups are **Local Tickets**.
- "publish" and "export" were considered for moving local work to a backend — resolved: **Promotion** converts a **Local Ticket** or **Local Epic** into a backend-backed object.
- Keeping local IDs as visible IDs after **Promotion** was considered — resolved: **Promotion** replaces the **Display ID** and preserves the old local ID as an **Alias**.
- Backend-intended creation by default was considered when a **Primary Backend** exists — resolved: new **Tickets** and **Epics** are local by default to avoid upstream tracker noise.
- "change log" and "audit log" were considered for replayable local intent — resolved: **Mutation Log** stores **Mutations**.
- Generic field patches were considered for **Mutations** — resolved: **Mutations** use named domain operations.
- Comment, label, and assignee mutations were considered for v1 — resolved: they are deferred.
- Event sourcing was considered for current state — resolved: the **Repository Store** stores current state, and the **Mutation Log** acts as an outbox for backend replay.
