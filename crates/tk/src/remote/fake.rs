//! Script-queue Backend Adapter for engine and command tests.
//!
//! Each script entry is consumed in order; an exhausted script panics so a
//! test that forgot to declare an interaction fails loudly instead of getting
//! a silent default. Script responses are moved from their queues; captured
//! calls own the input fields needed for assertions.

use std::collections::VecDeque;

use crate::domain::apply_outcome::ApplyOutcome;
use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{AdoptedItem, BackendItemRefresh};
use crate::domain::mutation_type::MutationType;
use crate::domain::mutation_view::MutationView;
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

/// Scripted response for one [`Adapter::apply_mutation`] call.
#[derive(Debug)]
pub enum ApplyResponse {
    /// Mutation accepted — returns [`ApplyOutcome::Accepted`] with a plain
    /// acknowledgement Receipt.
    Success,
    /// Promotion accepted — returns [`ApplyOutcome::Accepted`] with a Promotion
    /// Receipt carrying this backend key and Display ID, the identity the
    /// Adapter owns for the object it created (ADR-0036).
    PromotionSuccess {
        backend_key: String,
        display_id: String,
    },
    /// Mutation rejected — returns [`ApplyOutcome::Rejected`] carrying this detail.
    RecordedFailure(String),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Recorded `apply_mutation` invocation captured for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCall {
    pub sequence: i64,
    pub mutation_type: MutationType,
    pub item_id: String,
    /// JSON-stringified payload variant, identical to what the outbox wrote.
    pub payload_text: String,
    /// Backend identity the engine resolved for this Mutation. Recorded so a
    /// test can tell a Mutation that saw a preceding Promotion receipt from one
    /// that was handed a still-Local Item.
    pub backend_key: Option<String>,
    /// Backend identity the engine resolved for a Dependency Mutation's
    /// Blocking Item, recorded for the same reason `backend_key` is: the
    /// counterpart's Promotion is a separate receipt, applied earlier in the
    /// same run.
    pub counterpart_backend_key: Option<String>,
}

/// Strict, script-queue Backend Adapter for tests.
///
/// `adopt_script`, `refresh_script`, and `apply_script` are consumed in order.
/// Overflowing any script panics so a test that under-declared its interactions
/// fails loudly.
pub struct FakeAdapter {
    adopt_script: VecDeque<AdoptResponse>,
    refresh_script: VecDeque<RefreshResponse>,
    apply_script: VecDeque<ApplyResponse>,
    /// Recorded apply invocations in call order — populated on every path,
    /// including rejection and environment failure.
    pub captured_applies: Vec<ApplyCall>,
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
        apply_script: Vec<ApplyResponse>,
    ) -> Self {
        Self {
            adopt_script: adopt_script.into(),
            refresh_script: refresh_script.into(),
            apply_script: apply_script.into(),
            captured_applies: Vec::new(),
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

    fn apply_mutation(
        &mut self,
        view: &MutationView,
        _now: &str,
    ) -> Result<ApplyOutcome, ApplyError> {
        // Record before consulting the script so the rejection and env-failure
        // paths still leave evidence in `captured_applies`.
        self.captured_applies.push(ApplyCall {
            sequence: view.sequence,
            mutation_type: view.mutation_type,
            item_id: view.item_id.clone(),
            payload_text: view.payload.to_json_string(),
            backend_key: view.backend_key.clone(),
            counterpart_backend_key: view.counterpart_backend_key.clone(),
        });

        let response = self
            .apply_script
            .pop_front()
            .expect("FakeAdapter: apply script exhausted");
        match response {
            ApplyResponse::Success => Ok(ApplyOutcome::accepted()),
            ApplyResponse::PromotionSuccess {
                backend_key,
                display_id,
            } => Ok(ApplyOutcome::promoted(backend_key, display_id)),
            ApplyResponse::RecordedFailure(detail) => Ok(ApplyOutcome::rejected(detail)),
            ApplyResponse::EnvFailure(err) => Err(err),
        }
    }

    fn promotion_capabilities(&self) -> PromotionCapabilities {
        self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::apply_outcome::{ApplyOutcome, Receipt};
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{
        DependencyRef, EpicRef, MutationPayload, Promotion, StatusChange, TitleBody,
    };
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

    fn view(sequence: i64, mutation_type: MutationType, payload: MutationPayload) -> MutationView {
        MutationView {
            sequence,
            mutation_type,
            item_id: "t1".into(),
            item_class: ItemClass::Ticket,
            payload,
            backend_kind: Some("github".into()),
            backend_key: Some("1".into()),
            counterpart_backend_key: None,
        }
    }

    #[test]
    fn adopt_returns_scripted_item_and_captures_input() {
        let mut fake = FakeAdapter::directional(
            vec![AdoptResponse::Item(adopted_item("1", "gh-1"))],
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
        );
        assert_eq!(fake.refresh_item("1").unwrap().title, "First");
        assert!(fake.refresh_item("2").is_err());
        assert_eq!(fake.captured_refresh_keys.len(), 2);
    }

    #[test]
    fn apply_success_returns_success_outcome() {
        let mut fake = FakeAdapter::directional(vec![], vec![], vec![ApplyResponse::Success]);
        let outcome = fake
            .apply_mutation(
                &view(
                    1,
                    MutationType::UpdateTicket,
                    MutationPayload::UpdateTitleBody(TitleBody {
                        title: "T".into(),
                        body: "B".into(),
                    }),
                ),
                "2026-05-19T00:00:00.000Z",
            )
            .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Accepted(_)));
    }

    #[test]
    fn apply_promotion_success_returns_the_scripted_receipt() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![ApplyResponse::PromotionSuccess {
                backend_key: "42".into(),
                display_id: "gh-42".into(),
            }],
        );
        let outcome = fake
            .apply_mutation(
                &view(
                    1,
                    MutationType::PromoteTicket,
                    MutationPayload::Promotion(Promotion {
                        title: "T".into(),
                        body: "B".into(),
                        backend_kind: "github".into(),
                    }),
                ),
                "2026-05-19T00:00:00.000Z",
            )
            .unwrap();
        match outcome {
            ApplyOutcome::Accepted(Receipt::Promotion(receipt)) => {
                assert_eq!(receipt.backend_key, "42");
                assert_eq!(receipt.display_id, "gh-42");
            }
            other => panic!("expected a Promotion receipt, got {other:?}"),
        }
    }

    #[test]
    fn apply_records_call_with_payload() {
        let mut fake = FakeAdapter::directional(vec![], vec![], vec![ApplyResponse::Success]);
        fake.apply_mutation(
            &view(
                7,
                MutationType::UpdateTicket,
                MutationPayload::UpdateTitleBody(TitleBody {
                    title: "T".into(),
                    body: "B".into(),
                }),
            ),
            "2026-05-19T00:00:00.000Z",
        )
        .unwrap();
        assert_eq!(fake.captured_applies.len(), 1);
        let recorded = &fake.captured_applies[0];
        assert_eq!(recorded.sequence, 7);
        assert_eq!(recorded.mutation_type, MutationType::UpdateTicket);
        assert_eq!(recorded.item_id, "t1");
        assert!(recorded.payload_text.contains(r#""title":"T""#));
        assert!(recorded.payload_text.contains(r#""body":"B""#));
    }

    #[test]
    fn apply_recorded_failure_returns_failure_outcome() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![ApplyResponse::RecordedFailure(
                "validation: title required".into(),
            )],
        );
        let outcome = fake
            .apply_mutation(
                &view(
                    3,
                    MutationType::SetItemStatus,
                    MutationPayload::ItemStatus(StatusChange {
                        status: "done".into(),
                    }),
                ),
                "2026-05-19T00:00:00.000Z",
            )
            .unwrap();
        match outcome {
            ApplyOutcome::Rejected(f) => assert_eq!(f.detail, "validation: title required"),
            ApplyOutcome::Accepted(_) => panic!("expected Failure"),
        }
        // Rejection still records evidence.
        assert_eq!(fake.captured_applies.len(), 1);
        assert_eq!(fake.captured_applies[0].sequence, 3);
    }

    #[test]
    fn apply_env_failure_returns_bare_error_and_records_call() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![ApplyResponse::EnvFailure(ProcError::SpawnFailed)],
        );
        let err = fake.apply_mutation(
            &view(
                1,
                MutationType::UpdateTicket,
                MutationPayload::UpdateTitleBody(TitleBody {
                    title: "T".into(),
                    body: "B".into(),
                }),
            ),
            "2026-05-19T00:00:00.000Z",
        );
        assert!(matches!(err, Err(ProcError::SpawnFailed)));
        // env-failure path still records evidence.
        assert_eq!(fake.captured_applies.len(), 1);
        assert_eq!(fake.captured_applies[0].sequence, 1);
    }

    #[test]
    fn apply_records_epic_ref_payload_as_json() {
        let mut fake = FakeAdapter::directional(vec![], vec![], vec![ApplyResponse::Success]);
        fake.apply_mutation(
            &view(
                2,
                MutationType::AddTicketToEpic,
                MutationPayload::EpicRef(EpicRef {
                    epic_id: "e-internal".into(),
                }),
            ),
            "2026-05-19T00:00:00.000Z",
        )
        .unwrap();
        assert!(
            fake.captured_applies[0]
                .payload_text
                .contains(r#""epic_id":"e-internal""#)
        );
    }

    #[test]
    fn apply_records_dependency_ref_payload_as_json() {
        let mut fake = FakeAdapter::directional(vec![], vec![], vec![ApplyResponse::Success]);
        fake.apply_mutation(
            &view(
                4,
                MutationType::AddDependency,
                MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "b-internal".into(),
                }),
            ),
            "2026-05-19T00:00:00.000Z",
        )
        .unwrap();
        assert!(
            fake.captured_applies[0]
                .payload_text
                .contains(r#""blocking_id":"b-internal""#)
        );
    }

    #[test]
    fn apply_advances_script_across_calls() {
        let mut fake = FakeAdapter::directional(
            vec![],
            vec![],
            vec![
                ApplyResponse::Success,
                ApplyResponse::RecordedFailure("second call failed".into()),
            ],
        );
        let first = fake
            .apply_mutation(
                &view(
                    1,
                    MutationType::UpdateTicket,
                    MutationPayload::UpdateTitleBody(TitleBody {
                        title: "A".into(),
                        body: String::new(),
                    }),
                ),
                "2026-05-19T00:00:00.000Z",
            )
            .unwrap();
        assert!(matches!(first, ApplyOutcome::Accepted(_)));
        let second = fake
            .apply_mutation(
                &view(
                    2,
                    MutationType::UpdateTicket,
                    MutationPayload::UpdateTitleBody(TitleBody {
                        title: "B".into(),
                        body: String::new(),
                    }),
                ),
                "2026-05-19T00:00:00.000Z",
            )
            .unwrap();
        match second {
            ApplyOutcome::Rejected(f) => assert_eq!(f.detail, "second call failed"),
            ApplyOutcome::Accepted(_) => panic!("expected Failure"),
        }
        assert_eq!(fake.captured_applies.len(), 2);
        assert_eq!(fake.captured_applies.len(), 2);
    }

    #[test]
    fn defaults_to_no_promotion_capabilities() {
        let fake = FakeAdapter::directional(vec![], vec![], vec![]);
        assert_eq!(fake.promotion_capabilities(), PromotionCapabilities::none());
    }

    #[test]
    fn with_capabilities_overrides_the_declaration() {
        let caps = PromotionCapabilities::none().with_item_class(ItemClass::Epic);
        let fake = FakeAdapter::directional(vec![], vec![], vec![]).with_capabilities(caps);
        assert_eq!(fake.promotion_capabilities(), caps);
    }
}
