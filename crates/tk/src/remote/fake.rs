//! Script-queue Backend Adapter for engine and command tests.
//!
//! Each script entry is consumed in order; an exhausted script panics so a
//! test that forgot to declare an interaction fails loudly instead of getting
//! a silent default. Script responses are moved from their queues; captured
//! calls own the input fields needed for assertions.

use std::collections::VecDeque;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemIdentity, BackendItemInspection,
    BackendItemRefresh,
};
use crate::domain::backend_outcome::{BackendCreateOutcome, BackendEditOutcome};
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::proc::ProcError;

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// Scripted response for one [`Adapter::adopt_ticket`] call.
#[derive(Debug)]
pub enum AdoptResponse {
    /// Success — the fake returns this canonical Ticket.
    Item(AdoptedItem),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Scripted response for one [`Adapter::refresh_item`] call.
#[derive(Debug)]
pub enum RefreshResponse {
    /// Success — the fake returns backend-owned fields for this key.
    Item(BackendItemRefresh),
    /// Adapter-level rejection with this detail.
    RecordedFailure(String),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Scripted response for one [`Adapter::inspect_item`] call.
#[derive(Debug)]
pub enum InspectionResponse {
    /// Success — the fake returns canonical identity, content, and Ticket Kind.
    Item(BackendItemInspection),
    /// Adapter-level rejection with this detail.
    RecordedFailure(String),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Scripted response for one [`Adapter::apply_edit`] call.
#[derive(Debug)]
pub enum EditResponse {
    /// The Backend acknowledges the edit.
    Success,
    /// The Backend rejects the edit.
    RecordedFailure(String),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Scripted response for one [`Adapter::create_item`] call.
#[derive(Debug)]
pub enum CreateResponse {
    /// Creation succeeded with this canonical Backend identity.
    Created {
        backend_key: String,
        display_id: String,
    },
    /// Creation is certified to have had no effect.
    Rejected(String),
    /// The Backend may have created the object despite the failure.
    Indeterminate(String),
}

/// Strict, script-queue Backend Adapter for tests.
///
/// Each directional script is consumed in order. Overflowing any script panics
/// so a test that under-declared its interactions fails loudly.
pub struct FakeAdapter {
    adopt_script: VecDeque<AdoptResponse>,
    refresh_script: VecDeque<RefreshResponse>,
    inspection_script: VecDeque<InspectionResponse>,
    edit_script: VecDeque<EditResponse>,
    create_script: VecDeque<CreateResponse>,
    /// Recorded edit invocations in call order — populated on every path,
    /// including rejection and environment failure.
    pub captured_edits: Vec<BackendEdit>,
    /// Recorded creation invocations in call order.
    pub captured_creates: Vec<BackendCreate>,
    /// Inputs passed to `adopt_ticket`, in call order.
    pub captured_adopt_inputs: Vec<String>,
    /// Backend keys passed to `refresh_item`, in call order.
    pub captured_refresh_keys: Vec<String>,
    /// Backend keys passed to `inspect_item`, in call order.
    pub captured_inspection_keys: Vec<String>,
    /// This fake's resolved capability value. Static data, not a script entry,
    /// so tests set it once via
    /// [`FakeAdapter::with_capabilities`] instead of queuing a response per
    /// call.
    capabilities: PromotionCapabilities,
}

impl FakeAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            adopt_script: VecDeque::new(),
            refresh_script: VecDeque::new(),
            inspection_script: VecDeque::new(),
            edit_script: VecDeque::new(),
            create_script: VecDeque::new(),
            captured_edits: Vec::new(),
            captured_creates: Vec::new(),
            captured_adopt_inputs: Vec::new(),
            captured_refresh_keys: Vec::new(),
            captured_inspection_keys: Vec::new(),
            capabilities: PromotionCapabilities::none(),
        }
    }

    #[must_use]
    pub fn with_adopts(mut self, script: Vec<AdoptResponse>) -> Self {
        self.adopt_script = script.into();
        self
    }

