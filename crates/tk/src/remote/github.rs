//! GitHub Backend Adapter over the `gh` CLI (ADR-0021 fields, ADR-0034 opt-in).
//!
//! Implements [`Adapter`] by shelling out to `gh issue` through an injected
//! [`ProcRunner`] (ADR-0031: the real subprocess only in production, a
//! `FakeRunner` in tests). Creation and bare Adopt input let `gh` resolve the
//! repository from the command cwd; canonical issue URLs then pin existing
//! Item operations to their repository (ADR-0033).
//!
//! Pull is refresh-by-key: the engine hands the Adopted working set's active
//! keys to [`GithubAdapter::refresh_item`], which fetches each with one
//! `gh issue view`. A typeless issue carrying the private `bug` label adds a
//! cached repository-ownership read. There is no issue listing or discovery
//! (ADR-0034).

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemIdentity, BackendItemInspection,
    BackendItemRefresh,
};
use crate::domain::backend_outcome::{
    BackendCreateOutcome, BackendEditOutcome, Failure, FailureClass,
};
use crate::domain::promotion_capability::{PromotionCapabilities, PromotionRequirements};
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::proc::{ProcRunner, RunOutput};

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// The `--json` field set tk requests from `gh issue view`. `url` supplies the
/// canonical Adopt identity, guards Pull identity, and rejects pull requests
/// (see [`is_pull_request_url`]); `state` arrives UPPERCASE; `issueType` is an
/// object-or-null; `labels` supplies the personal-repository Bug fallback.
const ISSUE_JSON_FIELDS: &str = "number,title,body,state,issueType,labels,url";
const REPOSITORY_JSON_FIELDS: &str = "id,nameWithOwner,isInOrganization";
const ISSUE_TYPES_QUERY: &str = "query RepositoryIssueTypes($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { issueTypes(first: 50, after: $after) { nodes { id name isEnabled } pageInfo { hasNextPage endCursor } } } }";
const LABELS_QUERY: &str = "query RepositoryLabels($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { labels(first: 100, query: \"bug\", after: $after) { nodes { id name } pageInfo { hasNextPage endCursor } } } }";
const CREATE_BUG_QUERY: &str = "mutation CreateBugIssue($repositoryId: ID!, $title: String!, $body: String!, $issueTypeId: ID, $labelIds: [ID!]) { createIssue(input: { repositoryId: $repositoryId, title: $title, body: $body, issueTypeId: $issueTypeId, labelIds: $labelIds }) { issue { url number } } }";

/// GitHub Backend Adapter. Holds the injected runner and the command cwd from
/// which `gh` resolves the repository (ADR-0033), plus per-invocation
/// repository-ownership observations used by Pull classification.
pub struct GithubAdapter<'a> {
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
    repository_ownership: HashMap<String, bool>,
}

