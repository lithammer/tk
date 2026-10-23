//! GitHub Backend Adapter over the `gh` CLI (ADR-0021 fields, ADR-0034 opt-in,
//! ADR-0054 native Issue Types and issue fields).
//!
//! Implements [`Adapter`] by shelling out to `gh` through an injected
//! [`ProcRunner`] (ADR-0031: the real subprocess only in production, a
//! `FakeRunner` in tests). Creation and bare Adopt input let `gh` resolve the
//! repository from the command cwd; canonical issue URLs then pin existing
//! Item operations to their repository (ADR-0033). Creation and Priority
//! edits go through GraphQL, because `gh` has no issue field support.
//!
//! Pull is exact-key batching: the engine hands the Adapter the Adopted working
//! set, which the Adapter groups by host and reads through bounded GraphQL
//! operations. There is no issue listing or discovery (ADR-0034).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemAddress, BackendItemIdentity,
    BackendItemInspection, BackendItemRefresh, BackendPull, BackendPullItem,
};
use crate::domain::backend_outcome::{
    BackendCreateOutcome, BackendEditOutcome, Failure, FailureClass,
};
use crate::domain::lifecycle::Lifecycle;
use crate::domain::priority::Priority;
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::domain::ticket_kind::TicketKind;
use crate::proc::{ProcRunner, RunOutput};

use super::adapter::{Adapter, AdapterReadError, ApplyError};

mod graphql;
mod priority;

use graphql::cli::CliGraphqlTransport;
#[cfg(test)]
use graphql::{CREATE_ISSUE_QUERY, GraphqlExchange, GraphqlRequest, ISSUE_TYPES_QUERY};
use graphql::{
    CreateIssueData, CreateIssueOperation, GraphqlCompleted, GraphqlEnvelope, GraphqlError,
    GraphqlObservation, GraphqlOperation, GraphqlStartFailure, GraphqlTransport, IssueFieldValues,
    IssueFieldsOperation, IssueFieldsResponse, IssueTypePageResponse, IssueTypesField,
    IssueTypesOperation, MAX_PULL_KEYS_PER_QUERY, PullData, PullObject, PullOperation,
    PullRepository, PullTarget, SetIssueFieldValueOperation,
};
use priority::PriorityField;

/// Encode a one-item request through the production Pull operation.
#[cfg(test)]
pub(crate) fn single_pull_request_body(owner: &str, name: &str, number: i64) -> Vec<u8> {
    let targets = [PullTarget {
        input_index: 0,
        owner: owner.into(),
        name: name.into(),
        number,
    }];
    PullOperation::new("github.com", &targets).request().body()
}

/// Encode the first Issue Type page request through the production operation.
#[cfg(test)]
pub(crate) fn issue_types_request_body(owner: &str, name: &str) -> Vec<u8> {
    IssueTypesOperation::new("github.com", owner, name, None)
        .request()
        .body()
}

/// Encode the issue field request through the production operation.
#[cfg(test)]
pub(crate) fn issue_fields_request_body(owner: &str, name: &str) -> Vec<u8> {
    IssueFieldsOperation::new("github.com", owner, name)
        .request()
        .body()
}

/// Encode a `createIssue` request for a Ticket with its Priority resolved.
#[cfg(test)]
pub(crate) fn create_issue_request_body(
    repository_id: &str,
    title: &str,
    body: &str,
    issue_type_id: Option<&str>,
    priority: Option<(&str, &str)>,
) -> Vec<u8> {
    CreateIssueOperation::new(
        "github.com",
        repository_id,
        title,
        body,
        issue_type_id,
        priority.map(|(field_id, option_id)| graphql::FieldValue {
            field_id,
            option_id,
        }),
    )
    .request()
    .body()
}

/// The `--json` field set tk requests from `gh issue view`. `url` supplies the
/// canonical Adopt and Promotion-recovery identity and rejects pull requests
/// (see [`is_pull_request_url`]); `state` arrives UPPERCASE; `issueType` is an
/// object-or-null; `id` is the node ID that issue field mutations address.
const ISSUE_JSON_FIELDS: &str = "id,number,title,body,state,issueType,url";
const REPOSITORY_JSON_FIELDS: &str = "id,nameWithOwner,url";

/// Issue Type names Promotion requires, matched case-insensitively (ADR-0054).
const BUG_ISSUE_TYPE: &str = "Bug";
const EPIC_ISSUE_TYPE: &str = "Epic";

/// GitHub Backend Adapter. `gh` resolves bare inputs from the command cwd
/// (ADR-0033).
pub struct GithubAdapter<'a> {
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
    graphql: Box<dyn GraphqlTransport + 'a>,
}

impl<'a> GithubAdapter<'a> {
    #[must_use]
    pub fn new(runner: &'a dyn ProcRunner, cwd: &'a Path) -> Self {
        Self {
            runner,
            cwd,
            graphql: Box::new(CliGraphqlTransport::new(runner, cwd)),
        }
    }

    #[cfg(test)]
    fn with_graphql_transport(
        runner: &'a dyn ProcRunner,
        cwd: &'a Path,
        graphql: Box<dyn GraphqlTransport + 'a>,
    ) -> Self {
        Self {
            runner,
            cwd,
            graphql,
        }
    }
}

impl Adapter for GithubAdapter<'_> {
    fn backend_kind(&self) -> BackendKind {
        BackendKind::Github
    }

    fn adopt_ticket(&mut self, input: &str) -> Result<AdoptedItem, AdapterReadError> {
        let issue = self.view_issue(input)?;
        let identity = issue.validated_identity()?;
        let ticket_kind = classify_ticket_kind(&issue);
        issue.into_adopted_item(identity, ticket_kind)
    }

    fn pull(&mut self, items: &[BackendItemAddress]) -> Result<BackendPull, AdapterReadError> {
        self.pull_items(items)
    }

    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError> {
        let issue = self.view_matching_issue(key)?;
        let identity = issue.validated_identity()?;
        let ticket_kind = classify_ticket_kind(&issue);
        Ok(issue.into_inspection(identity, ticket_kind))
    }

    fn apply_edit(&mut self, edit: &BackendEdit) -> Result<BackendEditOutcome, ApplyError> {
        match edit {
            BackendEdit::UpdateTicket {
                ticket: item,
                snapshot,
            }
            | BackendEdit::UpdateEpic {
                epic: item,
                snapshot,
            } => self.run_edit(&[
                "gh",
                "issue",
                "edit",
                &item.backend_key,
                "--title",
                &snapshot.title,
                "--body",
                &snapshot.body,
            ]),
            BackendEdit::SetTicketPriority { ticket, change } => {
                self.set_ticket_priority(&ticket.backend_key, change.priority)
            }
            BackendEdit::SetItemStatus { item, change, .. } => match change.status {
                Lifecycle::Done => self.run_edit(&["gh", "issue", "close", &item.backend_key]),
                // v1 has no remote reopen: `done` is terminal (ADR-0006), and
                // ADR-0046's narrow Sync Skip exception restores `open` locally
                // without ever asking a Backend Adapter to reopen anything —
                // "this is not general remote-reopen support". An `open` target
                // therefore has no `gh` verb to run.
                Lifecycle::Open => Ok(BackendEditOutcome::rejected(
                    "cannot push a target Lifecycle of 'open': v1 has no remote reopen",
                )),
            },
            // Relationship sync (ADR-0021, tk-107 Dependencies, tk-132 Epic
            // membership): every arm edits the Mutation's own item, and any
            // counterpart address is resolved store-side onto the operation's
            // identity. All four flags below are native to `gh issue edit` as of
            // `gh` 2.94.0, which the adapter therefore requires; an older `gh`
            // rejects the unknown flag and the Mutation fails.
            BackendEdit::AddDependency {
                blocked, blocking, ..
            } => self.run_edit(&[
                "gh",
                "issue",
                "edit",
                &blocked.backend_key,
                "--add-blocked-by",
                &blocking.backend_key,
            ]),
            BackendEdit::RemoveDependency {
                blocked, blocking, ..
            } => self.run_edit(&[
                "gh",
                "issue",
                "edit",
                &blocked.backend_key,
                "--remove-blocked-by",
                &blocking.backend_key,
            ]),
            BackendEdit::AddTicketToEpic { ticket, epic, .. } => self.run_edit(&[
                "gh",
                "issue",
                "edit",
                &ticket.backend_key,
                "--parent",
                &epic.backend_key,
            ]),
            // `--remove-parent` clears whichever parent the issue actually has,
            // which is what makes the push converge on tk's cleared
            // `container_id`. The `--remove-sub-issue <ticket>` form names the
            // Epic tk expected and was observed to exit 0 without changing a
            // divergent parent, reporting a removal that never happened.
            BackendEdit::RemoveTicketFromEpic { ticket } => self.run_edit(&[
                "gh",
                "issue",
                "edit",
                &ticket.backend_key,
                "--remove-parent",
            ]),
        }
    }

    fn create_item(&mut self, create: &BackendCreate) -> BackendCreateOutcome {
        self.create_issue(create)
    }

    fn resolve_promotion_capabilities(
        &mut self,
        requirements: PromotionRequirements,
    ) -> Result<PromotionCapabilities, AdapterReadError> {
        use crate::domain::item_class::ItemClass;

        let mut capabilities = PromotionCapabilities::none();
        if requirements.requires_item_class(ItemClass::Ticket) {
            self.require_priority_field()?;
            capabilities = capabilities.with_item_class(ItemClass::Ticket);
        }
        if requirements.requires_item_class(ItemClass::Epic) {
            self.require_issue_type(EPIC_ISSUE_TYPE)?;
            capabilities = capabilities.with_item_class(ItemClass::Epic);
        }
        if requirements.requires_ticket_kind(TicketKind::Task) {
            capabilities = capabilities.with_ticket_kind(TicketKind::Task);
        }
        if requirements.requires_ticket_kind(TicketKind::Bug) {
            self.require_issue_type(BUG_ISSUE_TYPE)?;
            capabilities = capabilities.with_ticket_kind(TicketKind::Bug);
        }
        if requirements.requires_dependencies() {
            capabilities = capabilities.with_dependencies();
        }
        if requirements.requires_epic_membership() {
            capabilities = capabilities.with_epic_membership();
        }
        Ok(capabilities)
    }
}

