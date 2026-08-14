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
    /// Every V1 Mutation kind, written out rather than derived for the same
    /// reason [`text`] is: a caller that has to reason over the whole set —
    /// checking a stored spelling, or that a rule agrees with the SQL encoding
    /// it a second time — reads this list instead of maintaining its own.
    ///
    /// [`text`]: MutationType::text
    pub const ALL: [Self; 11] = [
        Self::UpdateTicket,
        Self::UpdateEpic,
        Self::SetItemStatus,
        Self::AddTicketToEpic,
        Self::RemoveTicketFromEpic,
        Self::AddDependency,
        Self::RemoveDependency,
        Self::AddExternalBlocker,
        Self::ResolveExternalBlocker,
        Self::PromoteTicket,
        Self::PromoteEpic,
    ];

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

    /// Whether this Mutation creates the backend object rather than editing one
    /// the Backend already has (ADR-0036).
    ///
    /// A Promotion is the Mutation every other Mutation of the same Promotion
    /// Operation is ordered behind: its receipt carries the identity they
    /// address, so it cannot be skipped and its acceptance converts the Item.
    /// Written as an exhaustive match so a third Promotion kind has to answer
    /// here instead of silently reading as an ordinary Mutation.
    #[must_use]
    pub fn is_promotion(self) -> bool {
        match self {
            Self::PromoteTicket | Self::PromoteEpic => true,
            Self::UpdateTicket
            | Self::UpdateEpic
            | Self::SetItemStatus
            | Self::AddTicketToEpic
            | Self::RemoveTicketFromEpic
            | Self::AddDependency
            | Self::RemoveDependency
            | Self::AddExternalBlocker
            | Self::ResolveExternalBlocker => false,
        }
    }

    /// Which other item's Backend address this Mutation's delivery has to
    /// resolve, beyond the target named by `mutations.item_id` (ADR-0038).
    ///
    /// Promotion Cancellation asks this of every Mutation ordered behind a
    /// withdrawn Promotion: one whose counterpart loses its prospective
    /// identity can never be applied, so it is withdrawn too. The match is
    /// exhaustive so a Mutation kind added later cannot reach the Mutation Log
    /// without a decision about what a withdrawal does to it.
    #[must_use]
    pub fn addressed_counterpart(self) -> AddressedCounterpart {
        match self {
            Self::AddTicketToEpic => AddressedCounterpart::Epic,
            Self::AddDependency | Self::RemoveDependency => AddressedCounterpart::BlockingItem,
            // Clearing Epic Membership is a 0..1 slot the Backend resolves
            // without naming the Epic, so it survives the Epic's withdrawal.
            Self::RemoveTicketFromEpic
            | Self::UpdateTicket
            | Self::UpdateEpic
            | Self::SetItemStatus
            | Self::AddExternalBlocker
            | Self::ResolveExternalBlocker
            | Self::PromoteTicket
            | Self::PromoteEpic => AddressedCounterpart::None,
        }
    }
}

/// The counterpart role a Mutation's payload names, when its delivery needs
/// that item's Backend address (ADR-0038).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressedCounterpart {
    /// The Mutation addresses nothing beyond its own target.
    None,
    /// The Epic named by the payload's `epic_id`.
    Epic,
    /// The Blocking Item named by the payload's `blocking_id`.
    BlockingItem,
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

    #[test]
    fn every_variant_round_trips_through_text_and_from_str() {
        for t in MutationType::ALL {
            assert_eq!(MutationType::from_str(t.text()), Ok(t));
        }
    }

    #[test]
    fn only_the_promote_kinds_are_promotions() {
        let promotions: Vec<&str> = MutationType::ALL
            .into_iter()
            .filter(|t| t.is_promotion())
            .map(MutationType::text)
            .collect();
        assert_eq!(promotions, vec!["promote_ticket", "promote_epic"]);
    }

    #[test]
    fn adding_epic_membership_addresses_the_epic_and_clearing_it_does_not() {
        // The asymmetry ADR-0038 turns on: withdrawing an Epic's Promotion
        // withdraws the additions naming it, while a removal still applies
        // because the Backend clears a 0..1 slot without addressing the Epic.
        assert_eq!(
            MutationType::AddTicketToEpic.addressed_counterpart(),
            AddressedCounterpart::Epic
        );
        assert_eq!(
            MutationType::RemoveTicketFromEpic.addressed_counterpart(),
            AddressedCounterpart::None
        );
    }

    #[test]
    fn both_dependency_kinds_address_their_blocking_item() {
        for mutation_type in [MutationType::AddDependency, MutationType::RemoveDependency] {
            assert_eq!(
                mutation_type.addressed_counterpart(),
                AddressedCounterpart::BlockingItem,
                "{mutation_type} names a Blocking Item the Backend must address"
            );
        }
    }

    #[test]
    fn a_promotion_addresses_no_counterpart() {
        // A Promotion creates its own target, so it can only join a withdrawn
        // set as one of the cancelled Promotions themselves.
        for mutation_type in MutationType::ALL.into_iter().filter(|t| t.is_promotion()) {
            assert_eq!(
                mutation_type.addressed_counterpart(),
                AddressedCounterpart::None
            );
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
