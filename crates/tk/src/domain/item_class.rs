//! Item Class distinguishes Tickets from Epics in the Repository Store.
//!
//! Ported from `src/domain/item_class.zig`. The two-variant set is mirrored in
//! the V1 `items.item_class` CHECK constraint; the `text()` spelling is the
//! storage contract.

/// The top-level item class stored in the Repository Store. The default is
/// [`ItemClass::Ticket`] — Tickets outnumber Epics in every real repository
/// and the discriminator drives mutation-type selection across the store
/// layer, where a sensible default keeps request-builder boilerplate light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ItemClass {
    #[default]
    Ticket,
    Epic,
}

impl ItemClass {
    /// SQLite storage and CLI rendering string.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Ticket => "ticket",
            Self::Epic => "epic",
        }
    }

    /// Capitalized noun for user-facing diagnostics, e.g.
    /// `Created worktree for Ticket: …` or `cannot start a done Epic`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ticket => "Ticket",
            Self::Epic => "Epic",
        }
    }
}