impl GithubAdapter<'_> {
    fn pull_items(
        &mut self,
        items: &[BackendItemAddress],
    ) -> Result<BackendPull, AdapterReadError> {
        let groups = plan_pull(items)?;
        let mut pulled = vec![None; items.len()];
        let mut item_errors = Vec::new();
        for group in groups {
            for targets in group.targets.chunks(MAX_PULL_KEYS_PER_QUERY) {
                let mut data = match self.pull_chunk(&group.host, targets) {
                    Ok(data) => data,
                    Err(PullChunkError::Items(errors)) => {
                        item_errors.extend(errors);
                        continue;
                    }
                    Err(PullChunkError::Request(error)) => return Err(error),
                };
                for target in targets {
                    let alias = target.alias();
                    let Some(repository) = data.items.remove(&alias) else {
                        item_errors.push(PullItemError {
                            input_index: target.input_index,
                            message: "GitHub GraphQL response omitted its Pull field".into(),
                        });
                        continue;
                    };
                    match decode_pull_item(&items[target.input_index], &target.owner, repository) {
                        Ok(item) => pulled[target.input_index] = Some(item),
                        Err(error) => item_errors.push(PullItemError {
                            input_index: target.input_index,
                            message: error.to_string(),
                        }),
                    }
                }
            }
        }
        if !item_errors.is_empty() {
            return Err(AdapterReadError::Failed(pull_item_error_detail(
                item_errors,
                items,
            )));
        }
        let pulled = pulled
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                item.ok_or_else(|| {
                    AdapterReadError::Failed(format!(
                        "GitHub Pull did not return requested key '{}' at index {index}",
                        items[index].backend_key
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        BackendPull::new(items, pulled).map_err(|error| AdapterReadError::Failed(error.to_string()))
    }

    fn pull_chunk(&self, host: &str, targets: &[PullTarget]) -> Result<PullData, PullChunkError> {
        let operation = PullOperation::new(host, targets);
        match graphql::execute(self.graphql.as_ref(), &operation) {
            GraphqlObservation::NotStarted(failure) => Err(PullChunkError::Request(
                AdapterReadError::Env(start_failure_as_proc_error(&failure)),
            )),
            GraphqlObservation::OutcomeUnobserved(_) => Err(PullChunkError::Request(
                AdapterReadError::Env(crate::proc::ProcError::OutcomeUnobserved),
            )),
            GraphqlObservation::Completed { exchange, envelope } => {
                let delivery_failure = exchange.completion.failure_detail().unwrap_or_default();
                let response = envelope.map_err(|error| {
                    if delivery_failure.is_empty() {
                        pull_request_error(
                            host,
                            format!("could not parse GitHub GraphQL response: {error}"),
                        )
                    } else {
                        pull_request_error(host, delivery_failure)
                    }
                })?;
                if !response.errors.is_empty() {
                    return Err(classify_pull_errors(response.errors, targets, host));
                }
                response.data.ok_or_else(|| {
                    pull_request_error(host, "GitHub GraphQL response contained no data")
                })
            }
        }
    }

    /// Run one edit through `gh` and map its outcome.
    ///
    /// A zero exit code is success even when stderr is not empty. `gh issue
    /// close` writes "is already closed" on a harmless repeat and exits zero.
    /// A nonzero exit rejects the Mutation with the stderr text and class.
    fn run_edit(&self, argv: &[&str]) -> Result<BackendEditOutcome, ApplyError> {
        let output = self.runner.run(argv, self.cwd)?;
        if output.succeeded() {
            return Ok(BackendEditOutcome::Acknowledged);
        }
        Ok(rejected_edit(&output))
    }

    /// Create the GitHub issue represented by either Promotion variant.
    ///
    /// Every creation is one GraphQL `createIssue` call, because `gh` cannot
    /// set an issue field and the type, fields, and creation must be one
    /// effect (ADR-0054). A Task is untyped; a Bug and an Epic carry their
    /// native type; a Ticket carries its Priority, an Epic none. A creation
    /// outcome is Indeterminate unless a canonical receipt proves it
    /// (ADR-0036), and Rejected only when a read before the mutation
    /// certifies no effect.
    fn create_issue(&self, create: &BackendCreate) -> BackendCreateOutcome {
        let (snapshot, type_name, priority) = match create {
            BackendCreate::Ticket {
                snapshot,
                ticket_kind: TicketKind::Task,
                priority,
            } => (snapshot, None, Some(*priority)),
            BackendCreate::Ticket {
                snapshot,
                ticket_kind: TicketKind::Bug,
                priority,
            } => (snapshot, Some(BUG_ISSUE_TYPE), Some(*priority)),
            BackendCreate::Epic { snapshot } => (snapshot, Some(EPIC_ISSUE_TYPE), None),
        };
        let repository = match self.current_repository() {
            Ok(repository) => repository,
            Err(error) => return BackendCreateOutcome::rejected(error.to_string()),
        };
        let (host, issue_type_id, priority_field) =
            match self.creation_taxonomy(&repository, type_name, priority) {
                Ok(taxonomy) => taxonomy,
                Err(error) => return BackendCreateOutcome::rejected(error.to_string()),
            };
        let operation = CreateIssueOperation::new(
            host,
            &repository.id,
            &snapshot.title,
            &snapshot.body,
            issue_type_id.as_deref(),
            priority_field
                .as_ref()
                .map(|(field, priority)| field.value(*priority)),
        );
        match graphql::execute(self.graphql.as_ref(), &operation) {
            GraphqlObservation::NotStarted(failure) => BackendCreateOutcome::Rejected(Failure {
                detail: format!("GraphQL request did not start: {}", failure.detail()),
                class: FailureClass::Unknown,
                retry_after_s: None,
            }),
            GraphqlObservation::OutcomeUnobserved(detail) => {
                BackendCreateOutcome::Indeterminate(Failure {
                    detail: format!(
                        "GraphQL request started, but its outcome is unknown: {detail}"
                    ),
                    class: FailureClass::Unknown,
                    retry_after_s: None,
                })
            }
            GraphqlObservation::Completed { exchange, envelope } => {
                Self::classify_creation(&exchange, envelope)
            }
        }
    }

    /// Resolve the issue type and Priority field a creation sets, reading the
    /// Backend before any mutation.
    fn creation_taxonomy<'r>(
        &self,
        repository: &'r RepositoryContext,
        type_name: Option<&str>,
        priority: Option<Priority>,
    ) -> Result<CreationTaxonomy<'r>, AdapterReadError> {
        let host = repository.host()?;
        let issue_type_id = type_name
            .map(|name| self.resolve_issue_type(repository, name))
            .transpose()?;
        let priority_field = match priority {
            Some(priority) => {
                let (owner, name) = repository.owner_and_name()?;
                Some((self.resolve_priority_field(host, owner, name)?, priority))
            }
            None => None,
        };
        Ok((host, issue_type_id, priority_field))
    }

    /// Replace the Priority on one issue. The field and option IDs and the
    /// issue's node ID come from reads, so a missing field rejects the
    /// Mutation with the named error before anything is written.
    fn set_ticket_priority(
        &self,
        key: &str,
        priority: Priority,
    ) -> Result<BackendEditOutcome, ApplyError> {
        let Some(address) = GithubIssueAddress::parse(key) else {
            return Ok(BackendEditOutcome::rejected(format!(
                "'{key}' is not a canonical GitHub issue URL"
            )));
        };
        match self.write_priority(&address, key, priority) {
            Ok(()) => Ok(BackendEditOutcome::Acknowledged),
            Err(AdapterReadError::Env(error)) => Err(error),
            Err(AdapterReadError::Failed(detail)) => Ok(BackendEditOutcome::Rejected(Failure {
                class: classify(&detail),
                detail,
                retry_after_s: None,
            })),
        }
    }

    fn write_priority(
        &self,
        address: &GithubIssueAddress<'_>,
        key: &str,
        priority: Priority,
    ) -> Result<(), AdapterReadError> {
        let field = self.resolve_priority_field(address.host, address.owner, address.repository)?;
        let issue_id = self.view_matching_issue(key)?.id.ok_or_else(|| {
            AdapterReadError::Failed(format!("gh returned no node ID for '{key}'"))
        })?;
        let response = self.graphql_read(&SetIssueFieldValueOperation::new(
            address.host,
            &issue_id,
            field.value(priority),
        ))?;
        response
            .set_issue_field_value
            .and_then(|payload| payload.issue)
            .map(drop)
            .ok_or_else(|| {
                AdapterReadError::Failed(format!(
                    "GitHub did not confirm the Priority change on '{key}'"
                ))
            })
    }

    fn classify_creation(
        exchange: &GraphqlCompleted,
        envelope: Result<GraphqlEnvelope<CreateIssueData>, String>,
    ) -> BackendCreateOutcome {
        let delivery_detail = exchange.completion.detail().to_owned();
        if let Ok(response) = envelope {
            if let Some(issue) = response
                .data
                .and_then(|data| data.create_issue)
                .and_then(|payload| payload.issue)
            {
                if let Some(identity) = parse_issue_url(&issue.url) {
                    return BackendCreateOutcome::Created(identity);
                }
            }
            let detail = if !response.errors.is_empty() {
                response
                    .errors
                    .into_iter()
                    .map(graphql::GraphqlError::into_message)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if delivery_detail.is_empty() {
                "GitHub GraphQL returned no issue URL receipt".into()
            } else {
                delivery_detail.clone()
            };
            return BackendCreateOutcome::Indeterminate(Failure {
                class: classify(&detail),
                detail,
                retry_after_s: None,
            });
        }
        BackendCreateOutcome::Indeterminate(Failure {
            class: classify(&delivery_detail),
            detail: if delivery_detail.is_empty() {
                "GitHub GraphQL returned an unrecognized response".into()
            } else {
                delivery_detail
            },
            retry_after_s: None,
        })
    }

    /// Fetch and parse one GitHub issue view by the caller's Backend key.
    fn view_issue(&self, key: &str) -> Result<GhIssue, AdapterReadError> {
        let output = self.runner.run(
            &["gh", "issue", "view", key, "--json", ISSUE_JSON_FIELDS],
            self.cwd,
        )?;
        if !output.succeeded() {
            return Err(AdapterReadError::Failed(stderr_string(&output)));
        }
        serde_json::from_slice(&output.stdout).map_err(|e| {
            AdapterReadError::Failed(format!("could not parse gh issue JSON for #{key}: {e}"))
        })
    }

    /// Fetch one issue and reject if `gh` returns another identity. Adopt
    /// remains the separate canonicalization entrypoint.
    fn view_matching_issue(&self, key: &str) -> Result<GhIssue, AdapterReadError> {
        let issue = self.view_issue(key)?;
        issue.validate_key(key)?;
        Ok(issue)
    }

    /// Read the repository that `gh` resolves from the Adapter command cwd.
    fn current_repository(&self) -> Result<RepositoryContext, AdapterReadError> {
        let output = self.runner.run(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            self.cwd,
        )?;
        if !output.succeeded() {
            return Err(AdapterReadError::Failed(stderr_string(&output)));
        }
        serde_json::from_slice(&output.stdout).map_err(|error| {
            AdapterReadError::Failed(format!(
                "could not parse GitHub repository metadata: {error}"
            ))
        })
    }

    /// Fail with the missing type's name and owner when the repository's
    /// account lacks it (ADR-0054).
    fn require_issue_type(&self, name: &str) -> Result<(), AdapterReadError> {
        let repository = self.current_repository()?;
        self.resolve_issue_type(&repository, name).map(drop)
    }

    /// Fail with the Priority field's name and owner when the repository's
    /// account lacks it or one of its four options (ADR-0054).
    fn require_priority_field(&self) -> Result<(), AdapterReadError> {
        let repository = self.current_repository()?;
        let (owner, name) = repository.owner_and_name()?;
        self.resolve_priority_field(repository.host()?, owner, name)
            .map(drop)
    }

    /// Read the account's issue fields and resolve Priority and its options.
    fn resolve_priority_field(
        &self,
        host: &str,
        owner: &str,
        name: &str,
    ) -> Result<PriorityField, AdapterReadError> {
        let response: IssueFieldsResponse =
            self.graphql_read(&IssueFieldsOperation::new(host, owner, name))?;
        let repository = response.repository.ok_or_else(|| {
            AdapterReadError::Failed(format!("GitHub returned no repository for {owner}/{name}"))
        })?;
        PriorityField::resolve(repository.issue_fields, owner)
    }

    /// Read every Issue Type page before concluding that no enabled, exact
    /// case-insensitive `name` exists.
    fn resolve_issue_type(
        &self,
        repository: &RepositoryContext,
        name: &str,
    ) -> Result<String, AdapterReadError> {
        let host = repository.host()?;
        let (owner, repo) = repository.owner_and_name()?;
        let missing = || {
            AdapterReadError::Failed(format!(
                "required issue type '{name}' is missing on {owner}"
            ))
        };
        let mut after = None;
        loop {
            let page: IssueTypePageResponse = self.graphql_read(&IssueTypesOperation::new(
                host,
                owner,
                repo,
                after.as_deref(),
            ))?;
            let repository = page.repository.ok_or_else(|| {
                AdapterReadError::Failed(format!(
                    "GitHub returned no repository for {owner}/{repo}"
                ))
            })?;
            let issue_types = match repository.issue_types {
                IssueTypesField::Connection(connection) => connection,
                IssueTypesField::Null(()) if after.is_none() => return Err(missing()),
                IssueTypesField::Null(()) => {
                    return Err(AdapterReadError::Failed(
                        "GitHub issue type pagination returned null after a continuation cursor"
                            .into(),
                    ));
                }
            };
            for issue_type in issue_types.nodes {
                if issue_type.is_enabled && issue_type.name.eq_ignore_ascii_case(name) {
                    return Ok(issue_type.id);
                }
            }
            if !issue_types.page_info.has_next_page {
                return Err(missing());
            }
            after = Some(issue_types.page_info.end_cursor.ok_or_else(|| {
                AdapterReadError::Failed(
                    "GitHub issue type pagination omitted its next cursor".into(),
                )
            })?);
        }
    }

    /// Run one typed GraphQL read without collapsing transport evidence.
    fn graphql_read<O: GraphqlOperation>(
        &self,
        operation: &O,
    ) -> Result<O::Response, AdapterReadError> {
        match graphql::execute(self.graphql.as_ref(), operation) {
            GraphqlObservation::NotStarted(failure) => {
                Err(AdapterReadError::Env(start_failure_as_proc_error(&failure)))
            }
            GraphqlObservation::OutcomeUnobserved(_) => Err(AdapterReadError::Env(
                crate::proc::ProcError::OutcomeUnobserved,
            )),
            GraphqlObservation::Completed { exchange, envelope } => {
                let delivery_failure = exchange.completion.failure_detail().unwrap_or_default();
                let response = envelope.map_err(|error| {
                    if delivery_failure.is_empty() {
                        AdapterReadError::Failed(format!(
                            "could not parse GitHub GraphQL response: {error}"
                        ))
                    } else {
                        AdapterReadError::Failed(delivery_failure.to_owned())
                    }
                })?;
                if !response.errors.is_empty() {
                    return Err(AdapterReadError::Failed(
                        response
                            .errors
                            .into_iter()
                            .map(graphql::GraphqlError::into_message)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ));
                }
                response.data.ok_or_else(|| {
                    AdapterReadError::Failed("GitHub GraphQL response contained no data".into())
                })
            }
        }
    }
}

/// GitHub Issue fields shared by native `gh issue view` and GraphQL Pull.
/// Serde ignores fields tk does not map.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: i64,
    title: String,
    body: String,
    /// UPPERCASE `OPEN`/`CLOSED` (raw GraphQL `IssueState`); a PR can also yield
    /// `MERGED`, but the `url` guard rejects PRs before state is mapped.
    state: String,
    #[serde(rename = "issueType")]
    issue_type: Option<GhIssueType>,
    /// Canonical issue/PR URL used for identity and the PR guard.
    url: String,
    /// Present only in `gh issue view`.
    #[serde(default)]
    id: Option<String>,
    /// Present only in a GraphQL Pull; `gh issue view` cannot return it.
    #[serde(default, rename = "issueFieldValues")]
    issue_field_values: Option<IssueFieldValues>,
}

#[derive(Debug, Deserialize)]
struct RepositoryContext {
    id: String,
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    url: String,
}

impl RepositoryContext {
    fn owner_and_name(&self) -> Result<(&str, &str), AdapterReadError> {
        self.name_with_owner.split_once('/').ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "GitHub returned an invalid repository name '{}'",
                self.name_with_owner
            ))
        })
    }

    fn host(&self) -> Result<&str, AdapterReadError> {
        let remainder = self.url.strip_prefix("https://").ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "GitHub returned an invalid repository URL '{}'",
                self.url
            ))
        })?;
        let (host, path) = remainder.split_once('/').ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "GitHub returned an invalid repository URL '{}'",
                self.url
            ))
        })?;
        if host.is_empty()
            || host.contains('@')
            || path != self.name_with_owner
            || path.contains(['?', '#'])
            || path.chars().any(char::is_whitespace)
        {
            return Err(AdapterReadError::Failed(format!(
                "GitHub returned an invalid repository URL '{}'",
                self.url
            )));
        }
        Ok(host)
    }
}

