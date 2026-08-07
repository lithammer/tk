//! Static declaration of what a Backend Adapter can represent under
//! Promotion (ADR-0036 "Backend capability is declared per facet and
//! staged").
//!
//! Pure domain data: no SQLite, subprocess, or rendering. Preflight reads a
//! Backend Adapter's [`PromotionCapabilities`] and rejects before any backend
//! call, so an Adapter never accepts Promotion intent it cannot later apply.
//! Capability is staged facet by facet as an Adapter earns it: an Adapter
//! starts at [`PromotionCapabilities::none`] and declares each facet only once
//! it can apply the Mutations that facet admits to the outbox.

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

/// What a Backend Adapter can represent under Promotion, declared per facet:
/// which Item Classes it can create, which Ticket Kinds it can create,
/// whether it can represent a Dependency, and whether it can represent Epic
/// membership.
///
/// Facets are queried by typed value (`can_create_item_class`,
/// `can_create_ticket_kind`) rather than as a collection callers index
/// themselves, so a caller asks the typed question directly and the enum
/// variant, not an integer, selects the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionCapabilities {
    item_classes: ItemClasses,
    ticket_kinds: TicketKinds,
    dependencies: bool,
    epic_membership: bool,
}

impl PromotionCapabilities {
    /// The baseline every Backend Adapter starts from: no Item Class, Ticket
    /// Kind, Dependency, or Epic membership representable under Promotion.
    #[must_use]
    pub fn none() -> Self {
        Self {
            item_classes: ItemClasses::NONE,
            ticket_kinds: TicketKinds::NONE,
            dependencies: false,
            epic_membership: false,
        }
    }

    /// Every facet on — the declaration a test uses when the Backend is not
    /// what it is exercising.
    ///
    /// Built from the fields rather than by chaining `with_*`, so a facet added
    /// later is a compile error here instead of leaving "everything" quietly
    /// meaning less than it says.
    #[must_use]
    pub fn all() -> Self {
        Self {
            item_classes: ItemClasses::ALL,
            ticket_kinds: TicketKinds::ALL,
            dependencies: true,
            epic_membership: true,
        }
    }

    /// Declare that Promotion can create the given Item Class on this
    /// Backend.
    #[must_use]
    pub fn with_item_class(mut self, class: ItemClass) -> Self {
        *self.item_classes.slot(class) = true;
        self
    }

    /// Declare that Promotion can create the given Ticket Kind on this
    /// Backend.
    #[must_use]
    pub fn with_ticket_kind(mut self, kind: TicketKind) -> Self {
        *self.ticket_kinds.slot(kind) = true;
        self
    }

    /// Declare that Promotion can represent a Dependency on this Backend.
    #[must_use]
    pub fn with_dependencies(mut self) -> Self {
        self.dependencies = true;
        self
    }

    /// Declare that Promotion can represent Epic membership on this Backend.
    #[must_use]
    pub fn with_epic_membership(mut self) -> Self {
        self.epic_membership = true;
        self
    }

    /// Whether Promotion can create the given Item Class on this Backend.
    #[must_use]
    pub fn can_create_item_class(&self, class: ItemClass) -> bool {
        self.item_classes.allows(class)
    }

    /// Whether Promotion can create the given Ticket Kind on this Backend.
    #[must_use]
    pub fn can_create_ticket_kind(&self, kind: TicketKind) -> bool {
        self.ticket_kinds.allows(kind)
    }

    /// Whether Promotion can represent a Dependency on this Backend.
    #[must_use]
    pub fn can_represent_dependencies(&self) -> bool {
        self.dependencies
    }

    /// Whether Promotion can represent Epic membership on this Backend.
    #[must_use]
    pub fn can_represent_epic_membership(&self) -> bool {
        self.epic_membership
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_declares_nothing() {
        let caps = PromotionCapabilities::none();
        assert!(!caps.can_create_item_class(ItemClass::Ticket));
        assert!(!caps.can_create_item_class(ItemClass::Epic));
        assert!(!caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(!caps.can_represent_dependencies());
        assert!(!caps.can_represent_epic_membership());
    }

    #[test]
    fn all_declares_every_facet() {
        let caps = PromotionCapabilities::all();
        assert!(caps.can_create_item_class(ItemClass::Ticket));
        assert!(caps.can_create_item_class(ItemClass::Epic));
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(caps.can_represent_dependencies());
        assert!(caps.can_represent_epic_membership());
    }

    #[test]
    fn with_item_class_declares_only_that_class() {
        let caps = PromotionCapabilities::none().with_item_class(ItemClass::Epic);
        assert!(caps.can_create_item_class(ItemClass::Epic));
        assert!(!caps.can_create_item_class(ItemClass::Ticket));
    }

    #[test]
    fn with_ticket_kind_declares_only_that_kind() {
        let caps = PromotionCapabilities::none().with_ticket_kind(TicketKind::Task);
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
    }

    #[test]
    fn with_dependencies_and_epic_membership_declare_independently() {
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
