//! Palette: named semantic Styles the rest of tk reaches for.
//!
//! One file owns every colour decision so retheming or auditing is a
//! single diff. Entries set to `style.none()` are intentional placeholders
//! whose colour choice has been deferred (the consumer that wires them in
//! may keep them uncoloured or pick a colour then). The Beads-mimicking
//! initial palette and its rationale live in ADR 0014.
//!
//! Nesting constraint: when a style appears as an outer span (`open`/
//! `close` bracketing several writes) and another wraps a span inside it,
//! the two must touch disjoint SGR families (foreground colour vs.
//! bold/dim vs. underline vs. background). The inner span's close resets
//! its family to default but does not restore a previously-set outer
//! value. The initial entries here are constraint-safe: foreground-colour
//! families and bold/dim families do not overlap.

const style = @import("style.zig");

pub const header = style.bold();

pub const id_epic = style.none();
pub const id_ticket = style.none();

pub const kind_bug = style.red();
pub const kind_epic = style.magenta();

pub const status_open = style.none();
pub const status_active = style.yellow();
pub const status_done = style.green();

pub const blocked = style.none();
pub const blocked_row = style.dim();
pub const separator = style.dim();

pub const priority_p0 = style.red();
pub const priority_p1 = style.yellow();
pub const priority_p2 = style.none();
pub const priority_p3 = style.none();
pub const priority_p4 = style.none();
