# Keep the repository store untracked by default

tk uses an untracked **Repository Store** for local state instead of checking **Ticket** state into git. This diverges from repository-tracked state, where ticket state is committed into git, to avoid merge conflicts, accidental leakage of local triage notes, and noisy commits, while leaving portability to backend sync or explicit import/export.
