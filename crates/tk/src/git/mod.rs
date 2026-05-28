//! Git subprocess discovery façade.
//!
//! Mirrors `src/git/` in the Zig oracle: every git invocation in tk flows
//! through this module so commands don't reach for `Command::new("git")`
//! directly.

pub mod discovery;
