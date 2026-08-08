//! GitHub Backend Adapter over the `gh` CLI (ADR-0021 fields, ADR-0034 opt-in).
//!
//! Implements [`Adapter`] by shelling out to `gh issue` through an injected
//! [`ProcRunner`] (ADR-0031: the real subprocess only in production, a
//! `FakeRunner` in tests). The repository is resolved from the command cwd —
//! no `--repo`, no stored repo (ADR-0033): `gh` reads the GitHub repo from the
//! checkout's git remote, stable across Workspaces.
//!
//! Pull is refresh-by-key: the engine hands the Adopted working set's active
//! keys to [`GithubAdapter::refresh_item`], which fetches each with one
//! `gh issue view`. There is no listing and no discovery (ADR-0034).

use std::path::Path;

use serde::Deserialize;

use crate::domain::apply_outcome::{ApplyOutcome, Failure, FailureClass};
use crate::domain::backend_kind::BackendKind;
use crate::domain::backend_operation::{AdoptedItem, BackendItemRefresh};
use crate::domain::mutation_payload::MutationPayload;
use crate::domain::mutation_type::MutationType;
use crate::domain::mutation_view::MutationView;
use crate::domain::promotion_capability::PromotionCapabilities;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::proc::{ProcRunner, RunOutput};

use super::adapter::{Adapter, AdapterReadError, ApplyError};

