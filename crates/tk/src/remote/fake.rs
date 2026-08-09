//! Script-queue Backend Adapter for engine and command tests.
//!
//! Each script entry is consumed in order; an exhausted script panics so a
//! test that forgot to declare an interaction fails loudly instead of getting
//! a silent default. Script responses are moved from their queues; captured
//! calls own the input fields needed for assertions.

use std::collections::VecDeque;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemIdentity, BackendItemRefresh,
};
use crate::domain::backend_outcome::{BackendCreateOutcome, BackendEditOutcome};
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::proc::ProcError;

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// Scripted response for one [`Adapter::adopt_ticket`] call.
#[derive(Debug)]
pub enum AdoptResponse {
    /// Success — the fake returns this canonical Ticket.
    Item(AdoptedItem),
    /// Adapter-level rejection — returns [`AdapterReadError::Failed`] carrying this
    /// detail.
    RecordedFailure(String),
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
    /// This fake's [`Adapter::promotion_capabilities`] return value. Static
    /// data, not a script entry, so tests set it once via
    /// [`FakeAdapter::with_capabilities`] instead of queuing a response per
    /// call.
    capabilities: PromotionCapabilities,
    backend_kind: BackendKind,
}

impl FakeAdapter {
    #[must_use]
    pub fn directional(
        adopt_script: Vec<AdoptResponse>,
        refresh_script: Vec<RefreshResponse>,
        edit_script: Vec<EditResponse>,
        create_script: Vec<CreateResponse>,
    ) -> Self {
        Self {
            adopt_script: adopt_script.into(),
            refresh_script: refresh_script.into(),
            edit_script: edit_script.into(),
            create_script: create_script.into(),
            captured_edits: Vec::new(),
            captured_creates: Vec::new(),
            captured_adopt_inputs: Vec::new(),
            captured_refresh_keys: Vec::new(),
            capabilities: PromotionCapabilities::none(),
            backend_kind: BackendKind::Github,
        }
    }

    /// Script this fake's [`Adapter::promotion_capabilities`] declaration.
    /// Defaults to [`PromotionCapabilities::none`] so each test opts into the
    /// exact Promotion facets it exercises.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: PromotionCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Configure the Backend kind this scripted Adapter represents.
    #[must_use]
    pub fn with_backend_kind(mut self, backend_kind: BackendKind) -> Self {
        self.backend_kind = backend_kind;
        self
    }
}

impl Adapter for FakeAdapter {
    fn backend_kind(&self) -> BackendKind {
        self.backend_kind
    }

