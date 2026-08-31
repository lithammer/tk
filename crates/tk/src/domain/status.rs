//! Item Status for Tickets and Epics.
//!
//! Not stored: Item Status is derived from the two axes `items` does store,
//! [`Lifecycle`] and [`WorkState`] (ADR-0043). `text()` is therefore the CLI
//! rendering users read and script against, not a storage contract — nothing
//! writes an `ItemStatus` to a column.

use std::fmt;

use crate::domain::lifecycle::Lifecycle;
use crate::domain::work_state::WorkState;

/// Lifecycle state shared by Tickets and Epics. `ItemStatus::Open` is the
/// default for newly-created local work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemStatus {
    Open,
    Active,
    Done,
}

impl ItemStatus {
    /// Derive the rendered Item Status from the two stored axes (ADR-0043):
    /// `done` when [`Lifecycle`] is done, otherwise `active` when
    /// [`WorkState`] is active, else `open`.
    ///
    /// The schema admits a `(done, active)` row — both `done` writers clear
    /// Work State rather than a CHECK aborting the write that would produce
    /// one — so the `Done` arm answers first and such a row renders as done
    /// everywhere.
    #[must_use]
    pub fn of(lifecycle: Lifecycle, work_state: WorkState) -> Self {
        match (lifecycle, work_state) {
            (Lifecycle::Done, _) => Self::Done,
            (Lifecycle::Open, WorkState::Active) => Self::Active,
            (Lifecycle::Open, WorkState::Idle) => Self::Open,
        }
    }

    /// CLI rendering string, and what `Display` writes.
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
    /// presentation glyph never collapse into the same source — the `Display`
    /// impl must keep the ASCII spelling users read and script against.
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
    fn glyph_is_distinct_from_text() {
        // Guard against accidentally collapsing the storage spelling and the
        // tree glyph: `tk adopt`'s `Status:` line and every scripted comparison
        // read `text()`, and both break if it ever returns a non-ASCII glyph.
        assert_ne!(ItemStatus::Open.text(), ItemStatus::Open.glyph());
    }

    #[test]
    fn derives_the_three_rendered_values_from_the_two_stored_axes() {
        // ADR-0043's derivation, spelled out over the reachable pairs. A drift
        // here silently changes what every command renders.
        assert_eq!(
            ItemStatus::of(Lifecycle::Open, WorkState::Idle),
            ItemStatus::Open
        );
        assert_eq!(
            ItemStatus::of(Lifecycle::Open, WorkState::Active),
            ItemStatus::Active
        );
        assert_eq!(
            ItemStatus::of(Lifecycle::Done, WorkState::Idle),
            ItemStatus::Done
        );
    }

    #[test]
    fn a_done_item_left_active_still_derives_done() {
        // Both `done` writers clear Work State, so `(done, active)` is
        // unreachable through them — but the schema admits it, and the
        // derivation must embody "done wins" rather than inherit it from its
        // writers. If this fails, a store repaired by hand would render work
        // that is closed as still in progress.
        assert_eq!(
            ItemStatus::of(Lifecycle::Done, WorkState::Active),
            ItemStatus::Done
        );
    }
}