/// The `--json` field set tk requests from `gh issue view`. `url` is fetched
/// only for the PR guard (see [`is_pull_request_url`]) and is not stored on the
/// directional result; `state` arrives UPPERCASE; `issueType` is an
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

    /// Run one apply `gh` invocation and map its outcome. Success is judged by
    /// **exit code 0**, never by stderr emptiness: `gh issue close`/`reopen`
    /// print an informational "is already closed/open" line to stderr on their
    /// idempotent no-op path yet still exit 0, so a harmless re-apply must read
    /// as Accepted. A non-zero exit is a per-Mutation rejection carrying the
    /// classified stderr.
    fn run_apply(&self, argv: &[&str]) -> Result<ApplyOutcome, ApplyError> {
        let output = self.runner.run(argv, self.cwd)?;
        if output.succeeded() {
            return Ok(ApplyOutcome::accepted());
        }
        let detail = stderr_string(&output);
        let class = classify(output.exit_code, &detail);
        Ok(ApplyOutcome::Rejected(Failure {
            detail,
            class,
            retry_after_s: None,
        }))
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
        self.view_issue(key)?.into_refresh()
    }

    fn apply_mutation(
        &mut self,
        view: &MutationView,
        _now: &str,
    ) -> Result<ApplyOutcome, ApplyError> {
        match view.mutation_type {
            MutationType::UpdateTicket => {
                let MutationPayload::UpdateTitleBody(tb) = &view.payload else {
                    unreachable!(
                        "load_applicable_mutations pairs update_ticket with UpdateTitleBody"
                    )
                };
                let Some(number) = backend_number(view) else {
                    return Ok(target_has_no_backend_identity());
                };
                self.run_apply(&[
                    "gh", "issue", "edit", number, "--title", &tb.title, "--body", &tb.body,
                ])
            }
            MutationType::SetItemStatus => {
                let MutationPayload::ItemStatus(change) = &view.payload else {
                    unreachable!("load_applicable_mutations pairs set_item_status with ItemStatus")
                };
                // done is terminal (ADR-0006), so reopen-from-done never occurs.
                let verb = match change.status.as_str() {
                    "done" => "close",
                    "open" | "active" => "reopen",
                    other => {
                        return Ok(ApplyOutcome::rejected(format!(
                            "unexpected target status '{other}'"
                        )));
                    }
                };
                let Some(number) = backend_number(view) else {
                    return Ok(target_has_no_backend_identity());
                };
                self.run_apply(&["gh", "issue", verb, number])
            }
            // Dependency sync (ADR-0021, tk-107): the blocked issue is the
            // Mutation's item; `--add-blocked-by`/`--remove-blocked-by` take the
            // blocking issue's number, resolved store-side onto
            // `counterpart_backend_key`. Native `gh issue edit` flags, so the
            // adapter requires `gh` >= 2.94.0; an older `gh` rejects the unknown
            // flag and the Mutation fails like any other (no longer no-op).
            MutationType::AddDependency => {
                let Some(number) = backend_number(view) else {
                    return Ok(target_has_no_backend_identity());
                };
                let Some(counterpart) = counterpart_number(view) else {
                    return Ok(blocking_item_has_no_backend_identity());
                };
                self.run_apply(&[
                    "gh",
                    "issue",
                    "edit",
                    number,
                    "--add-blocked-by",
                    counterpart,
                ])
            }
            MutationType::RemoveDependency => {
                let Some(number) = backend_number(view) else {
                    return Ok(target_has_no_backend_identity());
                };
                let Some(counterpart) = counterpart_number(view) else {
                    return Ok(blocking_item_has_no_backend_identity());
                };
                self.run_apply(&[
                    "gh",
                    "issue",
                    "edit",
                    number,
                    "--remove-blocked-by",
                    counterpart,
                ])
            }
            // No-op Accepted is ADR-0021's shape for a facet GitHub does not
            // yet sync: Epic-membership Apply is deferred to tk-132 and Epic
            // creation to tk-137. GitHub's `promotion_capabilities()` declares
            // neither representable, so ADR-0036 preflight refuses any
            // Promotion that would create a GitHub Backend Epic or membership
            // in one before either Mutation reaches the outbox — this arm never
            // sees one naming a real backend parent. Accepting keeps the queue
            // draining instead of wedging on a permanent rejection.
            MutationType::UpdateEpic
            | MutationType::AddTicketToEpic
            | MutationType::RemoveTicketFromEpic => Ok(ApplyOutcome::accepted()),
            // Preflight (ADR-0036) should catch a Promotion Mutation before
            // Apply, since GitHub's promotion_capabilities() declares
            // nothing. This arm is the safe fallback if that seam is
            // skipped — a rejection, not a panic, over reachable state.
            MutationType::PromoteTicket | MutationType::PromoteEpic => Ok(ApplyOutcome::rejected(
                "the GitHub Backend Adapter cannot apply a Promotion in this build",
            )),
            // load_applicable_mutations rejects these payload-less kinds before
            // they reach any adapter.
            MutationType::AddExternalBlocker | MutationType::ResolveExternalBlocker => {
                unreachable!("load_applicable_mutations filters payload-less mutation kinds")
            }
        }
    }

    fn promotion_capabilities(&self) -> PromotionCapabilities {
        // ADR-0036 "Backend capability is declared per facet and staged":
        // GitHub declares no Item Class, Ticket Kind, Dependency, or Epic
        // membership yet. tk-137 turns on Task and Epic creation once `gh
        // issue create` is implemented; tk-132 turns on Epic membership once
        // sub-issue Apply is implemented.
        PromotionCapabilities::none()
    }
}

impl GithubAdapter<'_> {
    /// Fetch and parse one GitHub issue view through the checkout-resolved Remote.
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

/// The GitHub issue number for a Mutation's target Item, absent while that
/// Item has no backend identity. A Pending Promotion Item keeps appending
/// ordinary Mutations behind its Promotion (ADR-0036), so a Mutation can reach
/// Apply before the receipt that assigns its key.
fn backend_number(view: &MutationView) -> Option<&str> {
    view.backend_key.as_deref()
}

/// The GitHub issue number of a dependency Mutation's Blocking Item, resolved
/// store-side onto `counterpart_backend_key`. Absent for the same reason
/// [`backend_number`] is.
fn counterpart_number(view: &MutationView) -> Option<&str> {
    view.counterpart_backend_key.as_deref()
}

/// The rejection every Apply arm returns for a target Item whose Promotion has
/// not yet been applied. One constructor so the wording is identical across
/// arms and still grep-able as a literal.
fn target_has_no_backend_identity() -> ApplyOutcome {
    ApplyOutcome::rejected(
        "the target Item has no backend identity yet; its Promotion has not yet been applied",
    )
}