    fn adopt_ticket(&mut self, input: &str) -> Result<AdoptedItem, AdapterReadError> {
        self.captured_adopt_inputs.push(input.to_string());
        let response = self
            .adopt_script
            .pop_front()
            .expect("FakeAdapter: adopt script exhausted");
        match response {
            AdoptResponse::Item(item) => Ok(item),
            AdoptResponse::RecordedFailure(detail) => Err(AdapterReadError::Failed(detail)),
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

    fn apply_edit(
        &mut self,
        edit: &BackendEdit,
        _now: &str,
    ) -> Result<BackendEditOutcome, ApplyError> {
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

    fn create_item(&mut self, create: &BackendCreate, _now: &str) -> BackendCreateOutcome {
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

    fn promotion_capabilities(&self) -> PromotionCapabilities {
        self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{Promotion, TitleBody};
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

    fn edit(sequence: i64) -> BackendEdit {
        BackendEdit::UpdateTicket {
            sequence,
            item_id: "t1".into(),
            ticket: BackendItemIdentity {
                backend_key: "1".into(),
                display_id: "gh-1".into(),
            },
            snapshot: TitleBody {
                title: "T".into(),
                body: "B".into(),
            },
        }
    }

    fn create(sequence: i64) -> BackendCreate {
        BackendCreate::Ticket {
            sequence,
            item_id: "t1".into(),
            promotion: Promotion {
                title: "T".into(),
                body: "B".into(),
                backend_kind: "github".into(),
            },
        }
    }

    #[test]
    fn adopt_returns_scripted_item_and_captures_input() {
        let mut fake = FakeAdapter::directional(
            vec![AdoptResponse::Item(adopted_item("1", "gh-1"))],
            vec![],
            vec![],
            vec![],
        );
        let got = fake.adopt_ticket("owner/repo#1").unwrap();
        assert_eq!(got.display_id, "gh-1");
        assert_eq!(got.ticket_kind, TicketKind::Task);
        assert_eq!(fake.captured_adopt_inputs, ["owner/repo#1"]);
    }

    #[test]
    fn refresh_returns_scripted_fields_and_captures_key() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![RefreshResponse::Item(refresh("Refreshed"))],
            vec![],
            vec![],
        );
        let got = fake.refresh_item("42").unwrap();
        assert_eq!(got.title, "Refreshed");
        assert_eq!(got.ticket_kind, Some(TicketKind::Task));
        assert_eq!(fake.captured_refresh_keys, ["42"]);
    }

    #[test]
    fn refresh_recorded_failure_returns_failed_with_detail() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![RefreshResponse::RecordedFailure("gh: HTTP 502".into())],
            vec![],
            vec![],
        );
        let err = fake.refresh_item("42").unwrap_err();
        match err {
            AdapterReadError::Failed(detail) => assert!(detail.contains("HTTP 502")),
            AdapterReadError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
    }

    #[test]
    fn adopt_env_failure_returns_bare_error() {
        let mut fake = FakeAdapter::directional(
            vec![AdoptResponse::EnvFailure(ProcError::ExecutableNotFound)],
            vec![],
            vec![],
            vec![],
        );
        assert!(matches!(
            fake.adopt_ticket("1").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
    }

    #[test]
    fn refresh_advances_script_across_calls() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![
                RefreshResponse::Item(refresh("First")),
                RefreshResponse::EnvFailure(ProcError::ExecutableNotFound),
            ],
            vec![],
            vec![],
        );
        assert_eq!(fake.refresh_item("1").unwrap().title, "First");
        assert!(fake.refresh_item("2").is_err());
        assert_eq!(fake.captured_refresh_keys.len(), 2);
    }

    #[test]
    fn edit_success_returns_acknowledgement_and_captures_the_call() {
        let mut fake =
            FakeAdapter::directional(vec![], vec![], vec![EditResponse::Success], vec![]);
        let outcome = fake
            .apply_edit(&edit(1), "2026-05-19T00:00:00.000Z")
            .unwrap();
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
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![],
            vec![CreateResponse::Created {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }],
        );
        let outcome = fake.create_item(&create(1), "2026-05-19T00:00:00.000Z");
        let BackendCreateOutcome::Created(identity) = outcome else {
            panic!("expected created identity")
        };
        assert_eq!(identity.backend_key, "42");
        assert_eq!(identity.display_id, "gh-42");
        assert_eq!(fake.captured_creates[0].sequence(), 1);
    }

    #[test]
    fn edit_rejection_and_environment_failure_remain_distinct() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![
                EditResponse::RecordedFailure("validation: title required".into()),
                EditResponse::EnvFailure(ProcError::SpawnFailed),
            ],
            vec![],
        );
        let outcome = fake
            .apply_edit(&edit(1), "2026-05-19T00:00:00.000Z")
            .unwrap();
        let BackendEditOutcome::Rejected(failure) = outcome else {
            panic!("expected rejection")
        };
        assert_eq!(failure.detail, "validation: title required");
        let err = fake.apply_edit(&edit(2), "2026-05-19T00:00:00.000Z");
        assert!(matches!(err, Err(ProcError::SpawnFailed)));
        assert_eq!(fake.captured_edits.len(), 2);
    }

    #[test]
    fn creation_scripts_all_three_certainty_outcomes() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![],
            vec![
                CreateResponse::Rejected("preflight validation".into()),
                CreateResponse::Indeterminate("connection lost".into()),
            ],
        );
        assert!(matches!(
            fake.create_item(&create(1), "now"),
            BackendCreateOutcome::Rejected(_)
        ));
        assert!(matches!(
            fake.create_item(&create(2), "now"),
            BackendCreateOutcome::Indeterminate(_)
        ));
        assert_eq!(fake.captured_creates.len(), 2);
    }

    #[test]
    fn defaults_to_no_promotion_capabilities() {
        let fake = FakeAdapter::directional(vec![], vec![], vec![], vec![]);
        assert_eq!(fake.promotion_capabilities(), PromotionCapabilities::none());
    }

    #[test]
    fn with_capabilities_overrides_the_declaration() {
        let caps = PromotionCapabilities::none().with_item_class(ItemClass::Epic);
        let fake = FakeAdapter::directional(vec![], vec![], vec![], vec![]).with_capabilities(caps);
        assert_eq!(fake.promotion_capabilities(), caps);
    }
}
