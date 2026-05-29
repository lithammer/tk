# Prime prints only when a Repository Store is initialized

`tk prime` prints its embedded briefing only when the current directory has
an openable Repository Store; with no openable store it exits 0 with empty
stdout and empty stderr. This inverts the original contract — Prime began as
"no Repository Store precondition; safe for hooks before `tk init`" — because
the agent hook that runs Prime moved from a single repo to a global
`SessionStart` / `PreCompact` hook that fires in every directory. The
hook-safe goal is unchanged; only the mechanism flips, from always-print to
silence outside an initialized repo.

## Considered Options

Detection reuses the existing `open_existing` seam and treats **every**
`OpenError` as silent — no store, outside a git repository, git missing, a
foreign database, a future-version store, or a SQLite fault all take the
empty-success path. Keeping genuine faults loud was rejected: Prime is not the
diagnostic surface, every other `tk` command opens the store and reports
corruption or a future-version store the moment it runs, and a global hook that
leaks stderr or a non-zero exit into every session is the exact noise this
change exists to remove. A lighter `git rev-parse` + file-exists probe that
skips `open_existing` was rejected because it would print the briefing for a
foreign or future-version store it cannot actually use.

## Consequences

The briefing-formatting contract (single trailing newline, header prefix, no CR
bytes) is now tested against a pure helper rather than by driving `run()`, since
`run()` requires an openable store. Print-versus-silent behaviour is pinned by
the scenario harness across initialized, git-without-init, and outside-git
cases.
