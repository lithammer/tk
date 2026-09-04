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
use crate::domain::mutation_state::MutationState;
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

/// Muted badge for a non-default Selection State (`[triage]` and `[parked]`).
/// Held/unaccepted work is marked without drawing the eye (ADR-0027).
/// A bright-black foreground, not `dimmed()`: the badge renders inside the
/// dimmed `BLOCKED_ROW` span on a blocked row, so it must touch a disjoint SGR
/// family — its close (`39`) must not reset the outer dim (ADR-0014 nesting).
pub const SELECTION_BADGE: Style = fg(AnsiColor::BrightBlack);

/// A Mutation the Backend failed to apply, rendered as `⚑` beside a list
/// row's title. Bright red, not the red `KIND_BUG` / `PRIORITY_P0` already
/// use — sharing their SGR would let the glyph read as one of those badges,
/// and `⚑`, `[bug]`, and `● P0` can all appear on the same row. A foreground
/// colour, not `dimmed()`: it renders inside the dimmed `BLOCKED_ROW` span on
/// a blocked row, so its close (`39`) must not reset the outer dim (ADR-0014
/// nesting).
pub const MUTATION_FAILED: Style = fg(AnsiColor::BrightRed);

/// A Mutation still queued for the Backend, rendered as `~` beside a list
/// row's title. Bright black, the same SGR `SELECTION_BADGE` uses — one
/// muted class, one SGR, as this palette already pairs Red across
/// `KIND_BUG` / `PRIORITY_P0` and Yellow across `PRIORITY_P1` /
/// `STATUS_ACTIVE` — kept as its own constant so a recolour is one edit. A
/// foreground colour, not `dimmed()`, so its close (`39`) cannot reset the
/// dimmed `BLOCKED_ROW` span it renders inside (ADR-0014 nesting).
pub const MUTATION_PENDING: Style = fg(AnsiColor::BrightBlack);

/// A Mutation whose Backend creation began with no confirmed identity or
/// no-effect verdict — the `applying` state, rendered as a word by
/// `tk sync log`, `tk show`'s Mutation sections, and the `tk list` banner.
/// Yellow, the SGR `STATUS_ACTIVE` uses: both mean work in flight, and this
/// palette already pairs one SGR across a semantic class. Deliberately not
/// `MUTATION_FAILED`'s red, which says the Backend refused the Mutation —
/// false of an Indeterminate creation, where tk never learned what happened
/// (ADR-0039).
pub const MUTATION_APPLYING: Style = fg(AnsiColor::Yellow);

/// An Abandoned Mutation — a Promotion withdrawn before tk recorded a
/// Backend identity, so a Backend object may exist that tk cannot address.
/// Magenta: it is the one Withdrawn Mutation that still asks something of
/// the reader, and CONTEXT.md singles it out for exactly that reason. Not
/// red, for the same reason [`MUTATION_APPLYING`] is not — nothing was
/// refused here. Nor the bright black [`MUTATION_SKIPPED`] and
/// [`MUTATION_CANCELLED`] carry, which would file it with the withdrawn
/// outcomes that ask nothing of anyone.
pub const MUTATION_ABANDONED: Style = fg(AnsiColor::Magenta);

/// A Skipped Mutation — Backend intent relinquished by Sync Skip. Bright
/// black: the outcome is resolved and asks nothing of the reader. One entry
/// per state rather than one for the Withdrawn Mutations as a group, because
/// an Abandoned Mutation is one of those too and is the exception that
/// [`MUTATION_ABANDONED`] exists for.
///
/// A foreground colour, not `dimmed()`, so its close (`39`) cannot reset a
/// `dimmed()` outer span it renders inside (ADR-0014 nesting).
pub const MUTATION_SKIPPED: Style = fg(AnsiColor::BrightBlack);

