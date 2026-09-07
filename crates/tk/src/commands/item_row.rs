//! Shared item-row rendering for `tk list` and `tk search`, with Ticket
//! markers also used by the dedicated Plan view.
//!
//! Both commands render the same compact, unaligned row — status glyph,
//! Display ID, optional blocked indicator, priority/kind markers, title —
//! and the same summary chrome (separator, totals, status/blocked legend).
//! `tk list` walks a List Tree and passes a tree prefix per row; `tk search`
//! lays its matches out flat with an empty prefix (ADR-0025). Keeping the
//! row and chrome here means a single source of truth, so list output can
//! never drift from search output.

use std::io::Write;

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::list::ListRow;

/// The shared badge for a non-default Selection State, or `None` for a row
/// that carries none. Rendering owns the badge token (ADR-0027); the domain
/// enum owns only the storage spelling. `accepted` is the default and stays
/// unbadged; `triage` and `parked` each get a cue.
pub(crate) fn selection_badge(selection_state: Option<SelectionState>) -> Option<&'static str> {
    match selection_state {
        Some(SelectionState::Triage) => Some("[triage]"),
        Some(SelectionState::Parked) => Some("[parked]"),
        Some(SelectionState::Accepted) | None => None,
    }
}

/// Shared Priority and Bug markers for List, Search and Plan rows.
/// Triage omits Priority but keeps the Bug marker (ADR-0027).
pub(crate) fn render_ticket_markers<W: Write + ?Sized>(
    out: &mut W,
    priority: Option<Priority>,
    kind: Option<TicketKind>,
    styler: SubStyler,
) -> std::io::Result<()> {
    if let Some(priority) = priority {
        let style = palette::priority_style(priority);
        write!(
            out,
            " {} {}",
            styler.wrap(style, "●"),
            styler.wrap(style, priority.text())
        )?;
    }
    if kind == Some(TicketKind::Bug) {
        write!(out, " {}", styler.wrap(palette::KIND_BUG, "[bug]"))?;
    }
    Ok(())
}

/// Pending Promotion label before a compact Item row's title (ADR-0041).
pub(crate) fn render_pending_promotion<W: Write + ?Sized>(
    out: &mut W,
    has_pending_promotion: bool,
) -> std::io::Result<()> {
    if has_pending_promotion {
        out.write_all(b"[pending promotion] ")?;
    }
    Ok(())
}

/// Which Mutation marker glyphs [`render_row`] actually put on a row.
///
/// Accumulated across the rendered rows and handed to [`render_chrome`], so
/// the legend explains the glyphs that reached the screen rather than the
/// flags they were derived from. Those two can part company: `render_row`
/// already suppresses `⊘` on a `done` row despite the flag being set.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MutationMarkers {
    failed: bool,
    pending: bool,
}

impl MutationMarkers {
    /// Union, for accumulating across rows.
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            failed: self.failed || other.failed,
            pending: self.pending || other.pending,
        }
    }
}

/// Render one row, prefixed by `tree_prefix` (empty for a flat layout, a
/// tree glyph for a nested List Tree child).
///
/// A `done` row never renders the blocked treatment (ADR-0025): closing an
/// item resolves none of its blockers, so a finished item can still carry an
/// unresolved blocker, but dimming it and printing `⊘` would read as nonsense.
/// `tk list` can feed a `done` row here too: `LIST_ROWS_SQL`'s
/// epic-parent-inclusion branch carries no status predicate on the parent, so
/// a `done` Epic with a matching child already reaches this gate through
/// `--ready` / `--blocked` / `--active` / `--triage` / `--parked`. Whether
/// that Epic should surface at all is tk-163's to decide; this gate's
/// behaviour once it does is unaffected either way.
pub(crate) fn render_row<W: Write + ?Sized>(
    stdout: &mut W,
    row: &ListRow,
    tree_prefix: &str,
    styler: SubStyler,
) -> std::io::Result<MutationMarkers> {
    stdout.write_all(tree_prefix.as_bytes())?;

    let show_blocked = row.has_unresolved_blocker && row.status != ItemStatus::Done;

    if show_blocked {
        write!(stdout, "{}", styler.open(palette::BLOCKED_ROW))?;
    }

    write!(
        stdout,
        "{} ",
        styler.wrap(palette::status_style(row.status), row.status.glyph())
    )?;
    write!(
        stdout,
        "{}",
        styler.wrap(palette::id_style(row.item_class), &row.display_id)
    )?;

    if show_blocked {
        write!(stdout, " {}", styler.wrap(palette::BLOCKED, "\u{2298}"))?;
    }

    match row.item_class {
        ItemClass::Ticket => {
            render_ticket_markers(stdout, row.priority, row.ticket_kind, styler)?;
            if let Some(badge) = selection_badge(row.selection_state) {
                write!(stdout, " {}", styler.wrap(palette::SELECTION_BADGE, badge))?;
            }
        }
        ItemClass::Epic => {
            write!(stdout, " {}", styler.wrap(palette::KIND_EPIC, "[epic]"))?;
        }
    }

    // Failed precedes pending; both remain visible on done rows (ADR-0040).
    // Pending Promotion has its own label (ADR-0041).
    let mut markers = MutationMarkers::default();
    if row.has_failed_mutation {
        write!(
            stdout,
            " {}",
            styler.wrap(palette::MUTATION_FAILED, "\u{2691}")
        )?;
        markers.failed = true;
    }
    if row.has_pending_mutation {
        write!(stdout, " {}", styler.wrap(palette::MUTATION_PENDING, "~"))?;
        markers.pending = true;
    }
    stdout.write_all(b" ")?;
    render_pending_promotion(stdout, row.has_pending_promotion)?;
    sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;

    if show_blocked {
        write!(stdout, "{}", styler.close(palette::BLOCKED_ROW))?;
    }
    stdout.write_all(b"\n")?;
    Ok(markers)
}

