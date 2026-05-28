//! Typed V1 Mutation kind enum shared by the Mutation Log and Backend Adapters.
//!
//! The `text()` spelling matches the `mutations.mutation_type` SQL CHECK
//! constraint verbatim so the type round-trips a SQL text column through
//! [`MutationType::text`] / [`MutationType::from_str`] without an intermediate
//! map. The mapping is written out explicitly rather than derived from the
//! variant names so renaming a variant cannot silently break the SQL contract.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// All V1 Mutation kinds that the Mutation Log outbox may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationType {
    UpdateTicket,
    UpdateEpic,
    SetItemStatus,
    AddTicketToEpic,
    RemoveTicketFromEpic,
    AddDependency,
    RemoveDependency,
    AddExternalBlocker,
    ResolveExternalBlocker,
    PromoteTicket,
    PromoteEpic,
}

impl MutationType {
    /// SQL-compatible text spelling. Matches the
    /// `mutations.mutation_type` CHECK constraint exactly.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::UpdateTicket => "update_ticket",
            Self::UpdateEpic => "update_epic",
            Self::SetItemStatus => "set_item_status",
            Self::AddTicketToEpic => "add_ticket_to_epic",
            Self::RemoveTicketFromEpic => "remove_ticket_from_epic",
            Self::AddDependency => "add_dependency",
            Self::RemoveDependency => "remove_dependency",
            Self::AddExternalBlocker => "add_external_blocker",
            Self::ResolveExternalBlocker => "resolve_external_blocker",
            Self::PromoteTicket => "promote_ticket",
            Self::PromoteEpic => "promote_epic",
        }
    }
}

impl fmt::Display for MutationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Returned by [`MutationType::from_str`] when the SQL text does not match a
/// known V1 Mutation kind. Carries the offending value so the caller can
/// surface a verbatim diagnostic (ADR-0017 message contract).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown mutation_type: {0}")]
pub struct ParseMutationTypeError(pub String);

impl FromStr for MutationType {
    type Err = ParseMutationTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "update_ticket" => Ok(Self::UpdateTicket),
            "update_epic" => Ok(Self::UpdateEpic),
            "set_item_status" => Ok(Self::SetItemStatus),
            "add_ticket_to_epic" => Ok(Self::AddTicketToEpic),
            "remove_ticket_from_epic" => Ok(Self::RemoveTicketFromEpic),
            "add_dependency" => Ok(Self::AddDependency),
            "remove_dependency" => Ok(Self::RemoveDependency),
            "add_external_blocker" => Ok(Self::AddExternalBlocker),
            "resolve_external_blocker" => Ok(Self::ResolveExternalBlocker),
            "promote_ticket" => Ok(Self::PromoteTicket),
            "promote_epic" => Ok(Self::PromoteEpic),
            other => Err(ParseMutationTypeError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[MutationType] = &[
        MutationType::UpdateTicket,
        MutationType::UpdateEpic,
        MutationType::SetItemStatus,
        MutationType::AddTicketToEpic,
        MutationType::RemoveTicketFromEpic,
        MutationType::AddDependency,
        MutationType::RemoveDependency,
        MutationType::AddExternalBlocker,
        MutationType::ResolveExternalBlocker,
        MutationType::PromoteTicket,
        MutationType::PromoteEpic,
    ];

    #[test]
    fn every_variant_round_trips_through_text_and_from_str() {
        for t in ALL {
            assert_eq!(MutationType::from_str(t.text()), Ok(*t));
        }
    }

    #[test]
    fn unknown_text_is_rejected() {
        assert_eq!(
            MutationType::from_str("not_a_real_type"),
            Err(ParseMutationTypeError("not_a_real_type".to_string())),
        );
    }
}
