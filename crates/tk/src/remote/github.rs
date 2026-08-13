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
//! `gh issue view`. There is no listing and no discovery (ADR-0034).

use std::path::Path;

use serde::Deserialize;

use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{
    AdoptedItem, BackendCreate, BackendEdit, BackendItemIdentity, BackendItemInspection,
    BackendItemRefresh,
};
use crate::domain::backend_outcome::{
    BackendCreateOutcome, BackendEditOutcome, Failure, FailureClass,
};
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::proc::{ProcRunner, RunOutput};

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// The `--json` field set tk requests from `gh issue view`. `url` supplies the
/// canonical Adopt identity, guards Pull identity, and rejects pull requests
/// (see [`is_pull_request_url`]); `state` arrives UPPERCASE; `issueType` is an
/// object-or-null.
const ISSUE_JSON_FIELDS: &str = "number,title,body,state,issueType,url";

/// GitHub Backend Adapter. Holds the injected runner and the command cwd from
/// which `gh` resolves the repository (ADR-0033). Stateless beyond those
/// borrows — `&mut self` is the trait's shape, not a need of this adapter.
pub struct GithubAdapter<'a> {
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
}

impl<'a> GithubAdapter<'a> {
    #[must_use]
    pub fn new(runner: &'a dyn ProcRunner, cwd: &'a Path) -> Self {
        Self { runner, cwd }
    }
}

