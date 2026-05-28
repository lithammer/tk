//! Per-command parsing and handlers. One module per subcommand.
//!
//! `resolver` is the shared open / resolve / diagnostic-rendering seam
//! used by every item command (ADR-0017).

pub mod add;
pub mod init;
pub mod list;
pub mod message;
pub mod next;
pub mod resolver;
pub mod show;
pub mod update;
