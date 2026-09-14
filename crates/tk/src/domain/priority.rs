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

/// Returned by [`Priority::from_str`] when the text is not an accepted CLI spelling.
/// Its `Display` is the message `clap` surfaces for an invalid `--priority`
/// value, so the single parser (used by `tk add` / `tk update` / `tk accept`)
/// keeps one verbatim diagnostic instead of drifting per-command copies.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid priority `{0}` (expected P0..P4, p0..p4, or 0..4)")]
pub struct ParsePriorityError(pub String);

impl FromStr for Priority {
    type Err = ParsePriorityError;

    /// Parse a CLI `--priority` token: `P0`..`P4`, `p0`..`p4`, or `0`..`4`.
    /// Storage and rendering still use [`Priority::text`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "P0" | "p0" | "0" => Ok(Self::P0),
            "P1" | "p1" | "1" => Ok(Self::P1),
            "P2" | "p2" | "2" => Ok(Self::P2),
            "P3" | "p3" | "3" => Ok(Self::P3),
            "P4" | "p4" | "4" => Ok(Self::P4),
            other => Err(ParsePriorityError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_accepts_cli_spellings_and_keeps_canonical_text() {
        for (spellings, expected, text) in [
            (["P0", "p0", "0"], Priority::P0, "P0"),
            (["P1", "p1", "1"], Priority::P1, "P1"),
            (["P2", "p2", "2"], Priority::P2, "P2"),
            (["P3", "p3", "3"], Priority::P3, "P3"),
            (["P4", "p4", "4"], Priority::P4, "P4"),
        ] {
            for spelling in spellings {
                let priority = Priority::from_str(spelling).unwrap();
                assert_eq!(priority, expected, "{spelling}");
                assert_eq!(priority.text(), text);
                assert_eq!(priority.to_string(), text);
            }
        }
    }

    #[test]
    fn from_str_rejects_other_spellings_with_shared_diagnostic() {
        for spelling in [
            "", "P5", "p5", "5", "P10", "10", "01", "p01", "-1", "+1", "p-1", "high", "crit",
            " P1", "P1 ", "1\n", "Ｐ1",
        ] {
            assert_eq!(
                Priority::from_str(spelling),
                Err(ParsePriorityError(spelling.to_owned())),
            );
        }
        assert_eq!(
            Priority::from_str("high").unwrap_err().to_string(),
            "invalid priority `high` (expected P0..P4, p0..p4, or 0..4)",
        );
    }

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