/// Render the summary chrome printed below a non-empty row set: a separator
/// line, the `Total: N items (…)` tally, the status / blocked legend, and —
/// only when at least one row carries one — a `Mutations:` legend naming the
/// marker glyphs present.
///
/// The `Mutations:` line is conditional, unlike `Status:` / `Blocked:`,
/// which print unconditionally even when no row matches (`Blocked: ⊘
/// blocked` prints on a store with nothing blocked). An always-on legend for
/// a condition most stores never hit would be noise, and the omission is why
/// every pre-existing scenario snapshot — none of which carries a marked
/// row — stays byte-identical. `markers` is what the rows actually rendered;
/// see [`MutationMarkers`].
pub(crate) fn render_chrome<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    markers: MutationMarkers,
    styler: SubStyler,
) -> std::io::Result<()> {
    let counts = StatusCounts::tally(rows);

    writeln!(
        stdout,
        "{}",
        styler.wrap(
            palette::SEPARATOR,
            "--------------------------------------------------------------------------------"
        )
    )?;

    render_total(stdout, rows.len(), counts)?;
    stdout.write_all(b"\n")?;

    write!(stdout, "Status: ")?;
    write!(
        stdout,
        "{} open  ",
        styler.wrap(palette::STATUS_OPEN, ItemStatus::Open.glyph())
    )?;
    write!(
        stdout,
        "{} active  ",
        styler.wrap(palette::STATUS_ACTIVE, ItemStatus::Active.glyph())
    )?;
    writeln!(
        stdout,
        "{} done",
        styler.wrap(palette::STATUS_DONE, ItemStatus::Done.glyph())
    )?;
    writeln!(
        stdout,
        "Blocked: {} blocked",
        styler.wrap(palette::BLOCKED, "\u{2298}")
    )?;

    render_mutation_legend(stdout, markers, styler)
}

/// The `Mutations:` legend, or nothing when no marker was rendered.
///
/// Takes what [`render_row`] emitted rather than re-reading the rows, so the
/// legend cannot name a glyph that never reached the screen — a row's flags
/// and the marker drawn from them can diverge, the way `show_blocked` already
/// suppresses `⊘` on a `done` row.
fn render_mutation_legend<W: Write + ?Sized>(
    stdout: &mut W,
    markers: MutationMarkers,
    styler: SubStyler,
) -> std::io::Result<()> {
    let mut entries = Vec::new();
    if markers.failed {
        entries.push(format!(
            "{} failed",
            styler.wrap(palette::MUTATION_FAILED, "\u{2691}")
        ));
    }
    if markers.pending {
        entries.push(format!(
            "{} pending",
            styler.wrap(palette::MUTATION_PENDING, "~")
        ));
    }
    if entries.is_empty() {
        return Ok(());
    }
    writeln!(stdout, "Mutations: {}", entries.join("  "))
}

fn render_total<W: Write + ?Sized>(
    stdout: &mut W,
    total: usize,
    counts: StatusCounts,
) -> std::io::Result<()> {
    let noun = if total == 1 { "item" } else { "items" };
    write!(stdout, "Total: {total} {noun} (")?;
    let mut wrote = false;
    write_count(stdout, &mut wrote, counts.open, "open")?;
    write_count(stdout, &mut wrote, counts.active, "active")?;
    write_count(stdout, &mut wrote, counts.done, "done")?;
    writeln!(stdout, ")")
}

fn write_count<W: Write + ?Sized>(
    stdout: &mut W,
    wrote: &mut bool,
    count: usize,
    label: &str,
) -> std::io::Result<()> {
    if count == 0 {
        return Ok(());
    }
    if *wrote {
        write!(stdout, ", ")?;
    }
    write!(stdout, "{count} {label}")?;
    *wrote = true;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct StatusCounts {
    open: usize,
    active: usize,
    done: usize,
}

impl StatusCounts {
    fn tally(rows: &[ListRow]) -> Self {
        let mut counts = Self::default();
        for row in rows {
            match row.status {
                ItemStatus::Open => counts.open += 1,
                ItemStatus::Active => counts.active += 1,
                ItemStatus::Done => counts.done += 1,
            }
        }
        counts
    }
}