/// The host, issue type ID, and Priority field value a creation needs.
type CreationTaxonomy<'a> = (&'a str, Option<String>, Option<(PriorityField, Priority)>);

struct PullGroup {
    host: String,
    targets: Vec<PullTarget>,
}

enum PullChunkError {
    /// A request-wide failure; later chunks must not run.
    Request(AdapterReadError),
    /// Path-attributed failures retained until every sequential chunk runs.
    Items(Vec<PullItemError>),
}

/// One item-scoped Pull failure tied to the caller's original input order.
struct PullItemError {
    input_index: usize,
    message: String,
}

/// Preserve the CLI-backed Adapter's environment error contract without
/// exposing [`crate::proc::ProcError`] through the ADR-0042 transport port.
fn start_failure_as_proc_error(failure: &GraphqlStartFailure) -> crate::proc::ProcError {
    match failure {
        GraphqlStartFailure::Unavailable(_) => crate::proc::ProcError::ExecutableNotFound,
        GraphqlStartFailure::Failed(_) => crate::proc::ProcError::SpawnFailed,
    }
}

/// Name the failed GitHub host on every request-wide Pull diagnostic.
fn pull_request_error(host: &str, detail: impl std::fmt::Display) -> PullChunkError {
    PullChunkError::Request(AdapterReadError::Failed(format!("{host}: {detail}")))
}

fn plan_pull(items: &[BackendItemAddress]) -> Result<Vec<PullGroup>, AdapterReadError> {
    let mut groups: Vec<PullGroup> = Vec::new();
    let mut group_by_host: HashMap<&str, usize> = HashMap::new();
    for (input_index, item) in items.iter().enumerate() {
        let address = GithubIssueAddress::parse(&item.backend_key).ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "'{}' is not a canonical GitHub issue URL",
                item.backend_key
            ))
        })?;
        let target = PullTarget {
            input_index,
            owner: address.owner.into(),
            name: address.repository.into(),
            number: address.number,
        };
        if let Some(&group_index) = group_by_host.get(address.host) {
            groups[group_index].targets.push(target);
        } else {
            group_by_host.insert(address.host, groups.len());
            groups.push(PullGroup {
                host: address.host.into(),
                targets: vec![target],
            });
        }
    }
    Ok(groups)
}

/// Split one GraphQL error list at the request-wide certainty boundary.
fn classify_pull_errors(
    errors: Vec<GraphqlError>,
    targets: &[PullTarget],
    host: &str,
) -> PullChunkError {
    let mut request_errors = Vec::new();
    let mut item_errors = Vec::new();
    for error in errors {
        let input_index = error.root_field().and_then(|field| {
            targets
                .iter()
                .find(|target| target.alias() == field)
                .map(|target| target.input_index)
        });
        let message = error.into_message();
        if let Some(index) = input_index {
            item_errors.push(PullItemError {
                input_index: index,
                message,
            });
        } else {
            request_errors.push(message);
        }
    }
    if !request_errors.is_empty() {
        request_errors.sort();
        request_errors.dedup();
        return pull_request_error(host, request_errors.join("\n"));
    }
    PullChunkError::Items(item_errors)
}

/// Build the earliest item failure detail in original input order.
fn pull_item_error_detail(
    mut item_errors: Vec<PullItemError>,
    items: &[BackendItemAddress],
) -> String {
    item_errors.sort_by(|left, right| {
        left.input_index
            .cmp(&right.input_index)
            .then(left.message.cmp(&right.message))
    });
    let first_index = item_errors
        .first()
        .expect("a non-empty GraphQL error list must classify at least one error")
        .input_index;
    let mut messages = item_errors
        .into_iter()
        .take_while(|error| error.input_index == first_index)
        .map(|error| error.message)
        .collect::<Vec<_>>();
    messages.dedup();
    format!(
        "{}: {}",
        items[first_index].backend_key,
        messages.join("\n")
    )
}

fn decode_pull_item(
    address: &BackendItemAddress,
    owner: &str,
    repository: Option<PullRepository>,
) -> Result<BackendPullItem, AdapterReadError> {
    let key = &address.backend_key;
    let repository = repository.ok_or_else(|| {
        AdapterReadError::Failed(format!(
            "{key}: GitHub returned no repository for the requested issue"
        ))
    })?;
    let PullRepository {
        issue_fields,
        item: object,
    } = repository;
    let object = object.ok_or_else(|| {
        AdapterReadError::Failed(format!("{key}: GitHub returned no issue or pull request"))
    })?;
    let refresh = match object {
        PullObject::PullRequest { number, url } => {
            return Err(AdapterReadError::Failed(format!(
                "#{number}: GitHub returned a pull request, not an issue ({url})"
            )));
        }
        PullObject::Issue { issue } => {
            issue.validate_key(key)?;
            issue.validate_identity()?;
            let ticket_kind = classify_ticket_kind(&issue);
            // An Epic issue carries no Priority, so it needs no Priority field.
            let priority = if issue.has_type(EPIC_ISSUE_TYPE) {
                None
            } else {
                PriorityField::resolve(issue_fields, owner)?;
                Some(priority::from_values(
                    issue.issue_field_values.as_ref(),
                    owner,
                )?)
            };
            issue.into_refresh(ticket_kind, priority)?
        }
    };
    Ok(BackendPullItem {
        address: address.clone(),
        refresh,
    })
}

