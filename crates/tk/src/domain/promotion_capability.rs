//! Static declaration of what a Backend Adapter can represent under
//! Promotion (ADR-0036 "Backend capability is declared per facet and
//! staged").
//!
//! Pure domain data: no SQLite, subprocess, or rendering. Preflight reads a
//! Backend Adapter's [`PromotionCapabilities`] and rejects before any backend
//! call, so an Adapter never accepts Promotion intent it cannot later apply.
//! Capability is staged facet by facet as an Adapter earns it — the GitHub
//! Adapter declares [`PromotionCapabilities::none`] until tk-137 (Task and
//! Epic creation) and tk-132 (Epic membership) turn facets on.

use super::item_class::ItemClass;
use super::ticket_kind::TicketKind;

/// What a Backend Adapter can represent under Promotion, declared per facet:
/// which Item Classes it can create, which Ticket Kinds it can create,
/// whether it can represent a Dependency, and whether it can represent Epic
/// membership.
///
/// Facets are queried by typed value (`can_create_item_class`,
/// `can_create_ticket_kind`) rather than as a collection callers index
/// themselves, so a caller asks the typed question directly instead of
/// reaching into storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionCapabilities {
    item_classes: [bool; 2],
    ticket_kinds: [bool; 2],
    dependencies: bool,
    epic_membership: bool,
}

impl PromotionCapabilities {
    /// The baseline every Backend Adapter starts from: no Item Class, Ticket
    /// Kind, Dependency, or Epic membership representable under Promotion.
    #[must_use]
    pub fn none() -> Self {
        Self {
            item_classes: [false, false],
            ticket_kinds: [false, false],
            dependencies: false,
            epic_membership: false,
        }
    }

    /// Declare that Promotion can create the given Item Class on this
    /// Backend.
    #[must_use]
    pub fn with_item_class(mut self, class: ItemClass) -> Self {
        self.item_classes[item_class_index(class)] = true;
        self
    }

    /// Declare that Promotion can create the given Ticket Kind on this
    /// Backend.
    #[must_use]
    pub fn with_ticket_kind(mut self, kind: TicketKind) -> Self {
        self.ticket_kinds[ticket_kind_index(kind)] = true;
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
        self.item_classes[item_class_index(class)]
    }

    /// Whether Promotion can create the given Ticket Kind on this Backend.
    #[must_use]
    pub fn can_create_ticket_kind(&self, kind: TicketKind) -> bool {
        self.ticket_kinds[ticket_kind_index(kind)]
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

fn item_class_index(class: ItemClass) -> usize {
    match class {
        ItemClass::Ticket => 0,
        ItemClass::Epic => 1,
    }
}

fn ticket_kind_index(kind: TicketKind) -> usize {
    match kind {
        TicketKind::Task => 0,
        TicketKind::Bug => 1,
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
