//! `tk grep` content-search scan (ADR-0026).
//!
//! Unlike `tk search`, grep does no SQL-side content filter: it matches a
//! regular expression per line in Rust and must materialise each matched
//! body anyway to build `grep -C`-style context. So the store streams every
//! Item's current state in creation order, one row at a time, and the command
//! renders each match straight to stdout — peak memory is one Item regardless
//! of store size. Matching in SQL would buy nothing and require the rusqlite
//! `functions` feature; streaming keeps `tk grep PATTERN | head` cheap.

use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;

use super::Store;

/// One Item's current state, carrying the title + body grep matches against
/// and the facets a `tk show`-style block renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepItem {
    pub display_id: String,
    pub item_class: ItemClass,
    pub ticket_kind: Option<TicketKind>,
    pub priority: Option<Priority>,
    pub title: String,
    pub body: String,
    pub status: ItemStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// Failure from [`scan`]. Splits a Repository Store read fault from a visitor
/// write fault so the command renders the right diagnostic: a `Sql` error is a
/// storage failure, a `Write` error is a broken stdout (e.g. `| head`).
#[derive(Debug)]
pub enum ScanError {
    Sql(rusqlite::Error),
    Write(std::io::Error),
}

/// Whole-store scan in creation order: every Item, every Item Status. `visit`
/// is called once per row as it streams from SQLite (`rows.next()` steps
/// lazily), so only one [`GrepItem`] is live at a time. A visitor write error
/// stops the scan and surfaces as [`ScanError::Write`].
pub fn scan<F>(store: &Store, mut visit: F) -> Result<(), ScanError>
where
    F: FnMut(GrepItem) -> std::io::Result<()>,
{
    const SCAN_SQL: &str = "\
select display_value, item_class, ticket_kind, priority, title, body, status, \
       created_at, updated_at \
  from items \
 order by created_seq asc";

    let mut stmt = store.conn().prepare(SCAN_SQL).map_err(ScanError::Sql)?;
    let mut rows = stmt.query([]).map_err(ScanError::Sql)?;
    while let Some(row) = rows.next().map_err(ScanError::Sql)? {
        let item = item_from_row(row).map_err(ScanError::Sql)?;
        visit(item).map_err(ScanError::Write)?;
    }
    Ok(())
}

fn item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GrepItem> {
    Ok(GrepItem {
        display_id: row.get(0)?,
        item_class: row.get(1)?,
        ticket_kind: row.get(2)?,
        priority: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        status: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
