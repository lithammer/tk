//! Shared detail-header rendering for `tk show` and `tk grep`.
//!
//! Both commands open an Item with the same two lines — the label line
//! (`<status-glyph> <display-id> · <title>`) and the facet bar
//! (`<P_> · <Kind> · Created: …` for Tickets, `Epic · Created: …` for Epics) —
//! before diverging: `tk show` follows with body + relationship sections,
//! `tk grep` with the matching hunks. Keeping the header here is the single
//! source of truth so the two cannot drift (parallel to how `item_row` is
//! shared by `tk list` and `tk search`). ADR-0014 styling is preserved.

use std::io::Write;

use anstyle::Style;
use regex::Regex;

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::render::highlight;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;

/// Borrowed view of the fields the header renders, built by each command from
/// its own row type (`ItemDetail` for show, `GrepItem` for grep).
pub(crate) struct Header<'a> {
    pub status: ItemStatus,
    pub display_id: &'a str,
    pub item_class: ItemClass,
    pub title: &'a str,
    pub priority: Option<Priority>,
    pub ticket_kind: Option<TicketKind>,
    pub created_at: &'a str,
    pub updated_at: &'a str,
}

/// Render the label line and facet bar, terminated by a newline each.
///
/// `title_highlight` wraps matches of that regex in the title (`tk grep`'s
/// match cue); `None` renders the title plain (`tk show`), keeping show output
/// byte-identical. The bright-yellow MATCH colour nests inside the bold title
/// without disturbing it (ADR-0014 disjoint families: a colour close leaves
/// bold on).
///
/// The Updated facet is omitted when `updated_at == created_at` (the
/// at-insert default), so a never-edited Item shows only its creation date.
pub(crate) fn render_header<W: Write + ?Sized>(
    stdout: &mut W,
    header: &Header<'_>,
    title_highlight: Option<&Regex>,
    styler: SubStyler,
) -> std::io::Result<()> {
    // Label line: <status-glyph> <display-id> · <title>
    write!(
        stdout,
        "{} ",
        styler.wrap(status_style(header.status), header.status.glyph())
    )?;
    write!(
        stdout,
        "{} \u{b7} ",
        styler.wrap(id_style(header.item_class), header.display_id)
    )?;
    write!(stdout, "{}", styler.open(palette::HEADER))?;
    match title_highlight {
        Some(re) => highlight::write_highlighted_line(stdout, header.title, re, styler)?,
        None => sanitize::write_sanitized_line(stdout, header.title.as_bytes())?,
    }
    write!(stdout, "{}", styler.close(palette::HEADER))?;
    stdout.write_all(b"\n")?;

    // Facet bar.
    stdout.write_all(b"  ")?;
    match header.item_class {
        ItemClass::Epic => {
            write!(
                stdout,
                "{}",
                styler.wrap(palette::KIND_EPIC, ItemClass::Epic.label())
            )?;
        }
        ItemClass::Ticket => {
            let priority = header
                .priority
                .expect("Tickets always carry a Priority (schema CHECK)");
            write!(
                stdout,
                "{}",
                styler.wrap(priority_style(priority), priority.text())
            )?;
            stdout.write_all(b" \xc2\xb7 ")?; // " · "
            let kind = header
                .ticket_kind
                .expect("Tickets always carry a TicketKind (schema CHECK)");
            match kind {
                TicketKind::Bug => {
                    write!(stdout, "{}", styler.wrap(palette::KIND_BUG, kind.label()))?;
                }
                TicketKind::Task => stdout.write_all(kind.label().as_bytes())?,
            }
        }
    }
    let created_date = first_chars(header.created_at, 10);
    write!(stdout, " \u{b7} Created: {created_date}")?;
    if header.updated_at != header.created_at {
        let updated_date = first_chars(header.updated_at, 10);
        write!(stdout, " \u{b7} Updated: {updated_date}")?;
    }
    stdout.write_all(b"\n")
}

fn status_style(status: ItemStatus) -> Style {
    match status {
        ItemStatus::Open => palette::STATUS_OPEN,
        ItemStatus::Active => palette::STATUS_ACTIVE,
        ItemStatus::Done => palette::STATUS_DONE,
    }
}

fn priority_style(p: Priority) -> Style {
    match p {
        Priority::P0 => palette::PRIORITY_P0,
        Priority::P1 => palette::PRIORITY_P1,
        Priority::P2 => palette::PRIORITY_P2,
        Priority::P3 => palette::PRIORITY_P3,
        Priority::P4 => palette::PRIORITY_P4,
    }
}

fn id_style(class: ItemClass) -> Style {
    match class {
        ItemClass::Epic => palette::ID_EPIC,
        ItemClass::Ticket => palette::ID_TICKET,
    }
}

/// Truncate by char count (created_at / updated_at are ASCII ISO-8601 in
/// practice, so this takes the `YYYY-MM-DD` date prefix), surviving a future
/// multi-byte stamp.
fn first_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
