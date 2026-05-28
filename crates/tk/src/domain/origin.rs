//! Origin describes whether an Item is local or backend-backed.
//!
//! The two-variant set is mirrored in the
//! V1 `items.origin` CHECK constraint; the `text()` spelling is the storage
//! contract.

/// Source of authority for a Ticket or Epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    Local,
    Backend,
}

impl Origin {
    /// SQLite storage and CLI rendering string.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Backend => "backend",
        }
    }
}