/// A `Bug` type is a Bug; every other case, untyped included, is a Task
/// (ADR-0054).
fn classify_ticket_kind(issue: &GhIssue) -> TicketKind {
    if issue.has_type(BUG_ISSUE_TYPE) {
        TicketKind::Bug
    } else {
        TicketKind::Task
    }
}

/// The `issueType` object; `null` for an untyped issue or a repo without issue
/// types.
#[derive(Debug, Deserialize)]
struct GhIssueType {
    name: String,
}

impl GhIssue {
    fn validate_key(&self, key: &str) -> Result<(), AdapterReadError> {
        if key == self.number.to_string() || key == self.url {
            return Ok(());
        }
        Err(AdapterReadError::Failed(format!(
            "GitHub resolved '{key}' to a different issue ({})",
            self.url
        )))
    }

    /// Canonical identity, or a read failure when `gh` returns something tk
    /// must not bind an Item to.
    ///
    /// PR guard (ADR-0034): native issue reads can resolve a pull request too
    /// because issues and PRs share one number sequence. Reject a `/pull/<n>`
    /// URL before it becomes a Backend Item; tk has no PR concept.
    fn validated_identity(&self) -> Result<BackendItemIdentity, AdapterReadError> {
        self.validate_identity()?;
        Ok(BackendItemIdentity {
            backend_key: self.url.clone(),
            display_id: format!("gh-{}", self.number),
        })
    }

    fn validate_identity(&self) -> Result<(), AdapterReadError> {
        if is_pull_request_url(&self.url) {
            return Err(AdapterReadError::Failed(format!(
                "#{} is a pull request, not an issue",
                self.number
            )));
        }
        let address = GithubIssueAddress::parse(&self.url).ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "#{}: GitHub returned a non-canonical issue URL ({})",
                self.number, self.url
            ))
        })?;
        if address.number != self.number {
            return Err(AdapterReadError::Failed(format!(
                "#{}: GitHub returned an issue URL for a different number ({})",
                self.number, self.url
            )));
        }
        Ok(())
    }

    /// Map the backend-owned issue state onto Lifecycle; GitHub distinguishes
    /// only open from closed, which is exactly the axis an Adapter sees
    /// (ADR-0043).
    fn lifecycle(&self) -> Result<Lifecycle, AdapterReadError> {
        match self.state.as_str() {
            "OPEN" => Ok(Lifecycle::Open),
            "CLOSED" => Ok(Lifecycle::Done),
            other => Err(AdapterReadError::Failed(format!(
                "#{}: unexpected issue state '{other}'",
                self.number
            ))),
        }
    }

    fn into_adopted_item(
        self,
        identity: BackendItemIdentity,
        ticket_kind: TicketKind,
    ) -> Result<AdoptedItem, AdapterReadError> {
        let status = self.lifecycle()?;
        Ok(AdoptedItem {
            display_id: identity.display_id,
            backend_key: identity.backend_key,
            ticket_kind,
            title: self.title,
            body: self.body,
            status,
        })
    }

    fn into_refresh(
        self,
        ticket_kind: TicketKind,
        priority: Option<Priority>,
    ) -> Result<BackendItemRefresh, AdapterReadError> {
        let status = self.lifecycle()?;
        Ok(BackendItemRefresh {
            title: self.title,
            body: self.body,
            status,
            ticket_kind: Some(ticket_kind),
            priority,
        })
    }

    fn has_type(&self, name: &str) -> bool {
        self.issue_type
            .as_ref()
            .is_some_and(|issue_type| issue_type.name.eq_ignore_ascii_case(name))
    }

    fn into_inspection(
        self,
        identity: BackendItemIdentity,
        ticket_kind: TicketKind,
    ) -> BackendItemInspection {
        BackendItemInspection {
            identity,
            title: self.title,
            body: self.body,
            ticket_kind,
        }
    }
}

/// True when `url`'s path ends in `/pull/<digits>` — GitHub's canonical PR url
/// shape (an issue is `/issues/<n>`). Anchored on the trailing segment, not a
/// bare `contains("/pull/")`, so a repo literally named `pull`
/// (`.../pull/issues/3`) is not a false positive.
fn is_pull_request_url(url: &str) -> bool {
    match url.rsplit_once('/') {
        Some((rest, last)) if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) => {
            rest.ends_with("/pull")
        }
        _ => false,
    }
}

/// Parse a canonical GitHub issue URL into its Backend identity.
///
/// Requiring HTTPS and the exact
/// `<host>/<owner>/<repo>/issues/<positive number>` shape keeps the URL and
/// Display ID consistent before the identity crosses into the Repository
/// Store.
fn parse_issue_url(url: &str) -> Option<BackendItemIdentity> {
    let address = GithubIssueAddress::parse(url)?;
    Some(BackendItemIdentity {
        display_id: format!("gh-{}", address.number),
        backend_key: url.to_owned(),
    })
}

struct GithubIssueAddress<'a> {
    host: &'a str,
    owner: &'a str,
    repository: &'a str,
    number: i64,
}

impl<'a> GithubIssueAddress<'a> {
    fn parse(url: &'a str) -> Option<Self> {
        let mut segments = url.split('/');
        let scheme = segments.next()?;
        let empty = segments.next()?;
        let host = segments.next()?;
        let owner = segments.next()?;
        let repository = segments.next()?;
        let collection = segments.next()?;
        let number = segments.next()?;
        if segments.next().is_some()
            || scheme != "https:"
            || !empty.is_empty()
            || host.is_empty()
            || host.contains('@')
            || owner.is_empty()
            || repository.is_empty()
            || collection != "issues"
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || (number.len() > 1 && number.starts_with('0'))
            || [host, owner, repository, number].iter().any(|segment| {
                segment.contains(['?', '#']) || segment.chars().any(char::is_whitespace)
            })
        {
            return None;
        }
        let number: i64 = number.parse().ok()?;
        if number <= 0 {
            return None;
        }
        Some(Self {
            host,
            owner,
            repository,
            number,
        })
    }
}

/// Classify a non-zero `gh` failure into a [`FailureClass`] (ADR-0016).
///
/// A conservative, spike-grounded set of mechanical anchors; everything
/// unmatched stays `Unknown` (the honest common case, since the GraphQL-backed
/// `gh issue` subcommands surface most failures as `GraphQL: <message>` with no
/// stable `HTTP <code>:` prefix). Precedence matters: a 403 collides between
/// auth and rate-limit, so the rate-limit anchors are tested first.
///
/// Exit status does not enter classification: `gh` exits 1 for almost
/// everything and even exit-4-for-auth is unreliable (cli/cli#9338). The
/// classification therefore gates on diagnostic text alone. `retry_after_s`
/// stays `None` because gh discards the rate-limit reset header from stderr.
fn classify(stderr: &str) -> FailureClass {
    let s = stderr.to_ascii_lowercase();
    if s.contains("rate limit exceeded") || s.contains("secondary rate limit") {
        FailureClass::RateLimited
    } else if s.contains("http 401") || s.contains("bad credentials") || s.contains("gh auth login")
    {
        FailureClass::Auth
    } else if s.contains("http 422") {
        FailureClass::Validation
    } else if s.contains("http 502") || s.contains("http 503") || s.contains("http 504") {
        FailureClass::Transient
    } else {
        FailureClass::Unknown
    }
}

