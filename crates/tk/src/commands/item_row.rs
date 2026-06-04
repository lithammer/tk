//! Shared item-row rendering for `tk list` and `tk search`.
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
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::list::ListRow;

/// Render one row, prefixed by `tree_prefix` (empty for a flat layout, a
/// tree glyph for a nested List Tree child).
///
/// A `done` row never renders the blocked treatment (ADR-0025): closing an
/// item resolves none of its blockers, so a finished item can still carry an
/// unresolved blocker, but dimming it and printing `⊘` would read as nonsense.
/// `tk list` never feeds a `done` row here (every list view is open/active
/// only), so this gate changes only `tk search` output and keeps `tk list`
/// byte-identical.
pub(crate) fn render_row<W: Write + ?Sized>(
    stdout: &mut W,
    row: &ListRow,
    tree_prefix: &str,
    styler: SubStyler,
) -> std::io::Result<()> {
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
            let priority = row
                .priority
                .expect("schema CHECK guarantees Tickets carry a Priority");
            let p_style = palette::priority_style(priority);
            write!(stdout, " {} ", styler.wrap(p_style, "\u{25cf}"))?;
            write!(stdout, "{}", styler.wrap(p_style, priority.text()))?;
            if row.ticket_kind == Some(TicketKind::Bug) {
                write!(stdout, " {}", styler.wrap(palette::KIND_BUG, "[bug]"))?;
            }
            stdout.write_all(b" ")?;
            sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;
        }
        ItemClass::Epic => {
            write!(stdout, " {} ", styler.wrap(palette::KIND_EPIC, "[epic]"))?;
            sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;
        }
    }

    if show_blocked {
        write!(stdout, "{}", styler.close(palette::BLOCKED_ROW))?;
    }
    stdout.write_all(b"\n")
}

/// Render the summary chrome printed below a non-empty row set: a separator
/// line, the `Total: N items (…)` tally, and the status / blocked legend.
pub(crate) fn render_chrome<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
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
    )
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