    #[must_use]
    pub fn with_refreshes(mut self, script: Vec<RefreshResponse>) -> Self {
        self.refresh_script = script.into();
        self
    }

    #[must_use]
    pub fn with_inspections(mut self, script: Vec<InspectionResponse>) -> Self {
        self.inspection_script = script.into();
        self
    }

    #[must_use]
    pub fn with_edits(mut self, script: Vec<EditResponse>) -> Self {
        self.edit_script = script.into();
        self
    }

    #[must_use]
    pub fn with_creates(mut self, script: Vec<CreateResponse>) -> Self {
        self.create_script = script.into();
        self
    }

    /// Configure this fake's resolved [`PromotionCapabilities`] value.
    /// Defaults to [`PromotionCapabilities::none`] so each test opts into the
    /// exact Promotion facets it exercises.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: PromotionCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl Default for FakeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for FakeAdapter {
    fn backend_kind(&self) -> BackendKind {
        BackendKind::Github
    }

    fn adopt_ticket(&mut self, input: &str) -> Result<AdoptedItem, AdapterReadError> {
        self.captured_adopt_inputs.push(input.to_string());
        let response = self
            .adopt_script
            .pop_front()
            .expect("FakeAdapter: adopt script exhausted");
        match response {
            AdoptResponse::Item(item) => Ok(item),
            AdoptResponse::EnvFailure(err) => Err(AdapterReadError::Env(err)),
        }
    }

    fn refresh_item(&mut self, key: &str) -> Result<BackendItemRefresh, AdapterReadError> {
        self.captured_refresh_keys.push(key.to_string());
        let response = self
            .refresh_script
            .pop_front()
            .expect("FakeAdapter: refresh script exhausted");
        match response {
            RefreshResponse::Item(item) => Ok(item),
            RefreshResponse::RecordedFailure(detail) => Err(AdapterReadError::Failed(detail)),
            RefreshResponse::EnvFailure(err) => Err(AdapterReadError::Env(err)),
        }
    }

    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError> {
        self.captured_inspection_keys.push(key.to_string());
        let response = self
            .inspection_script
            .pop_front()
            .expect("FakeAdapter: inspection script exhausted");
        match response {
            InspectionResponse::Item(item) => Ok(item),
            InspectionResponse::RecordedFailure(detail) => Err(AdapterReadError::Failed(detail)),
            InspectionResponse::EnvFailure(err) => Err(AdapterReadError::Env(err)),
        }
    }

    fn apply_edit(&mut self, edit: &BackendEdit) -> Result<BackendEditOutcome, ApplyError> {
        // Record before consulting the script so the rejection and env-failure
        // paths still leave evidence in `captured_edits`.
        self.captured_edits.push(edit.clone());

        let response = self
            .edit_script
            .pop_front()
            .expect("FakeAdapter: edit script exhausted");
        match response {
            EditResponse::Success => Ok(BackendEditOutcome::Acknowledged),
            EditResponse::RecordedFailure(detail) => Ok(BackendEditOutcome::rejected(detail)),
            EditResponse::EnvFailure(err) => Err(err),
        }
    }

    fn create_item(&mut self, create: &BackendCreate) -> BackendCreateOutcome {
        self.captured_creates.push(create.clone());
        let response = self
            .create_script
            .pop_front()
            .expect("FakeAdapter: create script exhausted");
        match response {
            CreateResponse::Created {
                backend_key,
                display_id,
            } => BackendCreateOutcome::Created(BackendItemIdentity {
                backend_key,
                display_id,
            }),
            CreateResponse::Rejected(detail) => BackendCreateOutcome::rejected(detail),
            CreateResponse::Indeterminate(detail) => BackendCreateOutcome::indeterminate(detail),
        }
    }

