# Keep the repository store untracked by default

tk keeps local state in an untracked **Repository Store** instead of committing
**Ticket** state to git. This avoids merge conflicts, keeps local triage notes
out of commits, and cuts commit noise. Portability, when needed, must come from
**Backend** sync or an explicit import and export path.