/// Trim the captured stderr to a single clean diagnostic line for the failure
/// detail / `AdapterReadError::Failed` payload.
fn stderr_string(output: &RunOutput) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn rejected_edit(output: &RunOutput) -> BackendEditOutcome {
    let detail = stderr_string(output);
    BackendEditOutcome::Rejected(Failure {
        class: classify(&detail),
        detail,
        retry_after_s: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemAddress;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{EpicRef, LifecycleChange, MutationPayload, TitleBody};
    use crate::domain::mutation_type::MutationType;
    use crate::proc::{FakeRunner, ProcError, RunOutput};

    /// Command cwd the Adapter passes to the runner; `FakeRunner::run` ignores it.
    fn cwd() -> &'static Path {
        Path::new(".")
    }

    fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn fail(exit_code: i32, stderr: &str) -> RunOutput {
        RunOutput {
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn expect_graphql_page(
        runner: &FakeRunner,
        query: &str,
        owner: &str,
        name: &str,
        after: Option<&str>,
        response: &str,
    ) {
        expect_graphql_page_output(runner, query, owner, name, after, ok(response));
    }

    fn expect_graphql_page_output(
        runner: &FakeRunner,
        query: &str,
        owner: &str,
        name: &str,
        after: Option<&str>,
        output: RunOutput,
    ) {
        let operation_name = if query == ISSUE_TYPES_QUERY {
            "RepositoryIssueTypes"
        } else {
            panic!("unexpected GraphQL page query")
        };
        let after = after.map_or_else(|| "null".into(), json_string);
        let body = format!(
            r#"{{"query":{},"operationName":"{operation_name}","variables":{{"after":{after},"name":{},"owner":{}}}}}"#,
            json_string(query),
            json_string(name),
            json_string(owner),
        );
        runner.expect_exact_with_stdin(&graphql_argv("github.com"), body.as_bytes(), output);
    }

    fn expect_typed_create(
        runner: &FakeRunner,
        repository_id: &str,
        title: &str,
        body: &str,
        issue_type_id: Option<&str>,
        priority: Option<(&str, &str)>,
        response: &str,
    ) {
        let issue_type_id = issue_type_id.map_or_else(|| "null".into(), json_string);
        let issue_fields = priority.map_or_else(
            || "null".into(),
            |(field_id, option_id)| {
                format!(
                    r#"[{{"fieldId":{},"singleSelectOptionId":{}}}]"#,
                    json_string(field_id),
                    json_string(option_id)
                )
            },
        );
        let request_body = format!(
            r#"{{"query":{},"operationName":"CreateIssue","variables":{{"body":{},"issueFields":{issue_fields},"issueTypeId":{issue_type_id},"repositoryId":{},"title":{}}}}}"#,
            json_string(CREATE_ISSUE_QUERY),
            json_string(body),
            json_string(repository_id),
            json_string(title),
        );
        runner.expect_exact_with_stdin(
            &graphql_argv("github.com"),
            request_body.as_bytes(),
            ok(response),
        );
    }

    fn graphql_argv(host: &str) -> [&str; 9] {
        [
            "gh",
            "api",
            "graphql",
            "--hostname",
            host,
            "-H",
            "Content-Type: application/json",
            "--input",
            "-",
        ]
    }

    /// The account's Priority field as GitHub's starter pack defines it.
    fn priority_fields() -> serde_json::Value {
        serde_json::json!({
            "nodes": [{"id": "F1", "name": "Priority", "options": [
                {"id": "O0", "name": "Urgent"},
                {"id": "O1", "name": "High"},
                {"id": "O2", "name": "Medium"},
                {"id": "O3", "name": "Low"},
            ]}],
            "pageInfo": {"hasNextPage": false},
        })
    }

    fn priority_fields_json() -> String {
        serde_json::json!({"data": {"repository": {"issueFields": priority_fields()}}}).to_string()
    }

    fn expect_graphql_fields(runner: &FakeRunner, owner: &str, name: &str, response: &str) {
        runner.expect_exact_with_stdin(
            &graphql_argv("github.com"),
            &issue_fields_request_body(owner, name),
            ok(response),
        );
    }

    fn json_string(value: &str) -> String {
        serde_json::to_string(value).expect("test string must serialize")
    }

    struct PullTransport {
        calls: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl GraphqlTransport for PullTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(request.host, "github.com");
            assert_eq!(request.operation_name, "PullItems");
            assert!(request.document.contains("item_0: repository"));
            assert!(request.document.contains("item_1: repository"));
            assert!(request.document.contains("issueOrPullRequest"));
            assert_eq!(
                request.variables,
                serde_json::json!({
                    "owner_0": "o",
                    "name_0": "r",
                    "number_0": 1,
                    "owner_1": "p",
                    "name_1": "q",
                    "number_1": 2,
                })
            );
            GraphqlExchange::Completed(GraphqlCompleted {
                body: br#"{"data":{"item_0":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":1,"title":"First","body":"B1","state":"OPEN","url":"https://github.com/o/r/issues/1","issueType":{"name":"Bug"} }},"item_1":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":2,"title":"Second","body":"B2","state":"CLOSED","url":"https://github.com/p/q/issues/2","issueType":{"name":"Bug"}}}}}"#.to_vec(),
                completion: graphql::GraphqlCompletion::Succeeded {
                    detail: String::new(),
                },
            })
        }
    }

    struct GeneratedPullTransport {
        calls: std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>>,
    }

    impl GraphqlTransport for GeneratedPullTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            let mut indices = request
                .variables
                .as_object()
                .unwrap()
                .keys()
                .filter_map(|key| key.strip_prefix("number_"))
                .map(|index| index.parse::<usize>().unwrap())
                .collect::<Vec<_>>();
            indices.sort_unstable();
            self.calls
                .borrow_mut()
                .push((request.host.clone(), indices.len()));
            let variables = request.variables.as_object().unwrap();
            let mut data = serde_json::Map::new();
            for index in indices {
                let owner = variables[&format!("owner_{index}")].as_str().unwrap();
                let name = variables[&format!("name_{index}")].as_str().unwrap();
                let number = variables[&format!("number_{index}")].as_i64().unwrap();
                data.insert(
                    format!("item_{index}"),
                    serde_json::json!({
                        "issueFields": priority_fields(),
                        "item": {
                            "__typename": "Issue",
                            "number": number,
                            "title": format!("Issue {number}"),
                            "body": "",
                            "state": "OPEN",
                            "url": format!(
                                "https://{}/{owner}/{name}/issues/{number}",
                                request.host
                            ),
                            "issueType": null,
                        }
                    }),
                );
            }
            GraphqlExchange::Completed(GraphqlCompleted {
                body: serde_json::to_vec(&serde_json::json!({"data": data})).unwrap(),
                completion: graphql::GraphqlCompletion::Succeeded {
                    detail: String::new(),
                },
            })
        }
    }

    struct StaticPullTransport {
        body: Vec<u8>,
    }

    impl GraphqlTransport for StaticPullTransport {
        fn exchange(&self, _request: &GraphqlRequest) -> GraphqlExchange {
            GraphqlExchange::Completed(GraphqlCompleted {
                body: self.body.clone(),
                completion: graphql::GraphqlCompletion::Succeeded {
                    detail: String::new(),
                },
            })
        }
    }

    struct CrossHostErrorTransport {
        calls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }

    impl GraphqlTransport for CrossHostErrorTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            self.calls.borrow_mut().push(request.host.clone());
            let body = if request.host == "one.example" {
                br#"{"data":{"item_0":null,"item_2":null},"errors":[{"message":"later input failed","path":["item_2","item"]}]}"#.to_vec()
            } else {
                br#"{"data":{"item_1":null},"errors":[{"message":"earlier input failed","path":["item_1","item"]}]}"#.to_vec()
            };
            GraphqlExchange::Completed(GraphqlCompleted {
                body,
                completion: graphql::GraphqlCompletion::Failed {
                    detail: String::new(),
                },
            })
        }
    }

    struct CrossChunkErrorTransport {
        calls: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl GraphqlTransport for CrossChunkErrorTransport {
        fn exchange(&self, _request: &GraphqlRequest) -> GraphqlExchange {
            let call = self.calls.get();
            self.calls.set(call + 1);
            let body = if call == 0 {
                br#"{"data":{"item_0":null},"errors":[{"message":"first input failed","path":["item_0","item"]}]}"#.to_vec()
            } else {
                br#"{"data":{"item_50":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":51,"title":"Last","body":"","state":"OPEN","url":"https://github.com/o/r/issues/51","issueType":null}}}}"#.to_vec()
            };
            GraphqlExchange::Completed(GraphqlCompleted {
                body,
                completion: if call == 0 {
                    graphql::GraphqlCompletion::Failed {
                        detail: String::new(),
                    }
                } else {
                    graphql::GraphqlCompletion::Succeeded {
                        detail: String::new(),
                    }
                },
            })
        }
    }

    struct ItemThenRequestErrorTransport {
        calls: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl GraphqlTransport for ItemThenRequestErrorTransport {
        fn exchange(&self, _request: &GraphqlRequest) -> GraphqlExchange {
            let call = self.calls.get();
            self.calls.set(call + 1);
            let body = if call == 0 {
                br#"{"data":{"item_0":null},"errors":[{"message":"first input failed","path":["item_0","item"]}]}"#.to_vec()
            } else {
                br#"{"data":null,"errors":[{"message":"service unavailable"}]}"#.to_vec()
            };
            GraphqlExchange::Completed(GraphqlCompleted {
                body,
                completion: graphql::GraphqlCompletion::Failed {
                    detail: String::new(),
                },
            })
        }
    }

    struct CrossHostDecodeErrorTransport;

    impl GraphqlTransport for CrossHostDecodeErrorTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            let body = if request.host == "one.example" {
                br#"{"data":{"item_0":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":1,"title":"Valid","body":"","state":"OPEN","url":"https://one.example/o/r/issues/1","issueType":null}},"item_2":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":3,"title":"Later","body":"","state":"FUTURE","url":"https://one.example/o/r/issues/3","issueType":null}}}}"#.to_vec()
            } else {
                br#"{"data":{"item_1":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":2,"title":"Earlier","body":"","state":"FUTURE","url":"https://two.example/o/r/issues/2","issueType":null}}}}"#.to_vec()
            };
            GraphqlExchange::Completed(GraphqlCompleted {
                body,
                completion: graphql::GraphqlCompletion::Succeeded {
                    detail: String::new(),
                },
            })
        }
    }

    struct PanicPullTransport;

    impl GraphqlTransport for PanicPullTransport {
        fn exchange(&self, _request: &GraphqlRequest) -> GraphqlExchange {
            panic!("invalid Pull input must be rejected before transport")
        }
    }

    struct PullStartFailureTransport {
        failure: std::cell::RefCell<Option<GraphqlStartFailure>>,
    }

    impl GraphqlTransport for PullStartFailureTransport {
        fn exchange(&self, _request: &GraphqlRequest) -> GraphqlExchange {
            GraphqlExchange::NotStarted(
                self.failure
                    .borrow_mut()
                    .take()
                    .expect("test transport is called once"),
            )
        }
    }

    struct ScriptedGraphqlTransport {
        exchanges: std::cell::RefCell<std::collections::VecDeque<GraphqlExchange>>,
        operations: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }

    impl GraphqlTransport for ScriptedGraphqlTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            self.operations.borrow_mut().push(request.operation_name);
            self.exchanges
                .borrow_mut()
                .pop_front()
                .expect("test transport must script every GraphQL exchange")
        }
    }

    fn completed_graphql(body: &str, failure: Option<&str>) -> GraphqlExchange {
        GraphqlExchange::Completed(GraphqlCompleted {
            body: body.as_bytes().to_vec(),
            completion: failure.map_or_else(
                || graphql::GraphqlCompletion::Succeeded {
                    detail: String::new(),
                },
                |detail| graphql::GraphqlCompletion::Failed {
                    detail: detail.into(),
                },
            ),
        })
    }

    fn create_typed_with_exchange(exchange: GraphqlExchange) -> BackendCreateOutcome {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        let operations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let transport = ScriptedGraphqlTransport {
            exchanges: std::cell::RefCell::new(
                [
                    completed_graphql(
                        r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
                        None,
                    ),
                    completed_graphql(
                        r#"{"data":{"repository":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}}}}}"#,
                        None,
                    ),
                    exchange,
                ]
                .into(),
            ),
            operations: operations.clone(),
        };
        let mut adapter =
            GithubAdapter::with_graphql_transport(&runner, cwd(), Box::new(transport));

        let outcome = adapter.create_item(&BackendCreate::Ticket {
            snapshot: TitleBody {
                title: "Bug title".into(),
                body: "Bug body".into(),
            },
            ticket_kind: TicketKind::Bug,
            priority: Priority::P2,
        });

        assert_eq!(
            operations.borrow().as_slice(),
            [
                "RepositoryIssueTypes",
                "RepositoryIssueFields",
                "CreateIssue"
            ]
        );
        runner.assert_all_consumed();
        outcome
    }

    /// Build a `gh issue view --json` object. `issue_type` is either `"null"`
    /// or a type name; the extra object fields exercise serde's field-skipping.
    fn issue_json(number: i64, state: &str, issue_type: &str, url: &str) -> String {
        let it = if issue_type == "null" {
            "null".to_string()
        } else {
            format!(r#"{{"id":"IT_x","name":"{issue_type}","description":"d","color":"RED"}}"#)
        };
        format!(
            r#"{{"number":{number},"title":"T{number}","body":"B","state":"{state}","issueType":{it},"updatedAt":"2026-06-20T00:00:00Z","url":"{url}"}}"#
        )
    }

    fn edit(mt: MutationType, payload: MutationPayload, key: &str) -> BackendEdit {
        let target = address(key);
        match (mt, payload) {
            (MutationType::UpdateTicket, MutationPayload::UpdateTitleBody(snapshot)) => {
                BackendEdit::UpdateTicket {
                    ticket: target,
                    snapshot,
                }
            }
            (MutationType::UpdateEpic, MutationPayload::UpdateTitleBody(snapshot)) => {
                BackendEdit::UpdateEpic {
                    epic: target,
                    snapshot,
                }
            }
            (MutationType::SetItemStatus, MutationPayload::Lifecycle(change)) => {
                BackendEdit::SetItemStatus {
                    item: target,
                    change,
                }
            }
            (MutationType::AddTicketToEpic, MutationPayload::EpicRef(_)) => {
                BackendEdit::AddTicketToEpic {
                    ticket: target,
                    epic: address("9"),
                }
            }
            (MutationType::RemoveTicketFromEpic, MutationPayload::EpicRef(_)) => {
                BackendEdit::RemoveTicketFromEpic { ticket: target }
            }
            other => panic!("unsupported test edit: {other:?}"),
        }
    }

    fn address(key: &str) -> BackendItemAddress {
        BackendItemAddress {
            backend_key: key.into(),
        }
    }

    #[test]
    fn issue_types_wire_field_requires_a_connection_or_explicit_null() {
        let response: IssueTypePageResponse =
            serde_json::from_str(r#"{"repository":{"issueTypes":null}}"#).unwrap();
        assert!(matches!(
            response.repository.unwrap().issue_types,
            IssueTypesField::Null(())
        ));

        for response in [
            r#"{"repository":{}}"#,
            r#"{"repository":{"issueTypes":{"pageInfo":{"hasNextPage":false,"endCursor":null}}}}"#,
            r#"{"repository":{"issueTypes":{"nodes":[],"pageInfo":null}}}"#,
        ] {
            assert!(serde_json::from_str::<IssueTypePageResponse>(response).is_err());
        }
    }

    #[test]
    fn pull_batches_multiple_repositories_and_restores_input_order() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(PullTransport {
                calls: calls.clone(),
            }),
        );
        let items = [
            address("https://github.com/o/r/issues/1"),
            address("https://github.com/p/q/issues/2"),
        ];

        let pulled = adapter.pull(&items).unwrap().into_refreshes();

        assert_eq!(calls.get(), 1);
        assert_eq!(pulled[0].0, items[0].backend_key);
        assert_eq!(pulled[0].1.title, "First");
        assert_eq!(pulled[0].1.ticket_kind, Some(TicketKind::Bug));
        assert_eq!(pulled[1].0, items[1].backend_key);
        assert_eq!(pulled[1].1.status, Lifecycle::Done);
        assert_eq!(pulled[1].1.ticket_kind, Some(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    #[test]
    fn pull_maps_only_a_bug_type_to_bug() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(StaticPullTransport {
                body: br#"{"data":{"item_0":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":1,"title":"Untyped","body":"","state":"OPEN","url":"https://github.com/o/r/issues/1","issueType":null}},"item_1":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":2,"title":"Feature","body":"","state":"OPEN","url":"https://github.com/o/r/issues/2","issueType":{"name":"Feature"}}},"item_2":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":3,"title":"Bug","body":"","state":"OPEN","url":"https://github.com/o/r/issues/3","issueType":{"name":"bug"}}},"item_3":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":4,"title":"Epic","body":"","state":"OPEN","url":"https://github.com/o/r/issues/4","issueType":{"name":"Epic"}}}}}"#.to_vec(),
            }),
        );
        let items = [
            address("https://github.com/o/r/issues/1"),
            address("https://github.com/o/r/issues/2"),
            address("https://github.com/o/r/issues/3"),
            address("https://github.com/o/r/issues/4"),
        ];

        let pulled = adapter.pull(&items).unwrap().into_refreshes();

        let kinds: Vec<_> = pulled
            .iter()
            .map(|(_, refresh)| refresh.ticket_kind)
            .collect();
        assert_eq!(
            kinds,
            [
                Some(TicketKind::Task),
                Some(TicketKind::Task),
                Some(TicketKind::Bug),
                Some(TicketKind::Task),
            ]
        );
    }

    #[test]
    fn pull_chunks_each_host_at_the_private_key_cap() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(GeneratedPullTransport {
                calls: calls.clone(),
            }),
        );
        let items = (1..=MAX_PULL_KEYS_PER_QUERY + 1)
            .map(|number| address(&format!("https://github.com/o/r/issues/{number}")))
            .collect::<Vec<_>>();

        let pulled = adapter.pull(&items).unwrap().into_refreshes();

        assert_eq!(pulled.len(), items.len());
        assert_eq!(
            calls.borrow().as_slice(),
            [
                ("github.com".to_string(), MAX_PULL_KEYS_PER_QUERY),
                ("github.com".to_string(), 1),
            ]
        );
    }

    #[test]
    fn pull_groups_hosts_in_first_seen_order_without_reordering_results() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(GeneratedPullTransport {
                calls: calls.clone(),
            }),
        );
        let items = [
            address("https://one.example/o/r/issues/1"),
            address("https://two.example/o/r/issues/2"),
            address("https://one.example/o/r/issues/3"),
        ];

        let pulled = adapter.pull(&items).unwrap().into_refreshes();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                ("one.example".to_string(), 2),
                ("two.example".to_string(), 1),
            ]
        );
        assert_eq!(
            pulled.into_iter().map(|(key, _)| key).collect::<Vec<_>>(),
            items
                .iter()
                .map(|item| item.backend_key.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pull_reports_item_errors_in_original_input_order() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(StaticPullTransport {
                body: br#"{"data":{"item_0":null,"item_1":null},"errors":[{"message":"second failed","path":["item_1","item"]},{"message":"first failed","path":["item_0","item"]}]}"#.to_vec(),
            }),
        );
        let items = [
            address("https://github.com/o/r/issues/1"),
            address("https://github.com/o/r/issues/2"),
        ];

        let error = adapter.pull(&items).unwrap_err().to_string();

        assert_eq!(error, "https://github.com/o/r/issues/1: first failed");
    }

    #[test]
    fn pull_reports_the_earliest_item_error_across_host_batches() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(CrossHostErrorTransport {
                calls: calls.clone(),
            }),
        );
        let items = [
            address("https://one.example/o/r/issues/1"),
            address("https://two.example/o/r/issues/2"),
            address("https://one.example/o/r/issues/3"),
        ];

        let error = adapter.pull(&items).unwrap_err().to_string();

        assert_eq!(
            error,
            "https://two.example/o/r/issues/2: earlier input failed"
        );
        assert_eq!(
            calls.borrow().as_slice(),
            ["one.example".to_string(), "two.example".to_string()]
        );
    }

    #[test]
    fn pull_checks_later_chunks_before_reporting_item_errors() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(CrossChunkErrorTransport {
                calls: calls.clone(),
            }),
        );
        let items = (1..=MAX_PULL_KEYS_PER_QUERY + 1)
            .map(|number| address(&format!("https://github.com/o/r/issues/{number}")))
            .collect::<Vec<_>>();

        let error = adapter.pull(&items).unwrap_err().to_string();

        assert_eq!(error, "https://github.com/o/r/issues/1: first input failed");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn pull_request_failure_overrides_an_earlier_item_failure() {
        let runner = FakeRunner::new();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(ItemThenRequestErrorTransport {
                calls: calls.clone(),
            }),
        );
        let items = (1..=MAX_PULL_KEYS_PER_QUERY + 1)
            .map(|number| address(&format!("https://github.com/o/r/issues/{number}")))
            .collect::<Vec<_>>();

        let error = adapter.pull(&items).unwrap_err().to_string();

        assert_eq!(error, "github.com: service unavailable");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn pull_reports_the_earliest_decode_failure_across_hosts() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(CrossHostDecodeErrorTransport),
        );
        let items = [
            address("https://one.example/o/r/issues/1"),
            address("https://two.example/o/r/issues/2"),
            address("https://one.example/o/r/issues/3"),
        ];

        let error = adapter.pull(&items).unwrap_err().to_string();

        assert!(
            error.starts_with("https://two.example/o/r/issues/2:"),
            "error={error:?}"
        );
        assert!(error.contains("unexpected issue state 'FUTURE'"));
    }

    #[test]
    fn pull_reports_pathless_and_unknown_path_errors_as_request_wide() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(StaticPullTransport {
                body: br#"{"data":null,"errors":[{"message":"z pathless"},{"message":"a unknown","path":["item_999"]}]}"#.to_vec(),
            }),
        );

        let error = adapter
            .pull(&[address("https://github.com/o/r/issues/1")])
            .unwrap_err()
            .to_string();

        assert_eq!(error, "github.com: a unknown\nz pathless");
    }

    #[test]
    fn pull_keeps_transport_availability_as_a_bare_environment_error() {
        for (failure, expected) in [
            (
                GraphqlStartFailure::Unavailable("missing transport".into()),
                ProcError::ExecutableNotFound,
            ),
            (
                GraphqlStartFailure::Failed("could not start".into()),
                ProcError::SpawnFailed,
            ),
        ] {
            let runner = FakeRunner::new();
            let mut adapter = GithubAdapter::with_graphql_transport(
                &runner,
                cwd(),
                Box::new(PullStartFailureTransport {
                    failure: std::cell::RefCell::new(Some(failure)),
                }),
            );

            let error = adapter
                .pull(&[address("https://github.example/o/r/issues/1")])
                .unwrap_err();

            assert!(matches!(
                (error, expected),
                (
                    AdapterReadError::Env(ProcError::ExecutableNotFound),
                    ProcError::ExecutableNotFound
                ) | (
                    AdapterReadError::Env(ProcError::SpawnFailed),
                    ProcError::SpawnFailed
                )
            ));
        }
    }

    #[test]
    fn pull_names_the_backend_key_when_a_response_field_is_missing() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::with_graphql_transport(
            &runner,
            cwd(),
            Box::new(StaticPullTransport {
                body: br#"{"data":{}}"#.to_vec(),
            }),
        );
        let key = "https://github.com/o/r/issues/52";

        let error = adapter.pull(&[address(key)]).unwrap_err().to_string();

        assert_eq!(
            error,
            format!("{key}: GitHub GraphQL response omitted its Pull field")
        );
        assert!(!error.contains("item_"));
    }

    #[test]
    fn pull_rejects_noncanonical_keys_before_transport() {
        let runner = FakeRunner::new();
        let mut adapter =
            GithubAdapter::with_graphql_transport(&runner, cwd(), Box::new(PanicPullTransport));

        let error = adapter.pull(&[address("1")]).unwrap_err().to_string();

        assert_eq!(error, "'1' is not a canonical GitHub issue URL");
    }

    #[test]
    fn pull_rejects_missing_pull_request_malformed_and_mismatched_results() {
        let cases = [
            (
                br#"{"data":{"item_0":null}}"#.as_slice(),
                "returned no repository",
            ),
            (
                br#"{"data":{"item_0":{"item":null}}}"#.as_slice(),
                "returned no issue or pull request",
            ),
            (
                br#"{"data":{"item_0":{"item":{"__typename":"PullRequest","number":1,"url":"https://github.com/o/r/pull/1"}}}}"#.as_slice(),
                "returned a pull request",
            ),
            (b"not json".as_slice(), "could not parse GitHub GraphQL response"),
            (
                br#"{"data":{"item_0":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":1,"title":"Wrong","body":"","state":"OPEN","url":"https://github.com/o/r/issues/2","issueType":null}}}}"#.as_slice(),
                "different issue",
            ),
            (
                br#"{"data":{"item_0":{"issueFields":{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O0","name":"Urgent"},{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}},"item":{"__typename":"Issue","number":1,"title":"Future","body":"","state":"FUTURE","url":"https://github.com/o/r/issues/1","issueType":null}}}}"#.as_slice(),
                "unexpected issue state",
            ),
        ];
        for (body, expected) in cases {
            let runner = FakeRunner::new();
            let mut adapter = GithubAdapter::with_graphql_transport(
                &runner,
                cwd(),
                Box::new(StaticPullTransport {
                    body: body.to_vec(),
                }),
            );

            let error = adapter
                .pull(&[address("https://github.com/o/r/issues/1")])
                .unwrap_err()
                .to_string();

            assert!(error.contains(expected), "error={error:?}");
        }
    }

    fn identity(url: &str) -> BackendItemIdentity {
        let key = url.rsplit('/').next().unwrap();
        BackendItemIdentity {
            backend_key: url.into(),
            display_id: format!("gh-{key}"),
        }
    }

    fn dep_edit(mt: MutationType, blocked: &str, blocking: &str) -> BackendEdit {
        match mt {
            MutationType::AddDependency => BackendEdit::AddDependency {
                blocked: address(blocked),
                blocking: address(blocking),
            },
            MutationType::RemoveDependency => BackendEdit::RemoveDependency {
                blocked: address(blocked),
                blocking: address(blocking),
            },
            _ => panic!("not a dependency edit"),
        }
    }

    // ---- directional reads ---------------------------------------------

    #[test]
    fn adopt_requests_exact_argv_and_parses_canonical_open_issue() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "42", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                42,
                "OPEN",
                "Task",
                "https://github.com/o/r/issues/42",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let s = adapter.adopt_ticket("42").unwrap();
        assert_eq!(s.backend_key, "https://github.com/o/r/issues/42");
        assert_eq!(s.display_id, "gh-42");
        assert_eq!(s.ticket_kind, TicketKind::Task);
        assert_eq!(s.status, Lifecycle::Open);
        assert_eq!(s.title, "T42");
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_requests_exact_argv_and_returns_canonical_identity_and_content() {
        let key = "42";
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", key, "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                42,
                "OPEN",
                "Bug",
                "https://github.com/o/r/issues/42",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let inspection = adapter.inspect_item(key).unwrap();
        assert_eq!(
            inspection.identity,
            BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/42".into(),
                display_id: "gh-42".into(),
            }
        );
        assert_eq!(inspection.title, "T42");
        assert_eq!(inspection.body, "B");
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_rejects_a_numeric_key_that_resolves_to_another_issue() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                2,
                "OPEN",
                "Task",
                "https://github.com/o/r/issues/2",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let AdapterReadError::Failed(detail) = adapter.inspect_item("1").unwrap_err() else {
            panic!("redirected identity must be a Backend read failure")
        };
        assert!(detail.contains("resolved '1' to a different issue"));
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_rejects_a_url_key_that_resolves_to_another_issue() {
        let key = "https://github.com/o/r/issues/1";
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", key, "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                2,
                "OPEN",
                "Task",
                "https://github.com/o/r/issues/2",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let AdapterReadError::Failed(detail) = adapter.inspect_item(key).unwrap_err() else {
            panic!("redirected identity must be a Backend read failure")
        };
        assert!(detail.contains("resolved"));
        assert!(detail.contains("issues/2"));
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_rejects_an_issue_number_that_disagrees_with_its_url() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                1,
                "OPEN",
                "Task",
                "https://github.com/o/r/issues/2",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let AdapterReadError::Failed(detail) = adapter.inspect_item("1").unwrap_err() else {
            panic!("inconsistent identity must be a Backend read failure")
        };
        assert!(detail.contains("URL for a different number"));
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_rejects_a_noncanonical_issue_url() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                1,
                "OPEN",
                "Task",
                "http://github.com/o/r/issues/1",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let AdapterReadError::Failed(detail) = adapter.inspect_item("1").unwrap_err() else {
            panic!("noncanonical identity must be a Backend read failure")
        };
        assert!(detail.contains("non-canonical issue URL"));
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_does_not_map_backend_status() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "42", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                42,
                "FUTURE_STATE",
                "Bug",
                "https://github.com/o/r/issues/42",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let inspection = adapter.inspect_item("42").unwrap();

        assert_eq!(inspection.identity.display_id, "gh-42");
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_rejects_a_pull_request() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "99", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                99,
                "OPEN",
                "null",
                "https://github.com/o/r/pull/99",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        match adapter.inspect_item("99").unwrap_err() {
            AdapterReadError::Failed(detail) => {
                assert!(detail.contains("#99 is a pull request"), "{detail}");
            }
            AdapterReadError::Env(error) => panic!("expected Failed, got Env({error:?})"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_non_zero_exit_is_adapter_failure() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "42", "--json", ISSUE_JSON_FIELDS],
            fail(1, "GraphQL: issue not found"),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        assert!(matches!(
            adapter.inspect_item("42").unwrap_err(),
            AdapterReadError::Failed(detail) if detail == "GraphQL: issue not found"
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn inspect_spawn_failure_is_environment_failure() {
        let runner = FakeRunner::new();
        runner.expect_exact_error(
            &["gh", "issue", "view", "42", "--json", ISSUE_JSON_FIELDS],
            ProcError::ExecutableNotFound,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        assert!(matches!(
            adapter.inspect_item("42").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn adopt_rejects_a_pull_request() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "99", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                99,
                "OPEN",
                "null",
                "https://github.com/o/r/pull/99",
            )),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        match adapter.adopt_ticket("99").unwrap_err() {
            AdapterReadError::Failed(d) => assert!(d.contains("#99 is a pull request"), "{d}"),
            AdapterReadError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
        runner.assert_all_consumed();
    }

    // ---- apply_edit -------------------------------------------------

    #[test]
    fn apply_update_ticket_edits_title_and_body() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &[
                "gh", "issue", "edit", "42", "--title", "New", "--body", "Body",
            ],
            ok(""),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "New".into(),
                body: "Body".into(),
            }),
            "42",
        );
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_set_status_done_closes() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["gh", "issue", "close", "42"], ok(""));
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::Lifecycle(LifecycleChange {
                status: Lifecycle::Done,
            }),
            "42",
        );
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_set_status_open_rejects_without_running_gh() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::Lifecycle(LifecycleChange {
                status: Lifecycle::Open,
            }),
            "42",
        );
        assert_eq!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::rejected(
                "cannot push a target Lifecycle of 'open': v1 has no remote reopen"
            )
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_exit_zero_with_stderr_is_accepted() {
        // gh's idempotent "already closed" no-op exits 0 but prints to stderr.
        // Success is judged by exit code, so this must be Acknowledged.
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "close", "42"],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: b"! Issue o/r#42 (T) is already closed".to_vec(),
            },
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::Lifecycle(LifecycleChange {
                status: Lifecycle::Done,
            }),
            "42",
        );
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_non_zero_is_classified_rejection() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "42", "--title", "T", "--body", ""],
            fail(1, "HTTP 422: Validation Failed"),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "T".into(),
                body: String::new(),
            }),
            "42",
        );
        match adapter.apply_edit(&v).unwrap() {
            BackendEditOutcome::Rejected(f) => {
                assert_eq!(f.class, FailureClass::Validation);
                assert_eq!(f.detail, "HTTP 422: Validation Failed");
                assert_eq!(f.retry_after_s, None);
            }
            BackendEditOutcome::Acknowledged => panic!("expected rejection"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_spawn_failure_is_apply_error() {
        let runner = FakeRunner::new();
        runner.expect_exact_error(&["gh", "issue", "close", "42"], ProcError::SpawnFailed);
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::Lifecycle(LifecycleChange {
                status: Lifecycle::Done,
            }),
            "42",
        );
        assert!(matches!(
            adapter.apply_edit(&v),
            Err(ProcError::SpawnFailed)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_update_epic_edits_title_and_body() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &[
                "gh", "issue", "edit", "42", "--title", "Epic", "--body", "Plan",
            ],
            ok(""),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let edit = edit(
            MutationType::UpdateEpic,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "Epic".into(),
                body: "Plan".into(),
            }),
            "42",
        );
        assert_eq!(
            adapter.apply_edit(&edit).unwrap(),
            BackendEditOutcome::Acknowledged
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_add_ticket_to_epic_sets_parent() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["gh", "issue", "edit", "42", "--parent", "9"], ok(""));
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let edit = edit(
            MutationType::AddTicketToEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            "42",
        );
        assert_eq!(
            adapter.apply_edit(&edit).unwrap(),
            BackendEditOutcome::Acknowledged
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_remove_ticket_from_epic_removes_parent() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["gh", "issue", "edit", "42", "--remove-parent"], ok(""));
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let edit = edit(
            MutationType::RemoveTicketFromEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            "42",
        );
        assert_eq!(
            adapter.apply_edit(&edit).unwrap(),
            BackendEditOutcome::Acknowledged
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_add_dependency_edits_blocked_by() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--add-blocked-by", "9"],
            ok(""),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = dep_edit(MutationType::AddDependency, "5", "9");
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_remove_dependency_edits_remove_blocked_by() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--remove-blocked-by", "9"],
            ok(""),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = dep_edit(MutationType::RemoveDependency, "5", "9");
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_dependency_non_zero_is_classified_rejection() {
        // A stale gh (< 2.94.0) rejects the unknown flag, so the queue stops.
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--add-blocked-by", "9"],
            fail(1, "unknown flag: --add-blocked-by"),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let v = dep_edit(MutationType::AddDependency, "5", "9");
        match adapter.apply_edit(&v).unwrap() {
            BackendEditOutcome::Rejected(f) => {
                assert!(f.detail.contains("unknown flag"), "{}", f.detail);
                assert_eq!(f.class, FailureClass::Unknown);
            }
            BackendEditOutcome::Acknowledged => panic!("stale gh must reject, not no-op"),
        }
        runner.assert_all_consumed();
    }

    // ---- Promotion -------------------------------------------------------

    #[test]
    fn resolves_requested_static_facets_without_io() {
        let runner = FakeRunner::new();
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let caps = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none()
                    .with_ticket_kind(TicketKind::Task)
                    .with_dependencies()
                    .with_epic_membership(),
            )
            .unwrap();
        assert!(!caps.can_create_item_class(ItemClass::Epic));
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(caps.can_represent_dependencies());
        assert!(caps.can_represent_epic_membership());
    }

    #[test]
    fn resolves_an_enabled_native_bug_type_for_bug_requirements() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug),
            )
            .unwrap();
        assert!(capabilities.can_create_ticket_kind(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    #[test]
    fn pages_issue_types_until_it_finds_an_enabled_bug() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Task","isEnabled":true}],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR"}}}}}"#,
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            Some("CURSOR"),
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_2","name":"Bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug),
            )
            .unwrap();
        assert!(capabilities.can_create_ticket_kind(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    #[test]
    fn a_disabled_bug_type_on_the_final_page_fails_preflight_naming_it() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Task","isEnabled":true}],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR"}}}}}"#,
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            Some("CURSOR"),
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_2","name":"Bug","isEnabled":false}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        assert!(matches!(
            adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug)
            ),
            Err(AdapterReadError::Failed(detail))
                if detail == "required issue type 'Bug' is missing on o"
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn resolves_an_enabled_epic_type_for_epic_requirements() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"EPIC","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_item_class(ItemClass::Epic),
            )
            .unwrap();
        assert!(capabilities.can_create_item_class(ItemClass::Epic));
        runner.assert_all_consumed();
    }

    #[test]
    fn issue_type_read_failure_is_not_reported_as_unsupported() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page_output(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            fail(1, "GraphQL: taxonomy read failed"),
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        assert!(matches!(
            adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug)
            ),
            Err(AdapterReadError::Failed(detail)) if detail == "GraphQL: taxonomy read failed"
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn creates_bug_with_the_resolved_issue_type_id() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        expect_graphql_fields(&runner, "o", "r", &priority_fields_json());
        expect_typed_create(
            &runner,
            "R_1",
            "Bug title",
            "Bug body",
            Some("IT_1"),
            Some(("F1", "O2")),
            r#"{"data":{"createIssue":{"issue":{"url":"https://github.com/o/r/issues/42","number":42}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());
        let outcome = adapter.create_item(&BackendCreate::Ticket {
            snapshot: TitleBody {
                title: "Bug title".into(),
                body: "Bug body".into(),
            },
            ticket_kind: TicketKind::Bug,
            priority: Priority::P2,
        });

        assert_eq!(
            outcome,
            BackendCreateOutcome::Created(identity("https://github.com/o/r/issues/42"))
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn typed_creation_rejects_when_the_graphql_exchange_does_not_start() {
        for failure in [
            GraphqlStartFailure::Unavailable("transport unavailable".into()),
            GraphqlStartFailure::Failed("request setup failed".into()),
        ] {
            let BackendCreateOutcome::Rejected(failure) =
                create_typed_with_exchange(GraphqlExchange::NotStarted(failure))
            else {
                panic!("a request that did not start cannot create an issue");
            };
            assert!(failure.detail.starts_with("GraphQL request did not start:"));
        }
    }

    #[test]
    fn typed_creation_is_indeterminate_when_the_exchange_outcome_is_unobserved() {
        let BackendCreateOutcome::Indeterminate(failure) = create_typed_with_exchange(
            GraphqlExchange::OutcomeUnobserved("connection closed while sending".into()),
        ) else {
            panic!("a started mutation without an observed outcome is indeterminate");
        };

        assert!(failure.detail.contains("outcome is unknown"));
        assert!(failure.detail.contains("connection closed while sending"));
    }

    #[test]
    fn typed_creation_accepts_a_valid_receipt_after_delivery_failure() {
        let outcome = create_typed_with_exchange(completed_graphql(
            r#"{"data":{"createIssue":{"issue":{"url":"https://github.com/o/r/issues/42","number":42}}}}"#,
            Some("delivery reported failure"),
        ));

        assert_eq!(
            outcome,
            BackendCreateOutcome::Created(identity("https://github.com/o/r/issues/42"))
        );
    }

    #[test]
    fn typed_creation_keeps_graphql_errors_as_indeterminate_detail() {
        let BackendCreateOutcome::Indeterminate(failure) =
            create_typed_with_exchange(completed_graphql(
                r#"{"data":{"createIssue":null},"errors":[{"message":"permission denied"}]}"#,
                Some("delivery reported failure"),
            ))
        else {
            panic!("GraphQL errors without a receipt leave creation indeterminate");
        };

        assert_eq!(failure.detail, "permission denied");
    }

    #[test]
    fn typed_creation_keeps_transport_detail_without_a_receipt() {
        let exchange = GraphqlExchange::Completed(GraphqlCompleted {
            body: br#"{"data":{"createIssue":null}}"#.to_vec(),
            completion: graphql::GraphqlCompletion::Succeeded {
                detail: "transport warning".into(),
            },
        });

        let BackendCreateOutcome::Indeterminate(failure) = create_typed_with_exchange(exchange)
        else {
            panic!("a response without a receipt leaves creation indeterminate");
        };

        assert_eq!(failure.detail, "transport warning");
    }

    #[test]
    fn typed_creation_treats_a_malformed_graphql_envelope_as_indeterminate() {
        let BackendCreateOutcome::Indeterminate(failure) =
            create_typed_with_exchange(completed_graphql("not json", None))
        else {
            panic!("a malformed response cannot prove whether creation ran");
        };

        assert_eq!(
            failure.detail,
            "GitHub GraphQL returned an unrecognized response"
        );
    }

    #[test]
    fn an_initial_null_issue_type_connection_fails_preflight_naming_the_epic_type() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"p/r","url":"https://github.com/p/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "p",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":null}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        assert!(matches!(
            adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_item_class(ItemClass::Epic)
            ),
            Err(AdapterReadError::Failed(detail))
                if detail == "required issue type 'Epic' is missing on p"
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn creates_an_epic_with_the_resolved_issue_type_id() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Bug","isEnabled":true},{"id":"IT_2","name":"Epic","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        expect_typed_create(
            &runner,
            "R_1",
            "Epic title",
            "Epic body",
            Some("IT_2"),
            None,
            r#"{"data":{"createIssue":{"issue":{"url":"https://github.com/o/r/issues/43","number":43}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let outcome = adapter.create_item(&BackendCreate::Epic {
            snapshot: TitleBody {
                title: "Epic title".into(),
                body: "Epic body".into(),
            },
        });

        assert_eq!(
            outcome,
            BackendCreateOutcome::Created(identity("https://github.com/o/r/issues/43"))
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn creating_an_epic_without_the_type_is_rejected_before_any_mutation() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "o",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        let BackendCreateOutcome::Rejected(failure) = adapter.create_item(&BackendCreate::Epic {
            snapshot: TitleBody {
                title: "Epic title".into(),
                body: "Epic body".into(),
            },
        }) else {
            panic!("a missing Epic type certifies that nothing was created");
        };
        assert!(
            failure
                .detail
                .contains("required issue type 'Epic' is missing on o")
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn rejects_null_issue_type_connection_after_pagination_started() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"p/r","url":"https://github.com/p/r"}"#),
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "p",
            "r",
            None,
            r#"{"data":{"repository":{"issueTypes":{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR"}}}}}"#,
        );
        expect_graphql_page(
            &runner,
            ISSUE_TYPES_QUERY,
            "p",
            "r",
            Some("CURSOR"),
            r#"{"data":{"repository":{"issueTypes":null}}}"#,
        );
        let mut adapter = GithubAdapter::new(&runner, cwd());

        assert!(matches!(
            adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug)
            ),
            Err(AdapterReadError::Failed(detail))
                if detail == "GitHub issue type pagination returned null after a continuation cursor"
        ));
        runner.assert_all_consumed();
    }

    /// Issue field lists that do not give tk a usable Priority, with the error
    /// each must raise for owner `o`. A deleted field reads as no value on every
    /// issue, which would overwrite every local Priority, so none may pass.
    fn unusable_priority_fields() -> [(&'static str, &'static str); 3] {
        [
            ("null", "required issue field 'Priority' is missing on o"),
            (
                r#"{"nodes":[{"id":"F2","name":"Effort","options":[]}],"pageInfo":{"hasNextPage":false}}"#,
                "required issue field 'Priority' is missing on o",
            ),
            (
                r#"{"nodes":[{"id":"F1","name":"Priority","options":[{"id":"O1","name":"High"},{"id":"O2","name":"Medium"},{"id":"O3","name":"Low"}]}],"pageInfo":{"hasNextPage":false}}"#,
                "required issue field 'Priority' lacks option 'Urgent' on o",
            ),
        ]
    }

    #[test]
    fn pull_fails_naming_a_missing_priority_field_or_option() {
        for (fields, expected) in unusable_priority_fields() {
            let body = format!(
                r#"{{"data":{{"item_0":{{"issueFields":{fields},"item":{{"__typename":"Issue","number":1,"title":"T","body":"","state":"OPEN","url":"https://github.com/o/r/issues/1","issueType":null}}}}}}}}"#
            );
            let runner = FakeRunner::new();
            let mut adapter = GithubAdapter::with_graphql_transport(
                &runner,
                cwd(),
                Box::new(StaticPullTransport {
                    body: body.into_bytes(),
                }),
            );

            let result = adapter.pull(&[address("https://github.com/o/r/issues/1")]);

            assert!(
                matches!(&result, Err(AdapterReadError::Failed(detail)) if detail.contains(expected)),
                "{fields}: {result:?}"
            );
        }
    }

    #[test]
    fn ticket_preflight_fails_naming_a_missing_priority_field_or_option() {
        for (fields, expected) in unusable_priority_fields() {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
                ok(r#"{"id":"R_1","nameWithOwner":"o/r","url":"https://github.com/o/r"}"#),
            );
            let transport = ScriptedGraphqlTransport {
                exchanges: std::cell::RefCell::new(
                    [completed_graphql(
                        &format!(r#"{{"data":{{"repository":{{"issueFields":{fields}}}}}}}"#),
                        None,
                    )]
                    .into(),
                ),
                operations: std::rc::Rc::default(),
            };
            let mut adapter =
                GithubAdapter::with_graphql_transport(&runner, cwd(), Box::new(transport));

            let result = adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_item_class(ItemClass::Ticket),
            );

            assert!(
                matches!(&result, Err(AdapterReadError::Failed(detail)) if detail == expected),
                "{fields}: {result:?}"
            );
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn classify_uses_mechanical_anchors_with_rate_limit_precedence() {
        assert_eq!(
            classify("API rate limit exceeded for user"),
            FailureClass::RateLimited
        );
        assert_eq!(
            classify("You have exceeded a secondary rate limit"),
            FailureClass::RateLimited
        );
        assert_eq!(classify("HTTP 401: Bad credentials"), FailureClass::Auth);
        assert_eq!(
            classify("To get started, please run:  gh auth login"),
            FailureClass::Auth
        );
        assert_eq!(
            classify("HTTP 422: Validation Failed"),
            FailureClass::Validation
        );
        assert_eq!(
            classify("HTTP 503: Service Unavailable"),
            FailureClass::Transient
        );
        assert_eq!(classify("some unrecognised error"), FailureClass::Unknown);
        // A 403 collides between auth and rate-limit; rate-limit wins.
        assert_eq!(
            classify("HTTP 403: API rate limit exceeded"),
            FailureClass::RateLimited
        );
        // Verbatim gh outputs observed in the tk-gh-playground spike
        // (docs/spikes/gh-cli-issue-behavior.md).
        assert_eq!(
            classify(
                "HTTP 401: Bad credentials (https://api.github.com/graphql)\nTry authenticating with:  gh auth login -h github.com"
            ),
            FailureClass::Auth
        );
        assert_eq!(
            classify(
                "GraphQL: Could not resolve to an issue or pull request with the number of 999999. (repository.issue)"
            ),
            FailureClass::Unknown
        );
    }

    #[test]
    fn pull_request_url_anchors_on_the_trailing_segment() {
        assert!(is_pull_request_url("https://github.com/o/r/pull/12"));
        assert!(!is_pull_request_url("https://github.com/o/r/issues/12"));
        // A repo literally named `pull` is not a false positive.
        assert!(!is_pull_request_url("https://github.com/o/pull/issues/3"));
        assert!(is_pull_request_url("https://github.com/o/pull/pull/3"));
    }
}
