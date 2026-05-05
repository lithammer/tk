# Keep the repository store untracked by default

Ticket uses an untracked **Repository Store** for local state instead of checking ticket state into git. This diverges from Beads-style repository-tracked state to avoid merge conflicts, accidental leakage of local triage notes, and noisy commits, while leaving portability to backend sync or explicit import/export.
