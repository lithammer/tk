//! Per-command parsing and handlers. One module per subcommand.
//!
//! `resolver` is the shared open / resolve / diagnostic-rendering seam
//! used by every item command (ADR-0017).

// Handlers take their parsed `Args` by value: it is a single-use parameter
// object clap built for exactly this call, moved to its one consumer rather
// than a value the caller keeps. `needless_pass_by_value` (pedantic) can't
// model that sink ownership, so it is allowed for this module; it stays active
// crate-wide to catch genuine owned-by-value (`String`/`Vec`) params elsewhere.
#![allow(clippy::needless_pass_by_value)]

pub mod accept;
pub mod add;
pub mod block;
pub mod done;
pub mod grep;
pub mod init;
pub mod item_header;
pub mod item_row;
pub mod lifecycle;
pub mod list;
pub mod manpage;
pub mod message;
pub mod next;
pub mod prime;
pub mod promote;
pub mod resolver;
pub mod scope;
pub mod search;
pub mod self_update;
pub mod show;
pub mod start;
pub mod stop;
pub mod sync;
pub mod unblock;
pub mod update;