/// A Cancelled Mutation — unapplied Backend intent withdrawn by Promotion
/// Cancellation, Detach, or a Store migration. Bright black for the same
/// reason [`MUTATION_SKIPPED`] is, and kept a separate constant on this
/// palette's standing convention: one SGR may serve several semantic names,
/// so recolouring a relinquished outcome stays one edit and cannot drag a
/// withdrawn one along with it.
pub const MUTATION_CANCELLED: Style = fg(AnsiColor::BrightBlack);

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

/// Dim chrome: list dividers, summary parentheticals, and the pointers that
/// send a reader to another command.
///
/// Not tree glyphs, despite the name. `tk list` writes its own `└── ` row
/// prefix plain, and `tk sync log`'s `└─` failure continuation matches it, so
/// dimming either would leave one styled tree glyph in a codebase whose
/// others are plain.
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
/// anchor and the bright-yellow `MATCH` highlight; not cyan, which the Display
/// ID anchor needs more than the separator does.
pub const HUNK_SEPARATOR: Style = fg(AnsiColor::Blue);

// Domain-enum → palette `Style` mappers. The single source of truth for these
// mappings, shared by every renderer (`item_row` for list/search, `item_header`
// for show/grep, show's relationship sub-rows, and the Mutation surfaces —
// `tk sync log`, show's Mutation sections, and the list banner) so a recolour
// is one edit.

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

/// Style for a Mutation's state token.
///
/// Every surface that names a Mutation state renders through here, so the
/// same Mutation cannot read as one thing in `tk sync log` and another in
/// `tk show` or the `tk list` banner. Matched exhaustively: a state added to
/// [`MutationState`] fails to compile here rather than defaulting into
/// whichever arm a predicate happened to pick.
///
/// `applied` borrows [`STATUS_DONE`] rather than owning an entry. It is the
/// one state no list view renders — `tk sync log`'s default filter excludes
/// it, no flag selects it, and `tk show` drops it — so it reaches a reader
/// only through `tk sync log <sequence>`, where it means the same thing
/// `STATUS_DONE` does on an Item.
///
/// [`MutationState`]: crate::domain::mutation_state::MutationState
#[must_use]
pub fn mutation_state_style(state: MutationState) -> Style {
    match state {
        MutationState::Pending => MUTATION_PENDING,
        MutationState::Failed => MUTATION_FAILED,
        MutationState::Applying => MUTATION_APPLYING,
        MutationState::Skipped => MUTATION_SKIPPED,
        MutationState::Cancelled => MUTATION_CANCELLED,
        MutationState::Abandoned => MUTATION_ABANDONED,
        MutationState::Applied => STATUS_DONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::mutation_state::MutationState;

    /// Pins every Mutation state to its palette entry. This table is the
    /// contract `tk sync log`, `tk show`'s Mutation sections, and the
    /// `tk list` banner all render through, so a state that would look one
    /// way on one surface and another way elsewhere fails here first.
    #[test]
    fn mutation_state_style_maps_every_state() {
        let expected = [
            (MutationState::Pending, MUTATION_PENDING),
            (MutationState::Failed, MUTATION_FAILED),
            (MutationState::Applying, MUTATION_APPLYING),
            (MutationState::Skipped, MUTATION_SKIPPED),
            (MutationState::Cancelled, MUTATION_CANCELLED),
            (MutationState::Abandoned, MUTATION_ABANDONED),
            (MutationState::Applied, STATUS_DONE),
        ];

        // Drive the assertions from `ALL` rather than from the table, so a
        // state the table forgets fails here instead of going untested. The
        // length check then catches the other direction: a duplicated row
        // that would otherwise hide a missing one.
        assert_eq!(
            expected.len(),
            MutationState::ALL.len(),
            "every MutationState needs exactly one row in this table"
        );

        for state in MutationState::ALL {
            let Some((_, want)) = expected.iter().find(|(candidate, _)| *candidate == state) else {
                panic!("no row in this table for {state}");
            };
            assert_eq!(
                mutation_state_style(state),
                *want,
                "wrong palette entry for {state}"
            );
        }
    }
}
