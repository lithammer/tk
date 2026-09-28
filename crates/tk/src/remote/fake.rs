//! Script-queue Backend Adapter for engine and command tests.
//!
//! Each script entry is consumed in order; an exhausted script panics so a
//! test that forgot to declare an interaction fails loudly instead of getting
//! a silent default. Script responses are moved from their queues; captured
//! calls own the input fields needed for assertions.

use std::collections::VecDeque;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemAddress, BackendItemIdentity,
    BackendItemInspection, BackendItemRefresh, BackendPull, BackendPullItem,
};
use crate::domain::backend_outcome::{BackendCreateOutcome, BackendEditOutcome};
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::proc::ProcError;

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// Scripted response for one [`Adapter::pull`] call.
#[derive(Debug)]
pub enum PullResponse {
    /// Success — the fake pairs these fields with the requested working set.
    Items(Vec<BackendItemRefresh>),
    /// Adapter-level rejection with this detail.
    RecordedFailure(String),
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
    pull_script: VecDeque<PullResponse>,
    inspection_script: VecDeque<BackendItemInspection>,
    edit_script: VecDeque<EditResponse>,
    create_script: VecDeque<CreateResponse>,
    /// Recorded edit invocations in call order — populated on every path,
    /// including rejection and environment failure.
    pub captured_edits: Vec<BackendEdit>,
    /// Recorded creation invocations in call order.
    pub captured_creates: Vec<BackendCreate>,
    /// Complete Backend key sets passed to Pull, in call order.
    pub captured_pull_keys: Vec<Vec<String>>,
    /// Backend keys passed to `inspect_item`, in call order.
    pub captured_inspection_keys: Vec<String>,
    /// This fake's resolved capability value. Static data, not a script entry,
    /// so tests set it once via
    /// [`FakeAdapter::with_capabilities`] instead of queuing a response per
    /// call.
    capabilities: PromotionCapabilities,
    capability_error: Option<AdapterReadError>,
}

impl FakeAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pull_script: VecDeque::new(),
            inspection_script: VecDeque::new(),
            edit_script: VecDeque::new(),
            create_script: VecDeque::new(),
            captured_edits: Vec::new(),
            captured_creates: Vec::new(),
            captured_pull_keys: Vec::new(),
            captured_inspection_keys: Vec::new(),
            capabilities: PromotionCapabilities::none(),
            capability_error: None,
        }
    }

    #[must_use]
    pub fn with_pulls(mut self, script: Vec<PullResponse>) -> Self {
        self.pull_script = script.into();
        self
    }

    #[must_use]
    pub fn with_inspections(mut self, script: Vec<BackendItemInspection>) -> Self {
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

    /// Make the next capability resolution fail with this Adapter read error.
    #[must_use]
    pub fn with_capability_error(mut self, error: AdapterReadError) -> Self {
        self.capability_error = Some(error);
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

    fn adopt_ticket(&mut self, _input: &str) -> Result<AdoptedItem, AdapterReadError> {
        panic!("FakeAdapter: unexpected Adopt call");
    }

    fn pull(&mut self, items: &[BackendItemAddress]) -> Result<BackendPull, AdapterReadError> {
        self.captured_pull_keys
            .push(items.iter().map(|item| item.backend_key.clone()).collect());
        let response = self
            .pull_script
            .pop_front()
            .expect("FakeAdapter: pull script exhausted");
        let refreshes = match response {
            PullResponse::Items(refreshes) => refreshes,
            PullResponse::RecordedFailure(detail) => {
                return Err(AdapterReadError::Failed(detail));
            }
        };
        assert_eq!(
            refreshes.len(),
            items.len(),
            "FakeAdapter: scripted Pull must cover the complete working set"
        );
        let pulled = items
            .iter()
            .cloned()
            .zip(refreshes)
            .map(|(address, refresh)| BackendPullItem { address, refresh })
            .collect();
        Ok(BackendPull::new(items, pulled)
            .expect("FakeAdapter pairs each scripted refresh with its requested key"))
    }

    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError> {
        self.captured_inspection_keys.push(key.to_string());
        Ok(self
            .inspection_script
            .pop_front()
            .expect("FakeAdapter: inspection script exhausted"))
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
        if let Some(error) = self.capability_error.take() {
            return Err(error);
        }
        Ok(self.capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemAddress;
    use crate::domain::lifecycle::Lifecycle;
    use crate::domain::ticket_kind::TicketKind;

    fn refresh(title: &str) -> BackendItemRefresh {
        BackendItemRefresh {
            title: title.into(),
            body: "Body".into(),
            status: Lifecycle::Open,
            ticket_kind: Some(TicketKind::Task),
        }
    }

    fn address(key: &str) -> BackendItemAddress {
        BackendItemAddress {
            backend_key: key.into(),
        }
    }

    #[test]
    fn pull_advances_script_across_calls() {
        let mut fake = FakeAdapter::new().with_pulls(vec![
            PullResponse::Items(vec![refresh("First")]),
            PullResponse::Items(vec![refresh("Second")]),
        ]);
        let first = fake
            .pull(&[address("1")])
            .unwrap()
            .into_refreshes()
            .pop()
            .unwrap()
            .1;
        assert_eq!(first.title, "First");
        let second = fake
            .pull(&[address("2")])
            .unwrap()
            .into_refreshes()
            .pop()
            .unwrap()
            .1;
        assert_eq!(second.title, "Second");
        assert_eq!(fake.captured_pull_keys.len(), 2);
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
    fn capability_error_fails_the_next_resolution() {
        let mut fake = FakeAdapter::new()
            .with_capability_error(AdapterReadError::Failed("taxonomy read failed".into()));

        assert!(matches!(
            fake.resolve_promotion_capabilities(PromotionRequirements::none()),
            Err(AdapterReadError::Failed(detail)) if detail == "taxonomy read failed"
        ));
        assert_eq!(
            fake.resolve_promotion_capabilities(PromotionRequirements::none())
                .unwrap(),
            PromotionCapabilities::none()
        );
    }
}
