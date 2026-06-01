# Scope is an explicit Epic argument or `TK_SCOPE`, not persisted Workspace state

`tk next` and `tk list` are narrowed by a **Scope**: an Epic supplied
explicitly as a positional `<epic-id>` argument or through the `TK_SCOPE`
environment variable, with the argument winning when both are present and an
absent Scope meaning the whole Repository Store. Scope is never persisted.
This removes the entire stored-and-inferred Workspace Scope subsystem —
`tk worktree` (`set`, `clear`, `start`, and bare inspect), `tk.scope` in
Worktree Config, Inferred Workspace Scope from branch names, and Workspace
Scope Source reporting — and supersedes ADR-0007 (default worktree path
layout), whose only consumer was `tk worktree start`.

Scope is **Epic-only**. Passing a Ticket where a Scope is expected is a typed
error (`tk next: scope 'tk-123' is not an Epic`) rather than a degenerate
single-item narrowing or a magic widen-to-parent. A Ticket Scope was the
whole motivation for this change: narrowing `tk next` or `tk list` to one
Ticket only ever echoes back the Ticket the caller already named, so it does
no work. The unit that earns scoped selection is an Epic and its children —
the "work through this feature" loop (`tk next` → implement → `tk done` →
`tk next`) and the focused board (`tk list <epic-id>`).

Scope remains the boundary for Effective Priority propagation (ADR-0015): a
candidate inside an Epic Scope does not inherit urgency from Blocked Items
outside it, so the same Ticket can rank differently scoped and unscoped. This
is why the concept is named **Scope** (a boundary of consideration that
changes `tk next`'s answer) and not **Filter** (a presentational subset that
would not). "Filter" is the verb `tk list` performs and the word its hint
uses (`Scope: <epic-id> (filtered to this Epic and its child Tickets)`); it is
not the concept.

## Why persisted and inferred scope did not earn their place

The stored/inferred Workspace Scope existed so `tk next` and `tk list` —
which took no scope argument — could be narrowed without restating an ID. But
the ID is always already present in how `tk` is actually driven:

- **Interactive prompts name the ID** (`implement tk-123`, `plan tk-10`), so
  the agent never needs to *discover* scope; it is handed the ID.
- **AFK / orchestrated grinds** are launched by a parent process that can put
  `TK_SCOPE=<epic-id>` in the environment every `tk` subprocess inherits, so
  the agent loops bare `tk next` with no per-call argument and the scope
  survives context compaction because it lives in the process, not the
  conversation.

Inferred Workspace Scope was doubly weak: feature branches are named after the
*Ticket* (`tk/tk-123-…`), so inference resolved to a single Ticket — exactly
the useless narrowing above — and it was invisible ambient state with no
off-switch, the property that makes hidden behavior hard to trust. A
deliberately set `tk.scope` in Worktree Config has the same hidden-state
shape: durable on disk, outliving the work, silently changing what `tk next`
considers. `TK_SCOPE` keeps the one workflow that mattered (the AFK epic
grind) while being ephemeral, inspectable (`echo $TK_SCOPE`), and trivially
cleared (`unset`, or a fresh shell).

Worktree *creation* needs no `tk` abstraction. `git worktree add` plus a
branch is two commands the harness already runs; `tk worktree start` was a
convenience whose main justification was enforcing the `tk/<id>-slug` naming
that fed inference. With inference gone, the Ticket Branch convention drops
from a contract to an optional nicety `tk` neither creates nor requires.

This is a pre-release design change, not a port deviation. `tk` has no users;
the Workspace Scope behavior inherited from the frozen Zig oracle
(ADR-0019) is being deleted rather than preserved, so the contract-preservation
principle of ADR-0018 does not bind here.

## Considered Options

- **Keep stored scope, set explicitly via a new `tk track <epic-id>`.**
  Rejected: an explicitly *set* persisted scope has the identical
  spooky-action-at-a-distance as inference — `tk next` behaves differently
  because of state set earlier and not visible in the command typed. The
  objection to inference was its invisibility and lack of an off-switch, which
  deliberate persistence does not cure.
- **Keep inference but resolve a Ticket branch up to its parent Epic.**
  Rejected: preserves the magic the change set out to remove and is
  surprising — naming one Ticket silently selects its siblings — and it still
  cannot be turned off.
- **Accept a Ticket as Scope with degenerate behavior** (`tk next <ticket>`
  returns that Ticket; `tk list <ticket>` shows just it). Rejected: silently
  reintroduces the empty narrowing this change exists to cut.
- **Name the concept `Filter`.** Rejected: it describes `tk list` but
  mis-describes `tk next`, whose Scope bounds Effective Priority propagation
  and thus changes which Ticket wins, not merely which rows are shown.

## Consequences

- `tk worktree` is removed entirely; `echo $TK_SCOPE` inspects the active
  Scope and `git worktree` manages checkouts. ADR-0007 is superseded.
- `tk next [<epic-id>]` and `tk list [<epic-id>]` gain an optional positional
  Epic argument; precedence is argument > `TK_SCOPE` > whole store. Both
  reject a Ticket argument with a typed error.
- `tk list` prints a hint when scoped so a filtered tree never reads as the
  full store.
- The Worktree Config `tk.scope` key, branch-name inference, and Workspace
  Scope Source are deleted; the `tk/<id>-slug` Ticket Branch convention is
  retained only as an optional human/agent nicety.
- Effective Priority still stops at the Scope boundary, now an Epic argument
  boundary rather than a stored Workspace Scope boundary.
- The positional Scope subsumes a proposed `tk list --parent <id>` filter
  (tk-55): in v1 the Parent Argument resolves only to an Epic, so "children of
  X" and "scope to Epic X" return identical rows, and one surface is enough.
  `--parent` stays the Parent Argument flag on `tk add`; `tk list` and
  `tk next` share the positional `<epic-id>`.
