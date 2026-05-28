//! Per-command parsing and handlers. One module per subcommand.
//!
//! `resolver` is the shared open / resolve / diagnostic-rendering seam
//! used by every item command (ADR-0017).

pub mod add;
pub mod block;
pub mod done;
pub mod init;
pub mod lifecycle;
pub mod list;
pub mod manpage;
pub mod message;
pub mod next;
pub mod prime;
pub mod resolver;
pub mod self_update;
pub mod show;
pub mod start;
pub mod stop;
pub mod unblock;
pub mod update;
pub mod worktree;
