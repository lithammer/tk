//! Typed data describing what a Backend Adapter can represent under Promotion
//! (ADR-0036, ADR-0021).
//!
//! Pure domain data: no SQLite, subprocess, or rendering. Pure graph analysis
//! produces [`PromotionRequirements`]; an Adapter resolves those requirements
//! into [`PromotionCapabilities`] before the planner commits Promotion intent.

use super::item_class::ItemClass;
use super::ticket_kind::TicketKind;

/// Which Item Classes a Backend Adapter can create under Promotion — one field
/// per [`ItemClass`] variant, selected by matching on the value, so a new
/// variant is a compile error in both the field list and every `match` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ItemClasses {
    ticket: bool,
    epic: bool,
}

impl ItemClasses {
    const NONE: Self = Self {
        ticket: false,
        epic: false,
    };
    const ALL: Self = Self {
        ticket: true,
        epic: true,
    };

    fn allows(self, class: ItemClass) -> bool {
        match class {
            ItemClass::Ticket => self.ticket,
            ItemClass::Epic => self.epic,
        }
    }

    fn slot(&mut self, class: ItemClass) -> &mut bool {
        match class {
            ItemClass::Ticket => &mut self.ticket,
            ItemClass::Epic => &mut self.epic,
        }
    }
}

/// Which Ticket Kinds a Backend Adapter can create under Promotion. Shaped
/// like [`ItemClasses`], for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TicketKinds {
    task: bool,
    bug: bool,
}

impl TicketKinds {
    const NONE: Self = Self {
        task: false,
        bug: false,
    };
    const ALL: Self = Self {
        task: true,
        bug: true,
    };

    fn allows(self, kind: TicketKind) -> bool {
        match kind {
            TicketKind::Task => self.task,
            TicketKind::Bug => self.bug,
        }
    }

    fn slot(&mut self, kind: TicketKind) -> &mut bool {
        match kind {
            TicketKind::Task => &mut self.task,
            TicketKind::Bug => &mut self.bug,
        }
    }
}

/// The exhaustive facet set shared by requirements and resolved capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromotionFacets {
    item_classes: ItemClasses,
    ticket_kinds: TicketKinds,
    dependencies: bool,
    epic_membership: bool,
}

impl PromotionFacets {
    const NONE: Self = Self {
        item_classes: ItemClasses::NONE,
        ticket_kinds: TicketKinds::NONE,
        dependencies: false,
        epic_membership: false,
    };
    const ALL: Self = Self {
        item_classes: ItemClasses::ALL,
        ticket_kinds: TicketKinds::ALL,
        dependencies: true,
        epic_membership: true,
    };

    fn with_item_class(mut self, class: ItemClass) -> Self {
        *self.item_classes.slot(class) = true;
        self
    }

    fn with_ticket_kind(mut self, kind: TicketKind) -> Self {
        *self.ticket_kinds.slot(kind) = true;
        self
    }

    fn with_dependencies(mut self) -> Self {
        self.dependencies = true;
        self
    }

    fn with_epic_membership(mut self) -> Self {
        self.epic_membership = true;
        self
    }
}

/// The Backend capability facets one Promotion graph requires.
///
/// Pure graph analysis derives this value without contacting the Backend. The
/// Adapter resolves it before the Promotion planner runs; only
/// repository-specific facets need a Backend read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionRequirements {
    facets: PromotionFacets,
}

impl PromotionRequirements {
    /// Start with no capability requirements.
    #[must_use]
    pub fn none() -> Self {
        Self {
            facets: PromotionFacets::NONE,
        }
    }

    /// Add a required Item Class facet.
    #[must_use]
    pub fn with_item_class(mut self, class: ItemClass) -> Self {
        self.facets = self.facets.with_item_class(class);
        self
    }

    /// Add a required Ticket Kind facet.
    #[must_use]
    pub fn with_ticket_kind(mut self, kind: TicketKind) -> Self {
        self.facets = self.facets.with_ticket_kind(kind);
        self
    }

    /// Add the Dependency facet.
    #[must_use]
    pub fn with_dependencies(mut self) -> Self {
        self.facets = self.facets.with_dependencies();
        self
    }

    /// Add the Epic-membership facet.
    #[must_use]
    pub fn with_epic_membership(mut self) -> Self {
        self.facets = self.facets.with_epic_membership();
        self
    }

    /// Whether the Promotion requires the given Item Class facet.
    #[must_use]
    pub fn requires_item_class(self, class: ItemClass) -> bool {
        self.facets.item_classes.allows(class)
    }

    /// Whether the Promotion requires the given Ticket Kind facet.
    #[must_use]
    pub fn requires_ticket_kind(self, kind: TicketKind) -> bool {
        self.facets.ticket_kinds.allows(kind)
    }

