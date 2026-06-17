//! Backend kind: the typed `<kind>` of `tk remote set` and the
//! `remotes.backend_kind` storage spelling.
//!
//! The two variants are mirrored in the V1 `remotes.backend_kind` CHECK
//! constraint (`'github'`, `'jira'`); the `text()` spelling is the storage
//! contract, written out explicitly so renaming a variant cannot silently
//! break the SQL CHECK.
//!
//! Per ADR-0033 the Display ID namespace a Remote occupies is *not* a property
//! of the kind — it is per-(kind, config) and owned by the Backend Adapter
//! (GitHub picks `gh`; Jira inherits the configured project key) — so this enum
//! deliberately carries no `display_prefix()`.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The Backend a Remote points at.
///
/// `Jira` is a valid kind and CHECK value, but `tk remote set jira` is refused
/// in v1 (ADR-0033): no Jira Backend Adapter exists yet and its `config_json`
/// shape is unsettled. The variant exists so `tk remote` (show), fixtures, and
/// the future Jira Backend Adapter (tk-35) can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Github,
    Jira,
}

impl BackendKind {
    /// SQLite storage spelling and the value the `remotes.backend_kind` CHECK
    /// constraint accepts.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Jira => "jira",
        }
    }
}

impl fmt::Display for BackendKind {
    /// Single-sources the rendered spelling on [`BackendKind::text`] so CLI
    /// output and the SQL spelling never diverge.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Returned by [`BackendKind::from_str`] when the text is not a known Backend
/// kind. Carries the offending value so the command can surface a verbatim
/// diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown backend kind: {0}")]
pub struct ParseBackendKindError(pub String);

impl FromStr for BackendKind {
    type Err = ParseBackendKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(Self::Github),
            "jira" => Ok(Self::Jira),
            other => Err(ParseBackendKindError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_matches_the_check_constrained_spellings() {
        // Pins the storage spellings against the `remotes.backend_kind` CHECK;
        // drift here is a silent store-contract break.
        assert_eq!(BackendKind::Github.text(), "github");
        assert_eq!(BackendKind::Jira.text(), "jira");
    }

    #[test]
    fn round_trips_through_text_and_from_str() {
        for k in [BackendKind::Github, BackendKind::Jira] {
            assert_eq!(BackendKind::from_str(k.text()), Ok(k));
        }
    }

    #[test]
    fn unknown_text_is_rejected() {
        assert_eq!(
            BackendKind::from_str("gitlab"),
            Err(ParseBackendKindError("gitlab".to_string()))
        );
    }

    #[test]
    fn display_writes_text() {
        assert_eq!(format!("{}", BackendKind::Github), "github");
    }
}
