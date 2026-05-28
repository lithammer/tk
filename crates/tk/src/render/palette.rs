//! Named semantic [`Style`] constants the rest of `tk` reaches for.
//!
//! One file owns every colour decision so retheming or auditing is a
//! single diff. Entries set to `Style::new()` are intentional placeholders
//! whose colour choice has been deferred (the consumer that wires them in
//! may keep them uncoloured or pick a colour then). Initial choices and
//! rationale live in ADR-0014.
//!
//! ## Nesting constraint (ADR-0014)
//!
//! When an entry appears as an outer span (`open` / `close` bracketing
//! several writes) and another wraps a span inside it, the two must touch
//! disjoint SGR families (foreground colour vs. bold/dim vs. underline vs.
//! background). The inner span's close resets its family to default but
//! does *not* restore a previously-set outer value. The initial entries
//! here are constraint-safe: foreground-colour families and bold/dim
//! families do not overlap.
//!
//! The crate-level `Styler` honours the invariant by hand-deriving each
//! palette entry's close from the [`Style`] value rather than reaching
//! for [`anstyle::Reset`] (the universal `\x1b[0m`, which would clobber
//! the outer span).

use anstyle::{AnsiColor, Color, Style};

const fn fg(color: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(color)))
}

/// Bold heading text used for section labels in `tk show` and `tk list`.
pub const HEADER: Style = Style::new().bold();

/// Display ID column for Epics. Currently unstyled — the colour choice
/// for Epics in lists has been deferred until the list rendering lands.
pub const ID_EPIC: Style = Style::new();

/// Display ID column for Tickets. Currently unstyled; see [`ID_EPIC`].
pub const ID_TICKET: Style = Style::new();

/// Bug-Ticket-kind badge in lists / detail views.
pub const KIND_BUG: Style = fg(AnsiColor::Red);

/// Epic-Ticket-kind badge in lists / detail views.
pub const KIND_EPIC: Style = fg(AnsiColor::Magenta);

/// Open Item status (placeholder — uncoloured).
pub const STATUS_OPEN: Style = Style::new();

/// Active Item status — items currently being worked.
pub const STATUS_ACTIVE: Style = fg(AnsiColor::Yellow);

/// Done Item status — terminal state per ADR-0006.
pub const STATUS_DONE: Style = fg(AnsiColor::Green);

/// Blocked marker beside an Item Display ID (placeholder).
pub const BLOCKED: Style = Style::new();

/// Outer-row dim for an Item whose Dependencies are not yet satisfied.
/// Pairs with `BLOCKED_ROW`'s family-disjoint inner spans.
pub const BLOCKED_ROW: Style = Style::new().dimmed();

/// Dim separator (e.g. trees, list dividers).
pub const SEPARATOR: Style = Style::new().dimmed();

/// Priority P0 — highest. Mirrors `KIND_BUG`'s SGR so urgent rows draw the
/// eye through colour rather than weight.
pub const PRIORITY_P0: Style = fg(AnsiColor::Red);

/// Priority P1.
pub const PRIORITY_P1: Style = fg(AnsiColor::Yellow);

/// Priority P2 (placeholder — uncoloured).
pub const PRIORITY_P2: Style = Style::new();

/// Priority P3 (placeholder — uncoloured).
pub const PRIORITY_P3: Style = Style::new();

/// Priority P4 — lowest (placeholder — uncoloured).
pub const PRIORITY_P4: Style = Style::new();