/// The counterpart of [`target_has_no_backend_identity`], for a dependency
/// Mutation whose Blocking Item is still unpromoted.
fn blocking_item_has_no_backend_identity() -> ApplyOutcome {
    ApplyOutcome::rejected(
        "the Blocking Item has no backend identity yet; its Promotion has not yet been applied",
    )
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
    /// Canonical issue/PR url; consumed only by the PR guard, never stored.
    url: String,
}

/// The `issueType` object; `null` for an untyped issue or a repo without issue
/// types. Only `name` is read (ADR-0021 issue-type → Ticket Kind mapping).
#[derive(Debug, Deserialize)]
struct GhIssueType {
    name: String,
}

struct NormalizedIssue {
    backend_key: String,
    ticket_kind: TicketKind,
    title: String,
    body: String,
    status: ItemStatus,
}

impl GhIssue {
    fn normalize(self) -> Result<NormalizedIssue, AdapterReadError> {
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
        let status = match self.state.as_str() {
            "OPEN" => ItemStatus::Open,
            "CLOSED" => ItemStatus::Done,
            other => {
                return Err(AdapterReadError::Failed(format!(
                    "#{}: unexpected issue state '{other}'",
                    self.number
                )));
            }
        };
        // "Bug" → Bug; every other value ("Task", "Feature", org-custom) and a
        // typeless issue → Task, matching the closed two-variant TicketKind
        // (ADR-0021). This read-only mapping never writes `--type`.
        let ticket_kind = match self.issue_type.as_ref().map(|t| t.name.as_str()) {
            Some("Bug") => TicketKind::Bug,
            _ => TicketKind::Task,
        };
        Ok(NormalizedIssue {
            backend_key: self.number.to_string(),
            ticket_kind,
            title: self.title,
            body: self.body,
            status,
        })
    }

    fn into_adopted_item(self) -> Result<AdoptedItem, AdapterReadError> {
        let issue = self.normalize()?;
        Ok(AdoptedItem {
            display_id: format!("gh-{}", issue.backend_key),
            backend_key: issue.backend_key,
            ticket_kind: issue.ticket_kind,
            title: issue.title,
            body: issue.body,
            status: issue.status,
        })
    }

    fn into_refresh(self) -> Result<BackendItemRefresh, AdapterReadError> {
        let issue = self.normalize()?;
        Ok(BackendItemRefresh {
            title: issue.title,
            body: issue.body,
            status: issue.status,
            ticket_kind: Some(issue.ticket_kind),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_payload::{
        DependencyRef, EpicRef, Promotion, StatusChange, TitleBody,
    };
    use crate::proc::{ErrorInjectingRunner, FakeRunner, ProcError, RunOutput};
    use std::path::PathBuf;

    const NOW: &str = "2026-06-20T00:00:00Z";

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

    fn view(mt: MutationType, payload: MutationPayload, key: Option<&str>) -> MutationView {
        MutationView {
            sequence: 1,
            mutation_type: mt,
            item_id: "t1".into(),
            item_class: ItemClass::Ticket,
            payload,
            backend_kind: Some("github".into()),
            backend_key: key.map(str::to_string),
            counterpart_backend_key: None,
        }
    }

    /// A dependency Mutation `view`, carrying the Blocking Item's resolved
    /// backend number on `counterpart_backend_key` (the store-side resolution
    /// the adapter relies on).
    fn dep_view(mt: MutationType, blocked: &str, blocking: &str) -> MutationView {
        MutationView {
            counterpart_backend_key: Some(blocking.into()),
            ..view(
                mt,
                MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "blocking-internal-id".into(),
                }),
                Some(blocked),
            )
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
        assert_eq!(s.backend_key, "42");
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
        let runner = ErrorInjectingRunner {
            err: ProcError::ExecutableNotFound,
        };
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.refresh_item("1").unwrap_err(),
            AdapterReadError::Env(ProcError::ExecutableNotFound)
        ));
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

    // ---- apply_mutation -------------------------------------------------

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
        let v = view(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "New".into(),
                body: "Body".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_set_status_done_closes() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["gh", "issue", "close", "42"], ok(""));
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = view(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
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
            let v = view(
                MutationType::SetItemStatus,
                MutationPayload::ItemStatus(StatusChange {
                    status: status.into(),
                }),
                Some("42"),
            );
            assert!(
                matches!(
                    adapter.apply_mutation(&v, NOW).unwrap(),
                    ApplyOutcome::Accepted(_)
                ),
                "{status}"
            );
            runner.assert_all_consumed();
        }
    }

