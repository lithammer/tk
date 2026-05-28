//! Item Status for Tickets and Epics.
//!
//! Ported from `src/domain/status.zig`. The three lifecycle states are mirrored
//! in the V1 `items.status` CHECK constraint; the `text()` spelling is the
//! storage contract, not just a rendering convenience.

use std::fmt;

/// Lifecycle state shared by Tickets and Epics. `ItemStatus::Open` is the
/// default for newly-created local work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ItemStatus {
    #[default]
    Open,
    Active,
    Done,
}

impl ItemStatus {
    /// SQLite storage and CLI rendering string.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Active => "active",
            Self::Done => "done",
        }
    }

    /// Compact tree glyph used by `tk list` and `tk show` rendering. Kept
    /// separate from [`ItemStatus::text`] so the storage spelling and the
    /// presentation glyph never collapse into the same source — the SQL
    /// CHECK constraint and the `Display` impl must keep ASCII spellings.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Open => "○",
            Self::Active => "◐",
            Self::Done => "✓",
        }
    }
}

impl fmt::Display for ItemStatus {
    /// Single-sources the unstyled representation on [`ItemStatus::text`]; the
    /// tree [`ItemStatus::glyph`] is a separate presentation and intentionally
    /// not used here.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", ItemStatus::Active), "active");
    }

    #[test]
    fn default_is_open() {
        assert_eq!(ItemStatus::default(), ItemStatus::Open);
    }

    #[test]
    fn glyph_is_distinct_from_text() {
        // Guard against accidentally collapsing the storage spelling and the
        // tree glyph: SQL CHECK constraints break if `text()` ever returns a
        // non-ASCII glyph.
        assert_ne!(ItemStatus::Open.text(), ItemStatus::Open.glyph());
    }
}
