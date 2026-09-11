//! Item Class distinguishes Tickets from Epics in the Repository Store.
//!
//! The two-variant set is mirrored in
//! the V1 `items.item_class` CHECK constraint; the `text()` spelling is the
//! storage contract.

use std::fmt;

use crate::domain::mutation_type::MutationType;

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

    /// The Mutation kind a title/body edit of this Item Class appends.
    #[must_use]
    pub fn update_mutation_type(self) -> MutationType {
        match self {
            Self::Ticket => MutationType::UpdateTicket,
            Self::Epic => MutationType::UpdateEpic,
        }
    }

    /// The Mutation kind that promotes this Item Class to a Backend (ADR-0036).
    ///
    /// The Repository Store enforces this pairing only for a Promotion already
    /// in flight: the cross-field CHECK names it under `applying` and
    /// `abandoned` alone, so a `pending` `promote_ticket` against an Epic
    /// inserts happily. It is a domain rule the Store cannot be relied on to
    /// catch, which is why one answer lives here rather than at each caller.
    #[must_use]
    pub fn promotion_mutation_type(self) -> MutationType {
        match self {
            Self::Ticket => MutationType::PromoteTicket,
            Self::Epic => MutationType::PromoteEpic,
        }
    }

    /// Capitalized noun for user-facing diagnostics, mid-sentence included:
    /// `Ticket 'tk-1' is done and cannot be reopened`, `cannot create Tickets
    /// under Promotion`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ticket => "Ticket",
            Self::Epic => "Epic",
        }
    }
}

impl fmt::Display for ItemClass {
    /// Single-sources the lowercase storage/CLI spelling on [`ItemClass::text`];
    /// the capitalized [`ItemClass::label`] is a separate diagnostic form and is
    /// intentionally not used here.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}
