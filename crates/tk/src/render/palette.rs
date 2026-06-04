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

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;

const fn fg(color: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(color)))
}

/// Bold heading text used for section labels in `tk show` and `tk list`.
pub const HEADER: Style = Style::new().bold();

/// Display ID for Epics. Cyan — the Display ID is the per-item anchor that
/// lets a reader pick one item out of a wall of text (`tk grep`, ADR-0026);
/// cyan is the palette's most eye-catching free colour. Shared with the
/// `item_header` (show/grep) and the list rows.
pub const ID_EPIC: Style = fg(AnsiColor::Cyan);

/// Display ID for Tickets. Cyan anchor; see [`ID_EPIC`].
pub const ID_TICKET: Style = fg(AnsiColor::Cyan);

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

/// `tk grep` matched-text highlight (ADR-0026). Bright yellow — the one vivid
/// colour the palette does not otherwise spend, so a highlighted word can never
/// be mistaken for a `KIND_BUG` / `PRIORITY_P0` badge (red), a `PRIORITY_P1` /
/// `STATUS_ACTIVE` marker (normal yellow), an Epic (magenta), or a Display ID
/// (cyan). It is a disjoint SGR family from the bold title, so a match inside
/// the title closes (`39`) without disturbing the outer bold (ADR-0014).
/// Because grep matches per line and closes the span before every newline, with
/// the indent written plain before it opens, the colour never bleeds across
/// lines or tints the indent.
pub const MATCH: Style = fg(AnsiColor::BrightYellow);

/// `tk grep` `--` separator between non-contiguous hunks (ADR-0026). Blue —
/// structural chrome that should read as secondary to the cyan Display ID
/// anchor and the red matches; cyan was reassigned to the Display ID because it
/// pops more and the anchor needs it more than the separator does.
pub const HUNK_SEPARATOR: Style = fg(AnsiColor::Blue);

// Domain-enum → palette `Style` mappers. The single source of truth for these
// mappings, shared by every renderer (`item_row` for list/search, `item_header`
// for show/grep, and show's relationship sub-rows) so a recolour is one edit.

/// Style for an Item's status glyph.
#[must_use]
pub fn status_style(status: ItemStatus) -> Style {
    match status {
        ItemStatus::Open => STATUS_OPEN,
        ItemStatus::Active => STATUS_ACTIVE,
        ItemStatus::Done => STATUS_DONE,
    }
}

/// Style for a Ticket's Priority marker.
#[must_use]
pub fn priority_style(priority: Priority) -> Style {
    match priority {
        Priority::P0 => PRIORITY_P0,
        Priority::P1 => PRIORITY_P1,
        Priority::P2 => PRIORITY_P2,
        Priority::P3 => PRIORITY_P3,
        Priority::P4 => PRIORITY_P4,
    }
}

/// Style for an Item's Display ID, by class.
#[must_use]
pub fn id_style(class: ItemClass) -> Style {
    match class {
        ItemClass::Epic => ID_EPIC,
        ItemClass::Ticket => ID_TICKET,
    }
}
