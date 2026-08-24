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
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::list::ListRow;

/// The muted list badge for a non-default Selection State, or `None` for a row
/// that carries none. Rendering owns the badge token (ADR-0027); the domain
/// enum owns only the storage spelling. `accepted` is the default and stays
/// unbadged; `triage` and `parked` each get a cue.
fn selection_badge(selection_state: Option<SelectionState>) -> Option<&'static str> {
    match selection_state {
        Some(SelectionState::Triage) => Some("[triage]"),
        Some(SelectionState::Parked) => Some("[parked]"),
        Some(SelectionState::Accepted) | None => None,
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
            // A triage Ticket carries no Priority (ADR-0027); omit the `● P_`
            // marker. The `[bug]` marker still renders.
            if let Some(priority) = row.priority {
                let p_style = palette::priority_style(priority);
                write!(stdout, " {} ", styler.wrap(p_style, "\u{25cf}"))?;
                write!(stdout, "{}", styler.wrap(p_style, priority.text()))?;
            }
            if row.ticket_kind == Some(TicketKind::Bug) {
                write!(stdout, " {}", styler.wrap(palette::KIND_BUG, "[bug]"))?;
            }
            if let Some(badge) = selection_badge(row.selection_state) {
                write!(stdout, " {}", styler.wrap(palette::SELECTION_BADGE, badge))?;
            }
        }
        ItemClass::Epic => {
            write!(stdout, " {}", styler.wrap(palette::KIND_EPIC, "[epic]"))?;
        }
    }

    // Mutation markers sit immediately before the title in both arms above:
    // the Ticket arm's selection badge is not an anchor the Epic arm has
    // (Selection State is Ticket-only, ADR-0027), and a Backend Epic can
    // carry its own Mutation (`update_epic`, `set_item_status`,
    // `add_ticket_to_epic`). Failed leads pending — the actionable marker
    // comes first — and both render on a `done` row (ADR-0040): a `done`
    // Backend Item with a queued Mutation is exactly the case where the
    // Backend does not yet agree the Item is done.
    if row.has_failed_mutation {
        write!(
            stdout,
            " {}",
            styler.wrap(palette::MUTATION_FAILED, "\u{2691}")
        )?;
    }
    if row.has_pending_mutation {
        write!(stdout, " {}", styler.wrap(palette::MUTATION_PENDING, "~"))?;
    }
    stdout.write_all(b" ")?;
    sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;

    if show_blocked {
        write!(stdout, "{}", styler.close(palette::BLOCKED_ROW))?;
    }
    stdout.write_all(b"\n")
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
/// row — stays byte-identical.
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
    )?;

    render_mutation_legend(stdout, rows, styler)
}

/// The `Mutations:` legend, or nothing when no row in `rows` carries a
/// pending or failed Mutation.
///
/// Folding the two flags directly over `rows` is correct only under an
/// invariant the callers hold, not one this function can check: every row
/// in `rows` is rendered exactly once. `render` emits roots and
/// `render_children` emits the rest, and a row whose parent is absent from
/// `rows` (a `done` Epic excluded from the default view, for instance)
/// falls through and renders at top level rather than being skipped. If a
/// future change starts suppressing a marker at render time — the way
/// `render_row`'s `show_blocked` gates `⊘` on a `done` row — this fold
/// would name a glyph that never actually appeared on screen, and would
/// have to move to reporting what `render_row` actually emitted instead of
/// folding the raw flags.
fn render_mutation_legend<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    styler: SubStyler,
) -> std::io::Result<()> {
    let failed = rows.iter().any(|row| row.has_failed_mutation);
    let pending = rows.iter().any(|row| row.has_pending_mutation);
    if !failed && !pending {
        return Ok(());
    }
    write!(stdout, "Mutations: ")?;
    if failed {
        write!(
            stdout,
            "{} failed",
            styler.wrap(palette::MUTATION_FAILED, "\u{2691}")
        )?;
    }
    if failed && pending {
        write!(stdout, "  ")?;
    }
    if pending {
        write!(
            stdout,
            "{} pending",
            styler.wrap(palette::MUTATION_PENDING, "~")
        )?;
    }
    writeln!(stdout)
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
