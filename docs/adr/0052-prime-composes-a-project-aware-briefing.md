# Prime composes a project-aware briefing

`tk prime` reports current Repository Store state before a fixed, task-based
workflow handbook. It selects handbook sections whose prerequisites exist so,
for example, a Store without a Remote gets no Remote workflow. This replaces
the static handbook, whose unconditional sections could not stay relevant to
each project.

## Current work

Prime reads one Store snapshot and renders Scope, the next Ticket and its
copyable `tk start` command, active Items, the whole Plan, then Mutation Log
state. It states empty and clean results instead of making omission ambiguous.
It uses the compact Item-row and state language shared with the related read
commands, while owning its own section structure.

A valid inherited `TK_SCOPE` limits active Items and next-Ticket selection to
the scoped Epic and its child Tickets. Prime names the Scope before those
facts. It still shows the whole Plan. With a populated Plan, next-Ticket
selection follows `tk next --plan`, including its intersection with Scope, and
never falls back to unplanned work. With an empty Plan, selection follows
ordinary `tk next`. An invalid inherited Scope produces a warning and no
scoped current-work claims.

Prime renders every active Item inside the current-work boundary and every
Plan member, including done Tickets. It does not cap either view.

Active Items describe the Store, not work owned by the current agent or
Workspace. Prime does not add Work ownership, task claiming, or Git worktree
orientation.

When a Remote is configured, Prime reports counts for every non-applied
Mutation state. The counts are context only: Prime does not tell the agent to
inspect or resolve them. Recovery remains with `tk sync log`, the dedicated
recovery commands, and their help.

## Workflow handbook

The handbook keeps one order and describes work as an agent performs it:
starting, capturing and updating, blocking, working the Plan, working in a
Scope, finishing, and Remote work when a Remote is configured. State changes
the immediate guidance, but does not hide an available core workflow.

Each section lists commands with short inline descriptions. The handbook
explains what commands do rather than prescribing agent habits or repeating
selection rules. The `tk start <id>` entry marks chosen work active before
starting. Detailed semantics belong in command help and the full reference.

Every briefing says it is contextual rather than complete and points to
`tk --help`, `tk <command> --help`, and `man tk` for the full command
reference.

## Failure and output

ADR-0020 still governs Store discovery: no openable Store means exit 0 with
empty stdout and stderr, which keeps global agent hooks quiet outside tk
repositories. Once the Store opens, a later read failure is a command error.
Prime builds the whole briefing before writing stdout, so that failure writes
only a diagnostic to stderr and exits nonzero instead of printing a partial or
context-free briefing.

Successful output remains on stdout, ends in exactly one newline, and contains
no carriage returns.

## Considered options

A fully variable document was rejected because agents would have to relearn
where facts live for each Store shape. Pruning commands whenever no current
Item uses them was rejected because it would hide workflows needed to create
that state. State-driven recovery advice was rejected because a session-start
hook should give context, not divert the agent from the user's request.
