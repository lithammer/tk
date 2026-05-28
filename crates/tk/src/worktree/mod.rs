//! Workspace Scope discovery and worktree management.
//!
//! See ARCHITECTURE.md ("Workspace Scope and Worktrees") for the
//! configured-vs-inferred ordering: a `tk.scope` git-config value takes
//! precedence; absent that, a `tk/<display-id>[-…]` branch name infers
//! one via [`scope::resolve_against_store`]. Slugs and worktree-set/start
//! orchestration land alongside the worktree commands.

pub mod scope;
