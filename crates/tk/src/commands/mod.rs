//! Per-command parsing and handlers. One module per subcommand.
//!
//! `resolver` is the shared open / resolve / diagnostic-rendering seam
//! used by every item command (ADR-0017).

pub mod init;
pub mod resolver;
pub mod show;
