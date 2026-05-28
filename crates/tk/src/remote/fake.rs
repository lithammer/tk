//! Script-queue Backend Adapter for engine and command tests.
//!
//! Each script entry is consumed in order; an exhausted script panics so a
//! test that forgot to declare an interaction fails loudly instead of getting
//! a silent default. Responses own their data, so `apply_mutation` records
//! calls by cloning into [`FakeAdapter::captured_applies`].

use crate::domain::apply_outcome::ApplyOutcome;
use crate::domain::backend_item_snapshot::BackendItemSnapshot;
use crate::domain::mutation_type::MutationType;
use crate::domain::mutation_view::MutationView;
use crate::proc::ProcError;

use super::adapter::{Adapter, ApplyError, PullError};

/// Scripted response for one [`Adapter::pull_backend_items`] call.
#[derive(Debug, Clone)]
pub enum PullResponse {
    /// Success — the fake returns this snapshot list.
    Snapshots(Vec<BackendItemSnapshot>),
    /// Adapter-level rejection — returns [`PullError::Failed`] carrying this
    /// detail.
    RecordedFailure(String),
    /// Environment failure — returns this bare error tag.
    EnvFailure(ProcError),
}

/// Scripted response for one [`Adapter::apply_mutation`] call.
#[derive(Debug, Clone)]
pub enum ApplyResponse {
    /// Mutation accepted — returns [`ApplyOutcome::Accepted`] with an empty Receipt.
    Success,
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
}

/// Strict, script-queue Backend Adapter for tests.
///
/// `pull_script` and `apply_script` are consumed in order. Overflowing either
/// panics; the panic surfaces a test that under-declared its interactions.
pub struct FakeAdapter {
    pull_script: Vec<PullResponse>,
    apply_script: Vec<ApplyResponse>,
    pub pull_index: usize,
    pub apply_index: usize,
    /// Recorded apply invocations in call order — populated on every path,
    /// including rejection and environment failure.
    pub captured_applies: Vec<ApplyCall>,
}

impl FakeAdapter {
    #[must_use]
    pub fn new(pull_script: Vec<PullResponse>, apply_script: Vec<ApplyResponse>) -> Self {
        Self {
            pull_script,
            apply_script,
            pull_index: 0,
            apply_index: 0,
            captured_applies: Vec::new(),
        }
    }
}

impl Adapter for FakeAdapter {
    fn pull_backend_items(&mut self) -> Result<Vec<BackendItemSnapshot>, PullError> {
        let response = self
            .pull_script
            .get(self.pull_index)
            .expect("FakeAdapter: pull script exhausted")
            .clone();
        self.pull_index += 1;
        match response {
            PullResponse::Snapshots(s) => Ok(s),
            PullResponse::RecordedFailure(detail) => Err(PullError::Failed(detail)),
            PullResponse::EnvFailure(err) => Err(PullError::Env(err)),
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
        });

        let response = self
            .apply_script
            .get(self.apply_index)
            .expect("FakeAdapter: apply script exhausted")
            .clone();
        self.apply_index += 1;
        match response {
            ApplyResponse::Success => Ok(ApplyOutcome::accepted()),
            ApplyResponse::RecordedFailure(detail) => Ok(ApplyOutcome::rejected(detail)),
            ApplyResponse::EnvFailure(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::apply_outcome::ApplyOutcome;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{
        DependencyRef, EpicRef, MutationPayload, StatusChange, TitleBody,
    };
    use crate::domain::status::ItemStatus;
    use crate::domain::ticket_kind::TicketKind;

    fn snapshot(backend_key: &str, display_id: &str, class: ItemClass) -> BackendItemSnapshot {
        BackendItemSnapshot {
            backend_kind: "github".into(),
            backend_key: backend_key.into(),
            display_id: display_id.into(),
            item_class: class,
            ticket_kind: if class == ItemClass::Ticket {
                Some(TicketKind::Task)
            } else {
                None
            },
            title: "Title".into(),
            body: "Body".into(),
            status: ItemStatus::Open,
            backend_updated_at: "2026-05-19T00:00:00Z".into(),
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
        }
    }

    #[test]
    fn pull_returns_scripted_snapshots() {
        let mut fake = FakeAdapter::new(
            vec![PullResponse::Snapshots(vec![
                snapshot("1", "gh-1", ItemClass::Ticket),
                snapshot("2", "gh-2", ItemClass::Epic),
            ])],
            vec![],
        );
        let got = fake.pull_backend_items().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].display_id, "gh-1");
        assert_eq!(got[0].ticket_kind, Some(TicketKind::Task));
        assert_eq!(got[1].item_class, ItemClass::Epic);
        assert_eq!(got[1].ticket_kind, None);
    }

    #[test]
    fn pull_returns_empty_snapshot_list() {
        let mut fake = FakeAdapter::new(vec![PullResponse::Snapshots(vec![])], vec![]);
        assert!(fake.pull_backend_items().unwrap().is_empty());
    }

    #[test]
    fn pull_recorded_failure_returns_failed_with_detail() {
        let mut fake = FakeAdapter::new(
            vec![PullResponse::RecordedFailure("gh: HTTP 502".into())],
            vec![],
        );
        let err = fake.pull_backend_items().unwrap_err();
        match err {
            PullError::Failed(detail) => assert!(detail.contains("HTTP 502")),
            PullError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
    }

    #[test]
    fn pull_env_failure_returns_bare_error() {
        let mut fake = FakeAdapter::new(
            vec![PullResponse::EnvFailure(ProcError::ExecutableNotFound)],
            vec![],
        );
        assert!(matches!(
            fake.pull_backend_items().unwrap_err(),
            PullError::Env(ProcError::ExecutableNotFound)
        ));
    }

    #[test]
    fn pull_advances_script_across_calls() {
        let mut fake = FakeAdapter::new(
            vec![
                PullResponse::Snapshots(vec![snapshot("1", "gh-1", ItemClass::Ticket)]),
                PullResponse::EnvFailure(ProcError::ExecutableNotFound),
            ],
            vec![],
        );
        assert_eq!(fake.pull_backend_items().unwrap().len(), 1);
        assert!(fake.pull_backend_items().is_err());
        assert_eq!(fake.pull_index, 2);
    }

    #[test]
    fn apply_success_returns_success_outcome() {
        let mut fake = FakeAdapter::new(vec![], vec![ApplyResponse::Success]);
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
    fn apply_records_call_with_payload() {
        let mut fake = FakeAdapter::new(vec![], vec![ApplyResponse::Success]);
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
        let mut fake = FakeAdapter::new(
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
        let mut fake = FakeAdapter::new(
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
        let mut fake = FakeAdapter::new(vec![], vec![ApplyResponse::Success]);
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
        let mut fake = FakeAdapter::new(vec![], vec![ApplyResponse::Success]);
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
        let mut fake = FakeAdapter::new(
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
        assert_eq!(fake.apply_index, 2);
        assert_eq!(fake.captured_applies.len(), 2);
    }
}
