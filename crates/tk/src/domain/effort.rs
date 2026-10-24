//! Effort is a Shared Field: how much work a Ticket is expected to take,
//! apart from how much it matters (ADR-0054).
//!
//! The three-variant set is mirrored verbatim in the `items.effort` CHECK
//! constraint, so the SQL spelling returned by [`Effort::text`] is the
//! contract. The Mutation payload and the CLI use the same spelling.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Expected amount of work on a Ticket. Optional: a Ticket with none stores
/// `NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    /// SQLite storage and CLI spelling. Matches the `items.effort` CHECK
    /// constraint exactly.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Returned by [`Effort::from_str`]; its `Display` is the message `clap`
/// surfaces for an invalid `--effort` value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid effort `{0}` (expected low, medium, or high)")]
pub struct ParseEffortError(pub String);

impl FromStr for Effort {
    type Err = ParseEffortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(ParseEffortError(other.to_string())),
        }
    }
}