impl Adapter for GithubAdapter<'_> {
    fn backend_kind(&self) -> BackendKind {
        BackendKind::Github
    }

    fn adopt_ticket(&mut self, input: &str) -> Result<AdoptedItem, AdapterReadError> {
        self.view_issue(input)?.into_adopted_item()
    }

    fn refresh_item(&mut self, key: &str) -> Result<BackendItemRefresh, AdapterReadError> {
        let issue = self.view_issue(key)?;
        if !issue.matches_key(key) {
            return Err(AdapterReadError::Failed(format!(
                "GitHub resolved '{key}' to a different issue ({})",
                issue.url
            )));
        }
        issue.into_refresh()
    }

    fn inspect_item(&mut self, key: &str) -> Result<BackendItemInspection, AdapterReadError> {
        self.view_issue(key)?.into_inspection()
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

    fn promotion_capabilities(&self) -> PromotionCapabilities {
        PromotionCapabilities::none()
            .with_item_class(crate::domain::item_class::ItemClass::Ticket)
            .with_item_class(crate::domain::item_class::ItemClass::Epic)
            .with_ticket_kind(TicketKind::Task)
            .with_dependencies()
            .with_epic_membership()
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

    /// Create the GitHub issue represented by either Promotion variant.
    ///
    /// GitHub Tickets and Epics share one typeless issue surface; structure is
    /// applied later through the relationship Mutations ordered behind their
    /// Promotions. The receipt is authoritative even when `gh` exits non-zero.
    fn create_issue(&self, create: &BackendCreate) -> BackendCreateOutcome {
        let snapshot = match create {
            BackendCreate::Ticket { snapshot } | BackendCreate::Epic { snapshot } => snapshot,
        };
        let output = match self.runner.run(
            &[
                "gh",
                "issue",
                "create",
                "--title",
                &snapshot.title,
                "--body",
                &snapshot.body,
            ],
            self.cwd,
        ) {
            Ok(output) => output,
            Err(
                err @ (crate::proc::ProcError::ExecutableNotFound
                | crate::proc::ProcError::SpawnFailed),
            ) => {
                return BackendCreateOutcome::Rejected(Failure {
                    detail: format!("gh issue create did not start: {err}"),
                    class: FailureClass::Unknown,
                    retry_after_s: None,
                });
            }
            Err(err @ crate::proc::ProcError::OutcomeUnobserved) => {
                return BackendCreateOutcome::Indeterminate(Failure {
                    detail: format!("gh issue create started, but its outcome is unknown: {err}"),
                    class: FailureClass::Unknown,
                    retry_after_s: None,
                });
            }
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
    /// Canonical issue/PR URL used for identity and the PR guard.
    url: String,
}

/// The `issueType` object; `null` for an untyped issue or a repo without issue
/// types. Only `name` is read (ADR-0021 issue-type → Ticket Kind mapping).
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

struct IssueContent {
    number: i64,
    identity: BackendItemIdentity,
    title: String,
    body: String,
}

impl GhIssue {
    fn matches_key(&self, key: &str) -> bool {
        key == self.number.to_string() || key == self.url
    }

    fn into_fields(self) -> Result<IssueFields, AdapterReadError> {
        let state = self.state.clone();
        let issue_type = self.issue_type.as_ref().map(|value| value.name.clone());
        let issue = self.into_content()?;
        let number = issue.number;

        let status = match state.as_str() {
            "OPEN" => ItemStatus::Open,
            "CLOSED" => ItemStatus::Done,
            other => {
                return Err(AdapterReadError::Failed(format!(
                    "#{number}: unexpected issue state '{other}'"
                )));
            }
        };
        // "Bug" → Bug; every other value ("Task", "Feature", org-custom) and a
        // typeless issue → Task, matching the closed two-variant TicketKind
        // (ADR-0021). This read-only mapping never writes `--type`.
        let ticket_kind = match issue_type.as_deref() {
            Some("Bug") => TicketKind::Bug,
            _ => TicketKind::Task,
        };
        Ok(IssueFields {
            number,
            backend_key: issue.identity.backend_key,
            ticket_kind,
            title: issue.title,
            body: issue.body,
            status,
        })
    }

    fn into_content(self) -> Result<IssueContent, AdapterReadError> {
        // PR guard (ADR-0034): `gh issue view <n>` resolves a pull request too
        // (issue and PR numbers share one sequence) and returns it as an
        // issue-shaped object, so reject when the canonical url is a /pull/<n>
        // path. tk has no PR concept; the user meant an issue.
        if is_pull_request_url(&self.url) {
            return Err(AdapterReadError::Failed(format!(
                "#{} is a pull request, not an issue",
                self.number
            )));
        }
        Ok(IssueContent {
            number: self.number,
            identity: BackendItemIdentity {
                backend_key: self.url,
                display_id: format!("gh-{}", self.number),
            },
            title: self.title,
            body: self.body,
        })
    }

    fn into_adopted_item(self) -> Result<AdoptedItem, AdapterReadError> {
        let issue = self.into_fields()?;
        Ok(AdoptedItem {
            display_id: format!("gh-{}", issue.number),
            backend_key: issue.backend_key,
            ticket_kind: issue.ticket_kind,
            title: issue.title,
            body: issue.body,
            status: issue.status,
        })
    }

    fn into_refresh(self) -> Result<BackendItemRefresh, AdapterReadError> {
        let issue = self.into_fields()?;
        Ok(BackendItemRefresh {
            title: issue.title,
            body: issue.body,
            status: issue.status,
            ticket_kind: Some(issue.ticket_kind),
        })
    }

    fn into_inspection(self) -> Result<BackendItemInspection, AdapterReadError> {
        let issue = self.into_content()?;
        Ok(BackendItemInspection {
            identity: issue.identity,
            title: issue.title,
            body: issue.body,
        })
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

/// Parse the canonical one-line URL receipt emitted by `gh issue create`.
///
/// The URL itself becomes the Backend key, retaining host and repository
/// identity without a separate Remote setting (ADR-0033). Requiring HTTPS and
/// the exact `<host>/<owner>/<repo>/issues/<positive number>` shape rejects
/// prose, API URLs, pull requests, and query or fragment suffixes.
fn parse_create_receipt(stdout: &[u8]) -> Option<BackendItemIdentity> {
    let receipt = std::str::from_utf8(stdout).ok()?.trim();
    if receipt.lines().count() != 1 {
        return None;
    }
    let mut segments = receipt.split('/');
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
    let backend_key = receipt.to_owned();
    Some(BackendItemIdentity {
        display_id: format!("gh-{number}"),
        backend_key,
    })
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
            MutationType::PromoteTicket => BackendCreate::Ticket { snapshot },
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
                assert!(detail.contains("#99 is a pull request"), "{detail}")
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
    fn github_declares_the_shipped_promotion_facets() {
        let runner = FakeRunner::new();
        let cwd = cwd();
        let adapter = GithubAdapter::new(&runner, &cwd);
        let caps = adapter.promotion_capabilities();
        assert!(caps.can_create_item_class(ItemClass::Ticket));
        assert!(caps.can_create_item_class(ItemClass::Epic));
        assert!(caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(caps.can_represent_dependencies());
        assert!(caps.can_represent_epic_membership());
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
