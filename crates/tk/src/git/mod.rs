//! Git subprocess discovery façade. Every git invocation in tk flows through
//! this module so commands don't reach for `Command::new("git")` directly.

pub mod discovery;