    /// Whether the Promotion requires the Dependency facet.
    #[must_use]
    pub fn requires_dependencies(self) -> bool {
        self.facets.dependencies
    }

    /// Whether the Promotion requires the Epic-membership facet.
    #[must_use]
    pub fn requires_epic_membership(self) -> bool {
        self.facets.epic_membership
    }
}

impl Default for PromotionRequirements {
    fn default() -> Self {
        Self::none()
    }
}

/// The Promotion facets a Backend Adapter has resolved: which Item Classes and
/// Ticket Kinds it can create, whether it can represent a Dependency, and
/// whether it can represent Epic membership.
///
/// Facets are queried by typed value (`can_create_item_class`,
/// `can_create_ticket_kind`) rather than as a collection callers index
/// themselves, so a caller asks the typed question directly and the enum
/// variant, not an integer, selects the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionCapabilities {
    facets: PromotionFacets,
}

impl PromotionCapabilities {
    /// No resolved Promotion capability facets.
    #[must_use]
    pub fn none() -> Self {
        Self {
            facets: PromotionFacets::NONE,
        }
    }

    /// Every Promotion capability facet.
    ///
    /// Built from the fields rather than by chaining `with_*`, so a facet added
    /// later is a compile error here instead of leaving "everything" quietly
    /// meaning less than it says.
    #[must_use]
    pub fn all() -> Self {
        Self {
            facets: PromotionFacets::ALL,
        }
    }

    /// Mark the given Item Class as creatable on this Backend.
    #[must_use]
    pub fn with_item_class(mut self, class: ItemClass) -> Self {
        self.facets = self.facets.with_item_class(class);
        self
    }

    /// Mark the given Ticket Kind as creatable on this Backend.
    #[must_use]
    pub fn with_ticket_kind(mut self, kind: TicketKind) -> Self {
        self.facets = self.facets.with_ticket_kind(kind);
        self
    }

    /// Mark Dependencies as representable on this Backend.
    #[must_use]
    pub fn with_dependencies(mut self) -> Self {
        self.facets = self.facets.with_dependencies();
        self
    }

    /// Mark Epic membership as representable on this Backend.
    #[must_use]
    pub fn with_epic_membership(mut self) -> Self {
        self.facets = self.facets.with_epic_membership();
        self
    }

    /// Whether Promotion can create the given Item Class on this Backend.
    #[must_use]
    pub fn can_create_item_class(&self, class: ItemClass) -> bool {
        self.facets.item_classes.allows(class)
    }

    /// Whether Promotion can create the given Ticket Kind on this Backend.
    #[must_use]
    pub fn can_create_ticket_kind(&self, kind: TicketKind) -> bool {
        self.facets.ticket_kinds.allows(kind)
    }

    /// Whether Promotion can represent a Dependency on this Backend.
    #[must_use]
    pub fn can_represent_dependencies(&self) -> bool {
        self.facets.dependencies
    }

    /// Whether Promotion can represent Epic membership on this Backend.
    #[must_use]
    pub fn can_represent_epic_membership(&self) -> bool {
        self.facets.epic_membership
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_has_no_facets() {
        let caps = PromotionCapabilities::none();
        assert!(!caps.can_create_item_class(ItemClass::Ticket));
        assert!(!caps.can_create_item_class(ItemClass::Epic));
        assert!(!caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(!caps.can_represent_dependencies());
        assert!(!caps.can_represent_epic_membership());
    }

    #[test]
    fn all_has_every_facet() {
        let caps = PromotionCapabilities::all();
        assert!(caps.can_create_item_class(ItemClass::Ticket));
        assert!(caps.can_create_item_class(ItemClass::Epic));
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(caps.can_represent_dependencies());
        assert!(caps.can_represent_epic_membership());
    }

    #[test]
    fn with_item_class_adds_only_that_class() {
        let caps = PromotionCapabilities::none().with_item_class(ItemClass::Epic);
        assert!(caps.can_create_item_class(ItemClass::Epic));
        assert!(!caps.can_create_item_class(ItemClass::Ticket));
    }

    #[test]
    fn with_ticket_kind_adds_only_that_kind() {
        let caps = PromotionCapabilities::none().with_ticket_kind(TicketKind::Task);
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
    }

    #[test]
    fn dependencies_and_epic_membership_are_independent() {
        let caps = PromotionCapabilities::none().with_dependencies();
        assert!(caps.can_represent_dependencies());
        assert!(!caps.can_represent_epic_membership());
    }

    #[test]
    fn facets_compose() {
        let caps = PromotionCapabilities::none()
            .with_item_class(ItemClass::Ticket)
            .with_ticket_kind(TicketKind::Task)
            .with_dependencies()
            .with_epic_membership();
        assert!(caps.can_create_item_class(ItemClass::Ticket));
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(caps.can_represent_dependencies());
        assert!(caps.can_represent_epic_membership());
    }
}