    fn resolve_promotion_capabilities(
        &mut self,
        _requirements: PromotionRequirements,
    ) -> Result<PromotionCapabilities, AdapterReadError> {
        Ok(self.capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemAddress;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::TitleBody;
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;

    fn adopted_item(backend_key: &str, display_id: &str) -> AdoptedItem {
        AdoptedItem {
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            ticket_kind: TicketKind::Task,
            title: "Title".into(),
            body: "Body".into(),
            status: ItemStatus::Open,
        }
    }

    fn refresh(title: &str) -> BackendItemRefresh {
        BackendItemRefresh {
            title: title.into(),
            body: "Body".into(),
            status: ItemStatus::Open,
            ticket_kind: Some(TicketKind::Task),
        }
    }

    fn inspection(title: &str) -> BackendItemInspection {
        BackendItemInspection {
            identity: BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/42".into(),
                display_id: "gh-42".into(),
            },
            title: title.into(),
            body: "Body".into(),
            ticket_kind: TicketKind::Task,
        }
    }

    fn edit() -> BackendEdit {
        BackendEdit::UpdateTicket {
            ticket: BackendItemAddress {
                backend_key: "1".into(),
            },
            snapshot: TitleBody {
                title: "T".into(),
                body: "B".into(),
            },
        }
    }

    fn create() -> BackendCreate {
        BackendCreate::Ticket {
            snapshot: TitleBody {
                title: "T".into(),
                body: "B".into(),
            },
            ticket_kind: TicketKind::Task,
        }
    }

    #[test]
    fn adopt_returns_scripted_item_and_captures_input() {
        let mut fake =
            FakeAdapter::new().with_adopts(vec![AdoptResponse::Item(adopted_item("1", "gh-1"))]);
        let got = fake.adopt_ticket("owner/repo#1").unwrap();
        assert_eq!(got.display_id, "gh-1");
        assert_eq!(got.ticket_kind, TicketKind::Task);
        assert_eq!(fake.captured_adopt_inputs, ["owner/repo#1"]);
    }

    #[test]
    fn refresh_returns_scripted_fields_and_captures_key() {
        let mut fake =
            FakeAdapter::new().with_refreshes(vec![RefreshResponse::Item(refresh("Refreshed"))]);
        let got = fake.refresh_item("42").unwrap();
        assert_eq!(got.title, "Refreshed");
        assert_eq!(got.ticket_kind, Some(TicketKind::Task));
        assert_eq!(fake.captured_refresh_keys, ["42"]);
    }

    #[test]
    fn inspection_returns_scripted_identity_and_captures_key() {
        let mut fake = FakeAdapter::new()
            .with_inspections(vec![InspectionResponse::Item(inspection("Inspected"))]);
        let got = fake
            .inspect_item("https://github.com/o/r/issues/42")
            .unwrap();
        assert_eq!(got.title, "Inspected");
        assert_eq!(got.identity.display_id, "gh-42");
        assert_eq!(
            fake.captured_inspection_keys,
            ["https://github.com/o/r/issues/42"]
        );
    }

    #[test]
    fn inspection_failure_variants_remain_distinct() {
        let mut fake = FakeAdapter::new().with_inspections(vec![
            InspectionResponse::RecordedFailure("HTTP 502".into()),
            InspectionResponse::EnvFailure(ProcError::SpawnFailed),
        ]);
        assert!(matches!(
            fake.inspect_item("42"),
            Err(AdapterReadError::Failed(detail)) if detail == "HTTP 502"
        ));
        assert!(matches!(
            fake.inspect_item("42"),
            Err(AdapterReadError::Env(ProcError::SpawnFailed))
        ));
        assert_eq!(fake.captured_inspection_keys, ["42", "42"]);
    }

    #[test]
    fn refresh_recorded_failure_returns_failed_with_detail() {
        let mut fake = FakeAdapter::new().with_refreshes(vec![RefreshResponse::RecordedFailure(
            "gh: HTTP 502".into(),
        )]);
        let err = fake.refresh_item("42").unwrap_err();
        match err {
            AdapterReadError::Failed(detail) => assert!(detail.contains("HTTP 502")),
            AdapterReadError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
    }

    #[test]
    fn adopt_env_failure_returns_bare_error() {
        let mut fake = FakeAdapter::new().with_adopts(vec![AdoptResponse::EnvFailure(
            ProcError::ExecutableNotFound,
        )]);
        assert!(matches!(
            fake.adopt_ticket("1").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
    }

    #[test]
    fn refresh_advances_script_across_calls() {
        let mut fake = FakeAdapter::new().with_refreshes(vec![
            RefreshResponse::Item(refresh("First")),
            RefreshResponse::EnvFailure(ProcError::ExecutableNotFound),
        ]);
        assert_eq!(fake.refresh_item("1").unwrap().title, "First");
        assert!(fake.refresh_item("2").is_err());
        assert_eq!(fake.captured_refresh_keys.len(), 2);
    }

    #[test]
    fn edit_success_returns_acknowledgement_and_captures_the_call() {
        let mut fake = FakeAdapter::new().with_edits(vec![EditResponse::Success]);
        let outcome = fake.apply_edit(&edit()).unwrap();
        assert_eq!(outcome, BackendEditOutcome::Acknowledged);
        let BackendEdit::UpdateTicket {
            ticket, snapshot, ..
        } = &fake.captured_edits[0]
        else {
            panic!("expected ticket update")
        };
        assert_eq!(ticket.backend_key, "1");
        assert_eq!(snapshot.title, "T");
    }

    #[test]
    fn create_success_returns_the_scripted_identity() {
        let mut fake = FakeAdapter::new().with_creates(vec![CreateResponse::Created {
            backend_key: "42".into(),
            display_id: "gh-42".into(),
        }]);
        let outcome = fake.create_item(&create());
        let BackendCreateOutcome::Created(identity) = outcome else {
            panic!("expected created identity")
        };
        assert_eq!(identity.backend_key, "42");
        assert_eq!(identity.display_id, "gh-42");
        assert!(matches!(
            fake.captured_creates[0],
            BackendCreate::Ticket { .. }
        ));
    }

    #[test]
    fn edit_rejection_and_environment_failure_remain_distinct() {
        let mut fake = FakeAdapter::new().with_edits(vec![
            EditResponse::RecordedFailure("validation: title required".into()),
            EditResponse::EnvFailure(ProcError::SpawnFailed),
        ]);
        let outcome = fake.apply_edit(&edit()).unwrap();
        let BackendEditOutcome::Rejected(failure) = outcome else {
            panic!("expected rejection")
        };
        assert_eq!(failure.detail, "validation: title required");
        let err = fake.apply_edit(&edit());
        assert!(matches!(err, Err(ProcError::SpawnFailed)));
        assert_eq!(fake.captured_edits.len(), 2);
    }

    #[test]
    fn creation_scripts_all_three_certainty_outcomes() {
        let mut fake = FakeAdapter::new().with_creates(vec![
            CreateResponse::Rejected("preflight validation".into()),
            CreateResponse::Indeterminate("connection lost".into()),
        ]);
        assert!(matches!(
            fake.create_item(&create()),
            BackendCreateOutcome::Rejected(_)
        ));
        assert!(matches!(
            fake.create_item(&create()),
            BackendCreateOutcome::Indeterminate(_)
        ));
        assert_eq!(fake.captured_creates.len(), 2);
    }

    #[test]
    fn defaults_to_no_promotion_capabilities() {
        let mut fake = FakeAdapter::new();
        assert_eq!(
            fake.resolve_promotion_capabilities(PromotionRequirements::none())
                .unwrap(),
            PromotionCapabilities::none()
        );
    }

    #[test]
    fn with_capabilities_overrides_the_declaration() {
        let caps = PromotionCapabilities::none().with_item_class(ItemClass::Epic);
        let mut fake = FakeAdapter::new().with_capabilities(caps);
        assert_eq!(
            fake.resolve_promotion_capabilities(PromotionRequirements::none())
                .unwrap(),
            caps
        );
    }
}