    #[test]
    fn apply_exit_zero_with_stderr_is_accepted() {
        // gh's idempotent "already closed" no-op exits 0 but prints to stderr.
        // Success is judged by exit code, so this must be Accepted, not rejected.
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
        let v = view(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
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
        let v = view(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "T".into(),
                body: String::new(),
            }),
            Some("42"),
        );
        match adapter.apply_mutation(&v, NOW).unwrap() {
            ApplyOutcome::Rejected(f) => {
                assert_eq!(f.class, FailureClass::Validation);
                assert!(f.detail.contains("Validation Failed"));
                assert_eq!(f.retry_after_s, None);
            }
            ApplyOutcome::Accepted(_) => panic!("expected rejection"),
        }
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_spawn_failure_is_apply_error() {
        let runner = ErrorInjectingRunner {
            err: ProcError::SpawnFailed,
        };
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = view(
            MutationType::SetItemStatus,
            MutationPayload::ItemStatus(StatusChange {
                status: "done".into(),
            }),
            Some("42"),
        );
        assert!(matches!(
            adapter.apply_mutation(&v, NOW),
            Err(ProcError::SpawnFailed)
        ));
    }

    #[test]
    fn apply_epic_membership_mutations_are_noop_accepted_without_a_call() {
        // Epic-membership sync stays deferred to a Promote-gated ticket
        // (ADR-0021): no backend Epic exists pre-Promotion. FakeRunner has no
        // expectations, so any gh call panics — Accepted proves no subprocess.
        let cases = [
            (
                MutationType::UpdateEpic,
                MutationPayload::UpdateTitleBody(TitleBody {
                    title: "E".into(),
                    body: String::new(),
                }),
            ),
            (
                MutationType::AddTicketToEpic,
                MutationPayload::EpicRef(EpicRef {
                    epic_id: "e".into(),
                }),
            ),
            (
                MutationType::RemoveTicketFromEpic,
                MutationPayload::EpicRef(EpicRef {
                    epic_id: "e".into(),
                }),
            ),
        ];
        for (mt, payload) in cases {
            let runner = FakeRunner::new();
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let v = view(mt, payload, Some("42"));
            assert!(
                matches!(
                    adapter.apply_mutation(&v, NOW).unwrap(),
                    ApplyOutcome::Accepted(_)
                ),
                "{mt}"
            );
        }
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
        let v = dep_view(MutationType::AddDependency, "5", "9");
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
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
        let v = dep_view(MutationType::RemoveDependency, "5", "9");
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_dependency_exit_zero_with_stderr_is_accepted() {
        // The idempotent re-link no-op: gh may print to stderr yet exit 0.
        // Success is judged by exit code, so this must read as Accepted.
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
        let v = dep_view(MutationType::AddDependency, "5", "9");
        assert!(matches!(
            adapter.apply_mutation(&v, NOW).unwrap(),
            ApplyOutcome::Accepted(_)
        ));
        runner.assert_all_consumed();
    }

    #[test]
    fn apply_dependency_non_zero_is_classified_rejection() {
        // A stale gh (< 2.94.0) rejects the unknown flag; the Mutation fails
        // like any other (no longer no-op-Accepted) and the queue wedges.
        let runner = FakeRunner::new();
        runner.expect_exact(
            &["gh", "issue", "edit", "5", "--add-blocked-by", "9"],
            fail(1, "unknown flag: --add-blocked-by"),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = dep_view(MutationType::AddDependency, "5", "9");
        match adapter.apply_mutation(&v, NOW).unwrap() {
            ApplyOutcome::Rejected(f) => {
                assert!(f.detail.contains("unknown flag"), "{}", f.detail);
                assert_eq!(f.class, FailureClass::Unknown);
            }
            ApplyOutcome::Accepted(_) => panic!("stale gh must reject, not no-op"),
        }
        runner.assert_all_consumed();
    }

    // ---- Promotion -------------------------------------------------------

    #[test]
    fn apply_promote_ticket_and_epic_are_rejected_without_a_call() {
        // GitHub's promotion_capabilities() declares nothing (ADR-0036), so
        // Apply must reject rather than spawn `gh`; FakeRunner has no
        // expectations, so any call panics.
        for mt in [MutationType::PromoteTicket, MutationType::PromoteEpic] {
            let runner = FakeRunner::new();
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let v = view(
                mt,
                MutationPayload::Promotion(Promotion {
                    title: "T".into(),
                    body: "B".into(),
                    backend_kind: "github".into(),
                }),
                Some("42"),
            );
            match adapter.apply_mutation(&v, NOW).unwrap() {
                ApplyOutcome::Rejected(f) => {
                    assert_eq!(
                        f.detail,
                        "the GitHub Backend Adapter cannot apply a Promotion in this build",
                        "{mt}"
                    );
                }
                ApplyOutcome::Accepted(_) => {
                    panic!("{mt}: GitHub declares no Promotion capability");
                }
            }
        }
    }

    #[test]
    fn github_declares_no_promotion_capabilities() {
        let runner = FakeRunner::new();
        let cwd = cwd();
        let adapter = GithubAdapter::new(&runner, &cwd);
        let caps = adapter.promotion_capabilities();
        assert!(!caps.can_create_item_class(ItemClass::Ticket));
        assert!(!caps.can_create_item_class(ItemClass::Epic));
        assert!(!caps.can_create_ticket_kind(TicketKind::Task));
        assert!(!caps.can_create_ticket_kind(TicketKind::Bug));
        assert!(!caps.can_represent_dependencies());
        assert!(!caps.can_represent_epic_membership());
    }

    #[test]
    fn apply_update_ticket_without_backend_key_is_rejected_not_panicked() {
        // A Pending Promotion Item can carry ordinary Mutations behind its
        // unapplied Promotion (ADR-0036), so `backend_key` can be absent for a
        // Mutation Type other than promote_*. FakeRunner has no expectations,
        // so any `gh` call panics.
        let runner = FakeRunner::new();
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = view(
            MutationType::UpdateTicket,
            MutationPayload::UpdateTitleBody(TitleBody {
                title: "New".into(),
                body: "Body".into(),
            }),
            None,
        );
        match adapter.apply_mutation(&v, NOW).unwrap() {
            ApplyOutcome::Rejected(f) => {
                assert!(f.detail.contains("no backend identity"), "{}", f.detail);
            }
            ApplyOutcome::Accepted(_) => panic!("expected rejection"),
        }
    }

    #[test]
    fn apply_add_dependency_without_backend_key_is_rejected_not_panicked() {
        let runner = FakeRunner::new();
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = MutationView {
            counterpart_backend_key: Some("9".into()),
            ..view(
                MutationType::AddDependency,
                MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "blocking-internal-id".into(),
                }),
                None,
            )
        };
        match adapter.apply_mutation(&v, NOW).unwrap() {
            ApplyOutcome::Rejected(f) => {
                assert!(f.detail.contains("target Item"), "{}", f.detail);
            }
            ApplyOutcome::Accepted(_) => panic!("expected rejection"),
        }
    }

    #[test]
    fn apply_add_dependency_without_counterpart_backend_key_is_rejected_not_panicked() {
        let runner = FakeRunner::new();
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let v = dep_view(MutationType::AddDependency, "5", "9");
        let v = MutationView {
            counterpart_backend_key: None,
            ..v
        };
        match adapter.apply_mutation(&v, NOW).unwrap() {
            ApplyOutcome::Rejected(f) => {
                assert!(f.detail.contains("Blocking Item"), "{}", f.detail);
            }
            ApplyOutcome::Accepted(_) => panic!("expected rejection"),
        }
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