impl<'a> GithubAdapter<'a> {
    #[must_use]
    pub fn new(runner: &'a dyn ProcRunner, cwd: &'a Path) -> Self {
        Self {
            runner,
            cwd,
            repository_ownership: HashMap::new(),
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
        let ticket_kind = self.ticket_kind(&issue, &identity)?;
        issue.into_adopted_item(identity, ticket_kind)
    }

    fn refresh_item(&mut self, key: &str) -> Result<BackendItemRefresh, AdapterReadError> {
        let issue = self.view_matching_issue(key)?;
        let identity = issue.validated_identity()?;
        let ticket_kind = self.ticket_kind(&issue, &identity)?;
        issue.into_refresh(identity, ticket_kind)
    }

    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError> {
        let issue = self.view_matching_issue(key)?;
        let identity = issue.validated_identity()?;
        let ticket_kind = self.ticket_kind(&issue, &identity)?;
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
            BackendEdit::SetItemStatus { item, change, .. } => {
                // done is terminal (ADR-0006), so reopen-from-done never occurs.
                let verb = match change.status.as_str() {
                    "done" => "close",
                    "open" | "active" => "reopen",
                    other => {
                        return Ok(BackendEditOutcome::rejected(format!(
                            "unexpected target status '{other}'"
                        )));
                    }
                };
                self.run_edit(&["gh", "issue", verb, &item.backend_key])
            }
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
        for class in [ItemClass::Ticket, ItemClass::Epic] {
            if requirements.requires_item_class(class) {
                capabilities = capabilities.with_item_class(class);
            }
        }
        if requirements.requires_ticket_kind(TicketKind::Task) {
            capabilities = capabilities.with_ticket_kind(TicketKind::Task);
        }
        if requirements.requires_ticket_kind(TicketKind::Bug) {
            let repository = self.current_repository()?;
            if self.find_bug_representation(&repository)?.is_some() {
                capabilities = capabilities.with_ticket_kind(TicketKind::Bug);
            }
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
    /// Run one edit `gh` invocation and map its outcome. Success is judged by
    /// **exit code 0**, never by stderr emptiness: `gh issue close`/`reopen`
    /// print an informational "is already closed/open" line to stderr on their
    /// idempotent no-op path yet still exit 0, so a harmless re-apply must read
    /// as Acknowledged. A non-zero exit is a per-Mutation rejection carrying the
    /// classified stderr.
    fn run_edit(&self, argv: &[&str]) -> Result<BackendEditOutcome, ApplyError> {
        let output = self.runner.run(argv, self.cwd)?;
        if output.succeeded() {
            return Ok(BackendEditOutcome::Acknowledged);
        }
        Ok(rejected_edit(&output))
    }

    /// Run one non-idempotent creation command and preserve ADR-0036's effect
    /// certainty boundary: a child that never starts is Rejected, while an
    /// unobserved child is Indeterminate.
    fn run_creation(
        &self,
        command: &str,
        argv: &[&str],
    ) -> Result<RunOutput, BackendCreateOutcome> {
        match self.runner.run(argv, self.cwd) {
            Ok(output) => Ok(output),
            Err(
                error @ (crate::proc::ProcError::ExecutableNotFound
                | crate::proc::ProcError::SpawnFailed),
            ) => Err(BackendCreateOutcome::Rejected(Failure {
                detail: format!("{command} did not start: {error}"),
                class: FailureClass::Unknown,
                retry_after_s: None,
            })),
            Err(error @ crate::proc::ProcError::OutcomeUnobserved) => {
                Err(BackendCreateOutcome::Indeterminate(Failure {
                    detail: format!("{command} started, but its outcome is unknown: {error}"),
                    class: FailureClass::Unknown,
                    retry_after_s: None,
                }))
            }
        }
    }

    /// Create the GitHub issue represented by either Promotion variant.
    ///
    /// Task Tickets and Epics share the typeless `gh issue create` surface.
    /// Bugs use the direct GraphQL path below so classification and creation
    /// are one effect (ADR-0021). A canonical receipt remains authoritative
    /// even when `gh` exits non-zero.
    fn create_issue(&self, create: &BackendCreate) -> BackendCreateOutcome {
        if let BackendCreate::Ticket {
            snapshot,
            ticket_kind: TicketKind::Bug,
        } = create
        {
            return self.create_bug_issue(snapshot);
        }
        let snapshot = match create {
            BackendCreate::Ticket { snapshot, .. } | BackendCreate::Epic { snapshot } => snapshot,
        };
        let output = match self.run_creation(
            "gh issue create",
            &[
                "gh",
                "issue",
                "create",
                "--title",
                &snapshot.title,
                "--body",
                &snapshot.body,
            ],
        ) {
            Ok(output) => output,
            Err(outcome) => return outcome,
        };

        if let Some(identity) = parse_create_receipt(&output.stdout) {
            return BackendCreateOutcome::Created(identity);
        }

        let stderr = stderr_string(&output);
        let class = classify(output.exit_code, &stderr);
        let failure = Failure {
            detail: create_failure_detail(&output, &stderr),
            class,
            retry_after_s: None,
        };
        if certifies_auth_rejection(&output, &stderr) {
            BackendCreateOutcome::Rejected(failure)
        } else {
            BackendCreateOutcome::Indeterminate(failure)
        }
    }

    /// Resolve the current Bug representation, then create the issue and its
    /// classification in one GraphQL mutation (ADR-0021).
    fn create_bug_issue(
        &self,
        snapshot: &crate::domain::mutation_payload::TitleBody,
    ) -> BackendCreateOutcome {
        let repository = match self.current_repository() {
            Ok(repository) => repository,
            Err(error) => return bug_read_rejection(&error),
        };
        let representation = match self.find_bug_representation(&repository) {
            Ok(Some(representation)) => representation,
            Ok(None) => {
                return BackendCreateOutcome::rejected(
                    "the configured GitHub repository has no usable Bug representation",
                );
            }
            Err(error) => return bug_read_rejection(&error),
        };
        let repository_id = format!("repositoryId={}", repository.id);
        let title = format!("title={}", snapshot.title);
        let body = format!("body={}", snapshot.body);
        let representation_arg = match representation {
            BugRepresentation::NativeIssueType(id) => format!("issueTypeId={id}"),
            BugRepresentation::Label(id) => format!("labelIds[]={id}"),
        };
        let argv = [
            "gh".to_owned(),
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={CREATE_BUG_QUERY}"),
            "-f".to_owned(),
            repository_id,
            "-f".to_owned(),
            title,
            "-f".to_owned(),
            body,
            "-f".to_owned(),
            representation_arg,
        ];
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let output = match self.run_creation("gh api graphql", &argv) {
            Ok(output) => output,
            Err(outcome) => return outcome,
        };
        let stderr = stderr_string(&output);
        let response: Result<GraphQlResponse<CreateIssueData>, _> =
            serde_json::from_slice(&output.stdout);
        if let Ok(response) = response {
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
                    .map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if stderr.is_empty() {
                "gh api graphql returned no GitHub issue URL receipt".into()
            } else {
                stderr.clone()
            };
            return BackendCreateOutcome::Indeterminate(Failure {
                class: classify(output.exit_code, &detail),
                detail,
                retry_after_s: None,
            });
        }
        BackendCreateOutcome::Indeterminate(Failure {
            class: classify(output.exit_code, &stderr),
            detail: if stderr.is_empty() {
                "gh api graphql returned an unrecognized response".into()
            } else {
                stderr
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

    /// Map GitHub classification to Ticket Kind. A native Issue Type wins;
    /// only a typeless issue in a personal repository may use the `bug` Label
    /// fallback (ADR-0021).
    fn ticket_kind(
        &mut self,
        issue: &GhIssue,
        identity: &BackendItemIdentity,
    ) -> Result<TicketKind, AdapterReadError> {
        if let Some(issue_type) = issue.issue_type.as_ref() {
            return Ok(if issue_type.name.eq_ignore_ascii_case("Bug") {
                TicketKind::Bug
            } else {
                TicketKind::Task
            });
        }
        if !issue
            .labels
            .iter()
            .any(|label| label.name.eq_ignore_ascii_case("bug"))
        {
            return Ok(TicketKind::Task);
        }

        let repository = identity
            .backend_key
            .rsplit_once("/issues/")
            .map(|(repository, _)| repository)
            .expect("validated GitHub issue identity contains its repository URL");
        let is_organization = self.repository_is_organization(repository)?;
        Ok(if is_organization {
            TicketKind::Task
        } else {
            TicketKind::Bug
        })
    }

    /// Read repository ownership once per canonical repository URL so Pull
    /// does not repeat the extra lookup for each labeled Bug.
    fn repository_is_organization(&mut self, repository: &str) -> Result<bool, AdapterReadError> {
        if let Some(&is_organization) = self.repository_ownership.get(repository) {
            return Ok(is_organization);
        }
        let output = self.runner.run(
            &[
                "gh",
                "repo",
                "view",
                repository,
                "--json",
                "isInOrganization",
            ],
            self.cwd,
        )?;
        if !output.succeeded() {
            return Err(AdapterReadError::Failed(stderr_string(&output)));
        }
        let ownership: RepositoryOwnership =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                AdapterReadError::Failed(format!(
                    "could not parse GitHub repository ownership: {error}"
                ))
            })?;
        self.repository_ownership
            .insert(repository.to_owned(), ownership.is_in_organization);
        Ok(ownership.is_in_organization)
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

    /// Prefer a native Issue Type for any repository; use an existing Label
    /// only for a personal repository (ADR-0021).
    fn find_bug_representation(
        &self,
        repository: &RepositoryContext,
    ) -> Result<Option<BugRepresentation>, AdapterReadError> {
        let (owner, name) = repository.name_with_owner.split_once('/').ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "GitHub returned an invalid repository name '{}'",
                repository.name_with_owner
            ))
        })?;
        if let Some(id) = self.find_native_bug_type(owner, name)? {
            return Ok(Some(BugRepresentation::NativeIssueType(id)));
        }
        if repository.is_in_organization {
            return Ok(None);
        }
        Ok(self
            .find_bug_label(owner, name)?
            .map(BugRepresentation::Label))
    }

    /// Read every Issue Type page before concluding that no enabled, exact
    /// case-insensitive `Bug` exists (ADR-0021).
    fn find_native_bug_type(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<Option<String>, AdapterReadError> {
        let mut after = None;
        loop {
            let page: IssueTypePageResponse =
                self.graphql_page(ISSUE_TYPES_QUERY, owner, name, after.as_deref())?;
            let repository = page.repository.ok_or_else(|| {
                AdapterReadError::Failed(format!(
                    "GitHub returned no repository for {owner}/{name}"
                ))
            })?;
            for issue_type in repository.issue_types.nodes {
                if issue_type.is_enabled && issue_type.name.eq_ignore_ascii_case("Bug") {
                    return Ok(Some(issue_type.id));
                }
            }
            if !repository.issue_types.page_info.has_next_page {
                return Ok(None);
            }
            after = Some(repository.issue_types.page_info.end_cursor.ok_or_else(|| {
                AdapterReadError::Failed(
                    "GitHub issue type pagination omitted its next cursor".into(),
                )
            })?);
        }
    }

    /// Read Label pages until the exact case-insensitive `bug` fallback is
    /// found or the connection is exhausted.
    fn find_bug_label(&self, owner: &str, name: &str) -> Result<Option<String>, AdapterReadError> {
        let mut after = None;
        loop {
            let page: LabelPageResponse =
                self.graphql_page(LABELS_QUERY, owner, name, after.as_deref())?;
            let repository = page.repository.ok_or_else(|| {
                AdapterReadError::Failed(format!(
                    "GitHub returned no repository for {owner}/{name}"
                ))
            })?;
            if let Some(label) = repository
                .labels
                .nodes
                .into_iter()
                .find(|label| label.name.eq_ignore_ascii_case("bug"))
            {
                return Ok(Some(label.id));
            }
            if !repository.labels.page_info.has_next_page {
                return Ok(None);
            }
            after = Some(repository.labels.page_info.end_cursor.ok_or_else(|| {
                AdapterReadError::Failed("GitHub label pagination omitted its next cursor".into())
            })?);
        }
    }

    /// Run one typed GraphQL page read. Transport, GraphQL, and malformed-data
    /// failures stay Adapter read errors; they never prove unsupported Bug.
    fn graphql_page<T: DeserializeOwned>(
        &self,
        query: &str,
        owner: &str,
        name: &str,
        after: Option<&str>,
    ) -> Result<T, AdapterReadError> {
        let query_arg = format!("query={query}");
        let owner_arg = format!("owner={owner}");
        let name_arg = format!("name={name}");
        let after_arg = format!("after={}", after.unwrap_or("null"));
        let argv = [
            "gh".to_owned(),
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            query_arg,
            "-f".to_owned(),
            owner_arg,
            "-f".to_owned(),
            name_arg,
            "-F".to_owned(),
            after_arg,
        ];
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let output = self.runner.run(&argv, self.cwd)?;
        if !output.succeeded() {
            return Err(AdapterReadError::Failed(stderr_string(&output)));
        }
        let response: GraphQlResponse<T> =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                AdapterReadError::Failed(format!(
                    "could not parse GitHub GraphQL response: {error}"
                ))
            })?;
        if !response.errors.is_empty() {
            return Err(AdapterReadError::Failed(
                response
                    .errors
                    .into_iter()
                    .map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
        response.data.ok_or_else(|| {
            AdapterReadError::Failed("GitHub GraphQL response contained no data".into())
        })
    }
}

/// Raw `gh issue view --json` shape. Only the fields tk maps are named; serde
/// ignores the rest (e.g. the issueType object's `id`/`description`/`color`).
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
    #[serde(default)]
    labels: Vec<GhLabel>,
    /// Canonical issue/PR URL used for identity and the PR guard.
    url: String,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RepositoryOwnership {
    #[serde(rename = "isInOrganization")]
    is_in_organization: bool,
}

#[derive(Debug, Deserialize)]
struct RepositoryContext {
    id: String,
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    #[serde(rename = "isInOrganization")]
    is_in_organization: bool,
}

#[derive(Debug)]
enum BugRepresentation {
    NativeIssueType(String),
    Label(String),
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct CreateIssueData {
    #[serde(rename = "createIssue")]
    create_issue: Option<CreateIssuePayload>,
}

#[derive(Debug, Deserialize)]
struct CreateIssuePayload {
    issue: Option<CreateIssueReceipt>,
}

#[derive(Debug, Deserialize)]
struct CreateIssueReceipt {
    url: String,
}

#[derive(Debug, Deserialize)]
struct IssueTypePageResponse {
    repository: Option<IssueTypeRepository>,
}

#[derive(Debug, Deserialize)]
struct IssueTypeRepository {
    #[serde(rename = "issueTypes")]
    issue_types: IssueTypeConnection,
}

#[derive(Debug, Deserialize)]
struct IssueTypeConnection {
    nodes: Vec<NativeIssueType>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct NativeIssueType {
    id: String,
    name: String,
    #[serde(rename = "isEnabled")]
    is_enabled: bool,
}

#[derive(Debug, Deserialize)]
struct LabelPageResponse {
    repository: Option<LabelRepository>,
}

#[derive(Debug, Deserialize)]
struct LabelRepository {
    labels: LabelConnection,
}

#[derive(Debug, Deserialize)]
struct LabelConnection {
    nodes: Vec<GraphQlLabel>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct GraphQlLabel {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

/// The `issueType` object; `null` for an untyped issue or a repo without issue
/// types. Native type names take precedence over the private label fallback
/// (ADR-0021).
#[derive(Debug, Deserialize)]
struct GhIssueType {
    name: String,
}

struct IssueFields {
    number: i64,
    backend_key: String,
    ticket_kind: TicketKind,
    title: String,
    body: String,
    status: ItemStatus,
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
    /// PR guard (ADR-0034): `gh issue view <n>` resolves a pull request too
    /// (issue and PR numbers share one sequence) and returns it as an
    /// issue-shaped object, so reject when the canonical url is a /pull/<n>
    /// path. tk has no PR concept; the user meant an issue.
    fn validated_identity(&self) -> Result<BackendItemIdentity, AdapterReadError> {
        if is_pull_request_url(&self.url) {
            return Err(AdapterReadError::Failed(format!(
                "#{} is a pull request, not an issue",
                self.number
            )));
        }
        let identity = parse_issue_url(&self.url).ok_or_else(|| {
            AdapterReadError::Failed(format!(
                "#{}: GitHub returned a non-canonical issue URL ({})",
                self.number, self.url
            ))
        })?;
        let display_id = format!("gh-{}", self.number);
        if identity.display_id != display_id {
            return Err(AdapterReadError::Failed(format!(
                "#{}: GitHub returned an issue URL for a different number ({})",
                self.number, self.url
            )));
        }
        Ok(identity)
    }

    fn into_fields(
        self,
        identity: BackendItemIdentity,
        ticket_kind: TicketKind,
    ) -> Result<IssueFields, AdapterReadError> {
        let number = self.number;

        let status = match self.state.as_str() {
            "OPEN" => ItemStatus::Open,
            "CLOSED" => ItemStatus::Done,
            other => {
                return Err(AdapterReadError::Failed(format!(
                    "#{number}: unexpected issue state '{other}'"
                )));
            }
        };
        Ok(IssueFields {
            number,
            backend_key: identity.backend_key,
            ticket_kind,
            title: self.title,
            body: self.body,
            status,
        })
    }

    fn into_adopted_item(
        self,
        identity: BackendItemIdentity,
        ticket_kind: TicketKind,
    ) -> Result<AdoptedItem, AdapterReadError> {
        let issue = self.into_fields(identity, ticket_kind)?;
        Ok(AdoptedItem {
            display_id: format!("gh-{}", issue.number),
            backend_key: issue.backend_key,
            ticket_kind: issue.ticket_kind,
            title: issue.title,
            body: issue.body,
            status: issue.status,
        })
    }

    fn into_refresh(
        self,
        identity: BackendItemIdentity,
        ticket_kind: TicketKind,
    ) -> Result<BackendItemRefresh, AdapterReadError> {
        let issue = self.into_fields(identity, ticket_kind)?;
        Ok(BackendItemRefresh {
            title: issue.title,
            body: issue.body,
            status: issue.status,
            ticket_kind: Some(issue.ticket_kind),
        })
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
        || [host, owner, repository, number]
            .iter()
            .any(|segment| segment.contains(['?', '#']) || segment.chars().any(char::is_whitespace))
    {
        return None;
    }
    let number: i64 = number.parse().ok()?;
    if number <= 0 {
        return None;
    }
    let backend_key = url.to_owned();
    Some(BackendItemIdentity {
        display_id: format!("gh-{number}"),
        backend_key,
    })
}

/// Parse the canonical one-line URL receipt emitted by `gh issue create`.
///
/// The URL itself becomes the Backend key, retaining host and repository
/// identity without a separate Remote setting (ADR-0033).
fn parse_create_receipt(stdout: &[u8]) -> Option<BackendItemIdentity> {
    let receipt = std::str::from_utf8(stdout).ok()?.trim();
    if receipt.lines().count() != 1 {
        return None;
    }
    parse_issue_url(receipt)
}

fn create_failure_detail(output: &RunOutput, stderr: &str) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("{stderr}\nUnrecognized stdout receipt: {stdout}"),
        (false, true) => stderr.to_owned(),
        (true, false) => format!("gh issue create returned an unrecognized receipt: {stdout}"),
        (true, true) => "gh issue create returned no GitHub issue URL receipt".to_owned(),
    }
}

fn certifies_auth_rejection(output: &RunOutput, stderr: &str) -> bool {
    if output.succeeded() || !output.stdout.is_empty() {
        return false;
    }
    let normalized = stderr.to_ascii_lowercase();
    normalized.starts_with("http 401: bad credentials (https://")
        && normalized.contains("/graphql)")
        && normalized.contains("\ntry authenticating with:  gh auth login -h ")
}

/// Classify a non-zero `gh` failure into a [`FailureClass`] (ADR-0016).
///
/// A conservative, spike-grounded set of mechanical anchors; everything
/// unmatched stays `Unknown` (the honest common case, since the GraphQL-backed
/// `gh issue` subcommands surface most failures as `GraphQL: <message>` with no
/// stable `HTTP <code>:` prefix). Precedence matters: a 403 collides between
/// auth and rate-limit, so the rate-limit anchors are tested first.
///
/// `exit_code` is part of the ADR-0016 contract but unused: `gh` exits 1 for
/// almost everything and even exit-4-for-auth is unreliable (cli/cli#9338), so
/// the classification gates on stderr alone. `retry_after_s` stays `None` — gh
/// discards the rate-limit reset header from its stderr.
fn classify(_exit_code: i32, stderr: &str) -> FailureClass {
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
        class: classify(output.exit_code, &detail),
        detail,
        retry_after_s: None,
    })
}

fn bug_read_rejection(error: &AdapterReadError) -> BackendCreateOutcome {
    BackendCreateOutcome::rejected(format!("could not resolve Bug representation: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend_operation::BackendItemAddress;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{EpicRef, MutationPayload, StatusChange, TitleBody};
    use crate::domain::mutation_type::MutationType;
    use crate::proc::{FakeRunner, ProcError, RunOutput};
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        std::env::current_dir().unwrap()
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
        let argv = [
            "gh".to_owned(),
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={query}"),
            "-f".to_owned(),
            format!("owner={owner}"),
            "-f".to_owned(),
            format!("name={name}"),
            "-F".to_owned(),
            format!("after={}", after.unwrap_or("null")),
        ];
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        runner.expect_exact(&argv, ok(response));
    }

    fn expect_bug_create(
        runner: &FakeRunner,
        repository_id: &str,
        title: &str,
        body: &str,
        representation: &str,
        response: &str,
    ) {
        let argv = [
            "gh".to_owned(),
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={CREATE_BUG_QUERY}"),
            "-f".to_owned(),
            format!("repositoryId={repository_id}"),
            "-f".to_owned(),
            format!("title={title}"),
            "-f".to_owned(),
            format!("body={body}"),
            "-f".to_owned(),
            representation.to_owned(),
        ];
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        runner.expect_exact(&argv, ok(response));
    }

    /// Build a `gh issue view --json` object. `issue_type` is either `"null"`
    /// or a type name; the extra object fields exercise serde's field-skipping.
    fn issue_json(number: i64, state: &str, issue_type: &str, url: &str) -> String {
        issue_json_with_labels(number, state, issue_type, url, "[]")
    }

    fn issue_json_with_labels(
        number: i64,
        state: &str,
        issue_type: &str,
        url: &str,
        labels: &str,
    ) -> String {
        let it = if issue_type == "null" {
            "null".to_string()
        } else {
            format!(r#"{{"id":"IT_x","name":"{issue_type}","description":"d","color":"RED"}}"#)
        };
        format!(
            r#"{{"number":{number},"title":"T{number}","body":"B","state":"{state}","issueType":{it},"labels":{labels},"updatedAt":"2026-06-20T00:00:00Z","url":"{url}"}}"#
        )
    }

    fn edit(mt: MutationType, payload: MutationPayload, key: Option<&str>) -> BackendEdit {
        let target = address(key.expect("adapter tests provide a target identity"));
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
            (MutationType::SetItemStatus, MutationPayload::ItemStatus(change)) => {
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

    fn create(mt: MutationType, title: &str, body: &str) -> BackendCreate {
        let snapshot = TitleBody {
            title: title.into(),
            body: body.into(),
        };
        match mt {
            MutationType::PromoteTicket => BackendCreate::Ticket {
                snapshot,
                ticket_kind: TicketKind::Task,
            },
            MutationType::PromoteEpic => BackendCreate::Epic { snapshot },
            _ => panic!("not a Promotion Mutation Type"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let s = adapter.adopt_ticket("42").unwrap();
        assert_eq!(s.backend_key, "https://github.com/o/r/issues/42");
        assert_eq!(s.display_id, "gh-42");
        assert_eq!(s.ticket_kind, TicketKind::Task);
        assert_eq!(s.status, ItemStatus::Open);
        assert_eq!(s.title, "T42");
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_maps_closed_to_done_and_bug_kind() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "7", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                7,
                "CLOSED",
                "Bug",
                "https://github.com/o/r/issues/7",
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let s = adapter.refresh_item("7").unwrap();
        assert_eq!(s.status, ItemStatus::Done);
        assert_eq!(s.ticket_kind, Some(TicketKind::Bug));
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.inspect_item("42").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_maps_non_bug_and_null_issue_type_to_task() {
        for it in ["Feature", "null", "CustomOrgType"] {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
                ok(&issue_json(
                    1,
                    "OPEN",
                    it,
                    "https://github.com/o/r/issues/1",
                )),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let s = adapter.refresh_item("1").unwrap();
            assert_eq!(
                s.ticket_kind,
                Some(TicketKind::Task),
                "issueType {it} → Task"
            );
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn refresh_maps_typeless_bug_label_to_bug_in_a_user_repository() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json_with_labels(
                1,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/1",
                r#"[{"name":"BUG"}]"#,
            )),
        );
        runner.expect_exact(
            &[
                "gh",
                "repo",
                "view",
                "https://github.com/o/r",
                "--json",
                "isInOrganization",
            ],
            ok(r#"{"isInOrganization":false}"#),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        assert_eq!(
            adapter.refresh_item("1").unwrap().ticket_kind,
            Some(TicketKind::Bug)
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_maps_typeless_bug_label_to_task_in_an_organization_repository() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json_with_labels(
                1,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/1",
                r#"[{"name":"bug"}]"#,
            )),
        );
        runner.expect_exact(
            &[
                "gh",
                "repo",
                "view",
                "https://github.com/o/r",
                "--json",
                "isInOrganization",
            ],
            ok(r#"{"isInOrganization":true}"#),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        assert_eq!(
            adapter.refresh_item("1").unwrap().ticket_kind,
            Some(TicketKind::Task)
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_native_issue_type_wins_over_a_bug_label() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json_with_labels(
                1,
                "OPEN",
                "Feature",
                "https://github.com/o/r/issues/1",
                r#"[{"name":"bug"}]"#,
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        assert_eq!(
            adapter.refresh_item("1").unwrap().ticket_kind,
            Some(TicketKind::Task)
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_rejects_a_key_that_resolves_to_another_issue() {
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let AdapterReadError::Failed(detail) = adapter.refresh_item(key).unwrap_err() else {
            panic!("redirected identity must be a Backend read failure")
        };
        assert!(detail.contains("resolved"));
        assert!(detail.contains("issues/2"));
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        match adapter.adopt_ticket("99").unwrap_err() {
            AdapterReadError::Failed(d) => assert!(d.contains("#99 is a pull request"), "{d}"),
            AdapterReadError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_non_zero_exit_is_pull_failed_with_stderr() {
        // Verbatim not-found stderr observed in the tk-gh-playground spike
        // (docs/spikes/gh-cli-issue-behavior.md).
        let stderr = "GraphQL: Could not resolve to an issue or pull request \
                      with the number of 5. (repository.issue)";
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "5", "--json", ISSUE_JSON_FIELDS],
            fail(1, stderr),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        match adapter.refresh_item("5").unwrap_err() {
            AdapterReadError::Failed(d) => assert_eq!(d, stderr),
            AdapterReadError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_spawn_failure_is_pull_env() {
        let runner = FakeRunner::new();
        runner.expect_exact_error(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ProcError::ExecutableNotFound,
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.refresh_item("1").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_calls_are_independent_and_preserve_order() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                1,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/1",
            )),
        );
        runner.expect_exact(
            &["gh", "issue", "view", "2", "--json", ISSUE_JSON_FIELDS],
            ok(&issue_json(
                2,
                "CLOSED",
                "null",
                "https://github.com/o/r/issues/2",
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let first = adapter.refresh_item("1").unwrap();
        let second = adapter.refresh_item("2").unwrap();
        assert_eq!(first.status, ItemStatus::Open);
        assert_eq!(second.status, ItemStatus::Done);
        runner.assert_all_consumed();
    }

    #[test]
    fn refresh_unparseable_json_is_pull_failed() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "view", "1", "--json", ISSUE_JSON_FIELDS],
            ok("not json"),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.refresh_item("1").unwrap_err(),
            AdapterReadError::Failed(_)
        ));
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = edit(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "New".into(),
                body: "Body".into(),
            }),
            Some("42"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_set_status_open_or_active_reopens() {
        for status in ["open", "active"] {
            let runner = FakeRunner::new();
            runner.expect_exact(&["gh", "issue", "reopen", "42"], ok(""));
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let v = edit(
                MutationType::SetItemStatus,
                MutationPayload::ItemStatus(StatusChange {
                    status: status.into(),
                }),
                Some("42"),
            );
            assert!(
                matches!(
                    adapter.apply_edit(&v).unwrap(),
                    BackendEditOutcome::Acknowledged
                ),
                "{status}"
            );
            runner.assert_all_consumed();
        }
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = edit(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "T".into(),
                body: String::new(),
            }),
            Some("42"),
        );
        match adapter.apply_edit(&v).unwrap() {
            BackendEditOutcome::Rejected(f) => {
                assert_eq!(f.class, FailureClass::Validation);
                assert!(f.detail.contains("Validation Failed"));
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = edit(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::UpdateEpic,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "Epic".into(),
                body: "Plan".into(),
            }),
            Some("42"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::AddTicketToEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            Some("42"),
        );
        assert_eq!(
            adapter.apply_edit(&edit).unwrap(),
            BackendEditOutcome::Acknowledged
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_add_ticket_to_epic_exit_zero_with_stderr_is_acknowledged() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "42", "--parent", "9"],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: b"! Issue #42 already has parent #9".to_vec(),
            },
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::AddTicketToEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            Some("42"),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::RemoveTicketFromEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            Some("42"),
        );
        assert_eq!(
            adapter.apply_edit(&edit).unwrap(),
            BackendEditOutcome::Acknowledged
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_epic_membership_non_zero_is_classified_rejection() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "42", "--parent", "9"],
            fail(1, "HTTP 422: parent must be an issue"),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::AddTicketToEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            Some("42"),
        );
        match adapter.apply_edit(&edit).unwrap() {
            BackendEditOutcome::Rejected(failure) => {
                assert_eq!(failure.class, FailureClass::Validation);
                assert_eq!(failure.detail, "HTTP 422: parent must be an issue");
                assert_eq!(failure.retry_after_s, None);
            }
            BackendEditOutcome::Acknowledged => panic!("expected rejection"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_epic_membership_spawn_failure_is_apply_error() {
        let runner = FakeRunner::new();
        runner.expect_exact_error(
            &["gh", "issue", "edit", "42", "--remove-parent"],
            ProcError::SpawnFailed,
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let edit = edit(
            MutationType::RemoveTicketFromEpic,
            MutationPayload::EpicRef(EpicRef {
                epic_id: "e".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_edit(&edit),
            Err(ProcError::SpawnFailed)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_add_dependency_edits_blocked_by() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--add-blocked-by", "9"],
            ok(""),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = dep_edit(MutationType::RemoveDependency, "5", "9");
        assert!(matches!(
            adapter.apply_edit(&v).unwrap(),
            BackendEditOutcome::Acknowledged
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_dependency_exit_zero_with_stderr_is_accepted() {
        // The idempotent re-link no-op: gh may print to stderr yet exit 0.
        // Success is judged by exit code, so this must read as Acknowledged.
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--add-blocked-by", "9"],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: b"! Issue already blocked by #9".to_vec(),
            },
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = dep_edit(MutationType::AddDependency, "5", "9");
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
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
    fn create_ticket_and_epic_use_the_same_exact_typeless_issue_argv() {
        for mt in [MutationType::PromoteTicket, MutationType::PromoteEpic] {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "issue", "create", "--title", "T", "--body", "Body"],
                ok("https://github.com/o/r/issues/42\n"),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let outcome = adapter.create_item(&create(mt, "T", "Body"));
            let BackendCreateOutcome::Created(identity) = outcome else {
                panic!("{mt}: expected a confirmed receipt, got {outcome:?}");
            };
            assert_eq!(identity.backend_key, "https://github.com/o/r/issues/42");
            assert_eq!(identity.display_id, "gh-42");
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn a_valid_receipt_wins_even_when_gh_exits_nonzero() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "create", "--title", "T", "--body", "B"],
            RunOutput {
                exit_code: 1,
                stdout: b"https://github.example/o/r/issues/7\n".to_vec(),
                stderr: b"a later CLI step failed".to_vec(),
            },
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        assert_eq!(
            adapter.create_item(&create(MutationType::PromoteTicket, "T", "B")),
            BackendCreateOutcome::Created(identity("https://github.example/o/r/issues/7"))
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn empty_and_malformed_success_receipts_are_indeterminate() {
        for stdout in ["", "created #42", "https://github.com/o/r/pull/42"] {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "issue", "create", "--title", "T", "--body", "B"],
                ok(stdout),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);

            let BackendCreateOutcome::Indeterminate(failure) =
                adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
            else {
                panic!("{stdout:?}: missing receipt must be indeterminate");
            };
            assert_eq!(failure.class, FailureClass::Unknown);
            assert!(failure.detail.contains("receipt"), "{}", failure.detail);
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn completed_nonzero_without_a_receipt_is_indeterminate() {
        for stderr in [
            "GraphQL: service unavailable",
            "HTTP 422: Validation Failed",
            "type \"Bug\" not found; available types:",
        ] {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "issue", "create", "--title", "T", "--body", "B"],
                fail(1, stderr),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);

            let BackendCreateOutcome::Indeterminate(failure) =
                adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
            else {
                panic!("{stderr}: completed nonzero is not certified no-effect");
            };
            assert_eq!(failure.detail, stderr);
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn authentication_rejection_is_certified_no_effect() {
        let stderr = "HTTP 401: Bad credentials (https://api.github.com/graphql)\n\
                      Try authenticating with:  gh auth login -h github.com";
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "create", "--title", "T", "--body", "B"],
            fail(1, stderr),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let BackendCreateOutcome::Rejected(failure) =
            adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
        else {
            panic!("the observed initial authentication failure certifies no effect");
        };
        assert_eq!(failure.detail, stderr);
        assert_eq!(failure.class, FailureClass::Auth);
        runner.assert_all_consumed();
    }

    #[test]
    fn auth_flavoured_text_alone_does_not_certify_creation_rejection() {
        for stderr in [
            "HTTP 401: Unauthorized",
            "GraphQL authentication failed: Bad credentials",
            "To get started, please run:  gh auth login",
        ] {
            let runner = FakeRunner::new();
            runner.expect_exact(
                &["gh", "issue", "create", "--title", "T", "--body", "B"],
                fail(1, stderr),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);

            let BackendCreateOutcome::Indeterminate(failure) =
                adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
            else {
                panic!("auth classification alone cannot certify no effect");
            };
            assert_eq!(failure.class, FailureClass::Auth);
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn pre_spawn_process_failures_are_certified_no_effect() {
        for err in [ProcError::ExecutableNotFound, ProcError::SpawnFailed] {
            let runner = FakeRunner::new();
            runner.expect_exact_error(
                &["gh", "issue", "create", "--title", "T", "--body", "B"],
                err,
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);

            let BackendCreateOutcome::Rejected(failure) =
                adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
            else {
                panic!("a pre-spawn error cannot create an issue");
            };
            assert!(failure.detail.starts_with("gh issue create did not start:"));
            assert_eq!(failure.class, FailureClass::Unknown);
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn post_spawn_process_failure_is_indeterminate() {
        let runner = FakeRunner::new();
        runner.expect_exact_error(
            &["gh", "issue", "create", "--title", "T", "--body", "B"],
            ProcError::OutcomeUnobserved,
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let BackendCreateOutcome::Indeterminate(failure) =
            adapter.create_item(&create(MutationType::PromoteTicket, "T", "B"))
        else {
            panic!("a started process may already have created an issue");
        };
        assert!(failure.detail.contains("started"));
        assert!(failure.detail.contains("outcome is unknown"));
        assert_eq!(failure.class, FailureClass::Unknown);
        runner.assert_all_consumed();
    }

    #[test]
    fn receipt_parser_requires_a_canonical_github_issue_url() {
        for invalid in [
            b"http://github.com/o/r/issues/1".as_slice(),
            b"https://github.com/o/r/issues/0",
            b"https://github.com/o/r/issues/01",
            b"https://github.com/o/r/issues/1?x=y",
            b"https://user@github.com/o/r/issues/1",
            b"https://github.com/o/r/issues/1\nextra",
            b"https://github.com/o/r/issues/not-a-number",
        ] {
            assert_eq!(parse_create_receipt(invalid), None, "{invalid:?}");
        }
        assert_eq!(
            parse_create_receipt(b" https://github.example/o/r/issues/12\n"),
            Some(identity("https://github.example/o/r/issues/12"))
        );
    }

    #[test]
    fn resolves_requested_static_facets_without_io() {
        let runner = FakeRunner::new();
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let caps = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none()
                    .with_item_class(ItemClass::Ticket)
                    .with_item_class(ItemClass::Epic)
                    .with_ticket_kind(TicketKind::Task)
                    .with_dependencies()
                    .with_epic_membership(),
            )
            .unwrap();
        assert!(caps.can_create_item_class(ItemClass::Ticket));
        assert!(caps.can_create_item_class(ItemClass::Epic));
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
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","isInOrganization":true}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

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
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","isInOrganization":true}"#),
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
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug),
            )
            .unwrap();
        assert!(capabilities.can_create_ticket_kind(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    #[test]
    fn uses_the_final_issue_type_page_before_rejecting_bug_capability() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","isInOrganization":true}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Task","isEnabled":true}],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR"}}}}}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_2","name":"Bug","isEnabled":false}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug),
            )
            .unwrap();
        assert!(!capabilities.can_create_ticket_kind(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    #[test]
    fn bug_capability_read_failure_is_not_reported_as_unsupported() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","isInOrganization":true}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            fail(1, "GraphQL: taxonomy read failed"),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        assert!(matches!(
            adapter.resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug)
            ),
            Err(AdapterReadError::Failed(detail)) if detail == "GraphQL: taxonomy read failed"
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn creates_bug_with_the_resolved_native_issue_type() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"o/r","isInOrganization":true}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Bug","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        expect_bug_create(
            &runner,
            "R_1",
            "Bug title",
            "Bug body",
            "issueTypeId=IT_1",
            r#"{"data":{"createIssue":{"issue":{"url":"https://github.com/o/r/issues/42","number":42}}}}"#,
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let outcome = adapter.create_item(&BackendCreate::Ticket {
            snapshot: TitleBody {
                title: "Bug title".into(),
                body: "Bug body".into(),
            },
            ticket_kind: TicketKind::Bug,
        });

        assert_eq!(
            outcome,
            BackendCreateOutcome::Created(identity("https://github.com/o/r/issues/42"))
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn creates_a_personal_bug_with_the_resolved_label() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"p/r","isInOrganization":false}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"labels":{"nodes":[{"id":"L_1","name":"bug"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        expect_bug_create(
            &runner,
            "R_1",
            "Bug title",
            "Bug body",
            "labelIds[]=L_1",
            r#"{"data":{"createIssue":{"issue":{"url":"https://github.com/p/r/issues/42","number":42}}}}"#,
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let outcome = adapter.create_item(&BackendCreate::Ticket {
            snapshot: TitleBody {
                title: "Bug title".into(),
                body: "Bug body".into(),
            },
            ticket_kind: TicketKind::Bug,
        });

        assert_eq!(
            outcome,
            BackendCreateOutcome::Created(identity("https://github.com/p/r/issues/42"))
        );
        runner.assert_all_consumed();
    }

    #[test]
    fn resolves_an_existing_bug_label_for_a_user_repository() {
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "repo", "view", "--json", REPOSITORY_JSON_FIELDS],
            ok(r#"{"id":"R_1","nameWithOwner":"p/r","isInOrganization":false}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"issueTypes":{"nodes":[{"id":"IT_1","name":"Task","isEnabled":true}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        runner.expect_prefix(
            &["gh", "api", "graphql"],
            ok(r#"{"data":{"repository":{"labels":{"nodes":[{"id":"L_1","name":"BUG"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);

        let capabilities = adapter
            .resolve_promotion_capabilities(
                PromotionRequirements::none().with_ticket_kind(TicketKind::Bug),
            )
            .unwrap();
        assert!(capabilities.can_create_ticket_kind(TicketKind::Bug));
        runner.assert_all_consumed();
    }

    // ---- classify / PR guard -------------------------------------------

    #[test]
    fn classify_uses_mechanical_anchors_with_rate_limit_precedence() {
        assert_eq!(
            classify(1, "API rate limit exceeded for user"),
            FailureClass::RateLimited
        );
        assert_eq!(
            classify(1, "You have exceeded a secondary rate limit"),
            FailureClass::RateLimited
        );
        assert_eq!(classify(1, "HTTP 401: Bad credentials"), FailureClass::Auth);
        assert_eq!(
            classify(4, "To get started, please run:  gh auth login"),
            FailureClass::Auth
        );
        assert_eq!(
            classify(1, "HTTP 422: Validation Failed"),
            FailureClass::Validation
        );
        assert_eq!(
            classify(1, "HTTP 503: Service Unavailable"),
            FailureClass::Transient
        );
        assert_eq!(
            classify(1, "some unrecognised error"),
            FailureClass::Unknown
        );
        // A 403 collides between auth and rate-limit; rate-limit wins.
        assert_eq!(
            classify(1, "HTTP 403: API rate limit exceeded"),
            FailureClass::RateLimited
        );
        // Verbatim gh outputs observed in the tk-gh-playground spike
        // (docs/spikes/gh-cli-issue-behavior.md).
        assert_eq!(
            classify(
                1,
                "HTTP 401: Bad credentials (https://api.github.com/graphql)\nTry authenticating with:  gh auth login -h github.com"
            ),
            FailureClass::Auth
        );
        assert_eq!(
            classify(
                1,
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
