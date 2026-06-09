//! Priority is a local-only Ticket ranking.
//!
//! The five-variant set is mirrored
//! verbatim in the V1 `items.priority` CHECK constraint, so the SQL spelling
//! returned by [`Priority::text`] is the contract — not just a rendering
//! convenience.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Local-only Ticket ranking. Lower discriminants sort before higher ones, so
/// `Priority::P0` is the highest-priority ticket and `Priority::P4` the lowest.
/// `Priority::P2` is the default for newly-created local Tickets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    P0,
    P1,
    #[default]
    P2,
    P3,
    P4,
}

impl Priority {
    /// SQLite storage and CLI rendering string. Matches the
    /// `items.priority` CHECK constraint exactly.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::P0 => "P0",
            Self::P1 => "P1",
            Self::P2 => "P2",
            Self::P3 => "P3",
            Self::P4 => "P4",
        }
    }
}

impl fmt::Display for Priority {
    /// Single-sources the unstyled representation on [`Priority::text`]; styled
    /// render sites still wrap `text()` through the Styler.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Returned by [`Priority::from_str`] when the text is not a `P0..P4` spelling.
/// Its `Display` is the message `clap` surfaces for an invalid `--priority`
/// value, so the single parser (used by `tk add` / `tk update` / `tk accept`)
/// keeps one verbatim diagnostic instead of drifting per-command copies.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid priority `{0}` (expected P0, P1, P2, P3, or P4)")]
pub struct ParsePriorityError(pub String);

impl FromStr for Priority {
    type Err = ParsePriorityError;

    /// Parse a CLI `--priority` token. The accepted spellings mirror
    /// [`Priority::text`] and the `items.priority` CHECK constraint.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "P0" => Ok(Self::P0),
            "P1" => Ok(Self::P1),
            "P2" => Ok(Self::P2),
            "P3" => Ok(Self::P3),
            "P4" => Ok(Self::P4),
            other => Err(ParsePriorityError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", Priority::P1), "P1");
    }

    #[test]
    fn default_is_p2() {
        assert_eq!(Priority::default(), Priority::P2);
    }

    #[test]
    fn ordering_matches_ranking() {
        // P0 is the highest-priority ticket and must sort before P4.
        assert!(Priority::P0 < Priority::P4);
    }
}
