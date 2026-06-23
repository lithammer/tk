//! GitHub Backend Adapter over the `gh` CLI (ADR-0021 fields, ADR-0034 opt-in).
//!
//! Implements [`Adapter`] by shelling out to `gh issue` through an injected
//! [`ProcRunner`] (ADR-0031: the real subprocess only in production, a
//! `FakeRunner` in tests). The repository is resolved from the command cwd —
//! no `--repo`, no stored repo (ADR-0033): `gh` reads the GitHub repo from the
//! checkout's git remote, stable across Workspaces.
//!
//! Pull is refresh-by-key: the engine hands the Adopted working set's active
//! keys to [`GithubAdapter::fetch_snapshots`], which fetches each with one
//! `gh issue view`. There is no listing and no discovery (ADR-0034).

use std::path::Path;

use serde::Deserialize;

use crate::domain::apply_outcome::{ApplyOutcome, Failure, FailureClass};
use crate::domain::backend_item_snapshot::BackendItemSnapshot;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_payload::MutationPayload;
use crate::domain::mutation_type::MutationType;
use crate::domain::mutation_view::MutationView;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
use crate::proc::{ProcRunner, RunOutput};

use super::adapter::{Adapter, ApplyError, PullError};

/// The `--json` field set tk requests from `gh issue view`. `url` is fetched
/// only for the PR guard (see [`is_pull_request_url`]) and is not stored on the
/// snapshot; `state` arrives UPPERCASE; `issueType` is an object-or-null.
const ISSUE_JSON_FIELDS: &str = "number,title,body,state,issueType,updatedAt,url";

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
    fn fetch_snapshots(&mut self, keys: &[&str]) -> Result<Vec<BackendItemSnapshot>, PullError> {
        let mut snapshots = Vec::with_capacity(keys.len());
        for &key in keys {
            let output = self.runner.run(
                &["gh", "issue", "view", key, "--json", ISSUE_JSON_FIELDS],
                self.cwd,
            )?;
            if !output.succeeded() {
                return Err(PullError::Failed(stderr_string(&output)));
            }
            let issue: GhIssue = serde_json::from_slice(&output.stdout).map_err(|e| {
                PullError::Failed(format!("could not parse gh issue JSON for #{key}: {e}"))
            })?;
            snapshots.push(issue.into_snapshot()?);
        }
        Ok(snapshots)
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
                let number = backend_number(view);
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
                let number = backend_number(view);
                self.run_apply(&["gh", "issue", verb, number])
            }
            // Relationship sync is deferred (ADR-0021): these Mutations still
            // reach the adapter but drive no `gh` call. No-op Accepted keeps the
            // queue draining instead of wedging it on a permanent rejection.
            MutationType::UpdateEpic
            | MutationType::AddTicketToEpic
            | MutationType::RemoveTicketFromEpic
            | MutationType::AddDependency
            | MutationType::RemoveDependency => Ok(ApplyOutcome::accepted()),
            // load_applicable_mutations rejects these payload-less kinds before
            // they reach any adapter.
            MutationType::PromoteTicket
            | MutationType::PromoteEpic
            | MutationType::AddExternalBlocker
            | MutationType::ResolveExternalBlocker => {
                unreachable!("load_applicable_mutations filters payload-less mutation kinds")
            }
        }
    }
}

/// The GitHub issue number for a Ticket Mutation. A Mutation only exists for a
/// Backend item (pre-Promotion local edits are current-state changes, not
/// Mutations), so the backend key is always present here.
fn backend_number(view: &MutationView) -> &str {
    view.backend_key
        .as_deref()
        .expect("a backend Ticket Mutation carries a backend key")
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
    #[serde(rename = "updatedAt")]
    updated_at: String,
    /// Canonical issue/PR url; consumed only by the PR guard, never stored.
    url: String,
}

/// The `issueType` object; `null` for an untyped issue or a repo without issue
/// types. Only `name` is read (ADR-0021 issue-type → Ticket Kind mapping).
#[derive(Debug, Deserialize)]
struct GhIssueType {
    name: String,
}

impl GhIssue {
    fn into_snapshot(self) -> Result<BackendItemSnapshot, PullError> {
        // PR guard (ADR-0034): `gh issue view <n>` resolves a pull request too
        // (issue and PR numbers share one sequence) and returns it as an
        // issue-shaped object, so reject when the canonical url is a /pull/<n>
        // path. tk has no PR concept; the user meant an issue.
        if is_pull_request_url(&self.url) {
            return Err(PullError::Failed(format!(
                "#{} is a pull request, not an issue",
                self.number
            )));
        }
        let status = match self.state.as_str() {
            "OPEN" => ItemStatus::Open,
            "CLOSED" => ItemStatus::Done,
            other => {
                return Err(PullError::Failed(format!(
                    "#{}: unexpected issue state '{other}'",
                    self.number
                )));
            }
        };
        // "Bug" → Bug; every other value ("Task", "Feature", org-custom) and a
        // typeless issue → Task, matching the closed two-variant TicketKind
        // (ADR-0021). Pull-only; `--type` is never written.
        let ticket_kind = match self.issue_type.as_ref().map(|t| t.name.as_str()) {
            Some("Bug") => TicketKind::Bug,
            _ => TicketKind::Task,
        };
        Ok(BackendItemSnapshot {
            backend_kind: "github".into(),
            backend_key: self.number.to_string(),
            display_id: format!("gh-{}", self.number),
            item_class: ItemClass::Ticket,
            ticket_kind: Some(ticket_kind),
            title: self.title,
            body: self.body,
            status,
            backend_updated_at: self.updated_at,
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
/// detail / `PullError::Failed` payload.
fn stderr_string(output: &RunOutput) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::mutation_payload::{DependencyRef, EpicRef, StatusChange, TitleBody};
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
        }
    }

    // ---- fetch_snapshots ------------------------------------------------

    #[test]
    fn fetch_requests_exact_argv_and_parses_open_issue() {
        let runner = FakeRunner::new();
        runner.expect(
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
        let snaps = adapter.fetch_snapshots(&["42"]).unwrap();
        assert_eq!(snaps.len(), 1);
        let s = &snaps[0];
        assert_eq!(s.backend_key, "42");
        assert_eq!(s.display_id, "gh-42");
        assert_eq!(s.item_class, ItemClass::Ticket);
        assert_eq!(s.ticket_kind, Some(TicketKind::Task));
        assert_eq!(s.status, ItemStatus::Open);
        assert_eq!(s.title, "T42");
        assert_eq!(s.backend_updated_at, "2026-06-20T00:00:00Z");
    }

    #[test]
    fn fetch_maps_closed_to_done_and_bug_kind() {
        let runner = FakeRunner::new();
        runner.expect(
            &["gh", "issue", "view", "7"],
            ok(&issue_json(
                7,
                "CLOSED",
                "Bug",
                "https://github.com/o/r/issues/7",
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let s = &adapter.fetch_snapshots(&["7"]).unwrap()[0];
        assert_eq!(s.status, ItemStatus::Done);
        assert_eq!(s.ticket_kind, Some(TicketKind::Bug));
    }

    #[test]
    fn fetch_maps_non_bug_and_null_issue_type_to_task() {
        for it in ["Feature", "null", "CustomOrgType"] {
            let runner = FakeRunner::new();
            runner.expect(
                &["gh", "issue", "view", "1"],
                ok(&issue_json(
                    1,
                    "OPEN",
                    it,
                    "https://github.com/o/r/issues/1",
                )),
            );
            let cwd = cwd();
            let mut adapter = GithubAdapter::new(&runner, &cwd);
            let s = &adapter.fetch_snapshots(&["1"]).unwrap()[0];
            assert_eq!(
                s.ticket_kind,
                Some(TicketKind::Task),
                "issueType {it} → Task"
            );
        }
    }

    #[test]
    fn fetch_rejects_a_pull_request() {
        let runner = FakeRunner::new();
        runner.expect(
            &["gh", "issue", "view", "99"],
            ok(&issue_json(
                99,
                "OPEN",
                "null",
                "https://github.com/o/r/pull/99",
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        match adapter.fetch_snapshots(&["99"]).unwrap_err() {
            PullError::Failed(d) => assert!(d.contains("#99 is a pull request"), "{d}"),
            PullError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
    }

    #[test]
    fn fetch_non_zero_exit_is_pull_failed_with_stderr() {
        // Verbatim not-found stderr observed in the tk-gh-playground spike
        // (docs/spikes/gh-cli-issue-behavior.md).
        let stderr = "GraphQL: Could not resolve to an issue or pull request \
                      with the number of 5. (repository.issue)";
        let runner = FakeRunner::new();
        runner.expect(&["gh", "issue", "view", "5"], fail(1, stderr));
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        match adapter.fetch_snapshots(&["5"]).unwrap_err() {
            PullError::Failed(d) => assert_eq!(d, stderr),
            PullError::Env(e) => panic!("expected Failed, got Env({e:?})"),
        }
    }

    #[test]
    fn fetch_spawn_failure_is_pull_env() {
        let runner = ErrorInjectingRunner {
            err: ProcError::ExecutableNotFound,
        };
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.fetch_snapshots(&["1"]).unwrap_err(),
            PullError::Env(ProcError::ExecutableNotFound)
        ));
    }

    #[test]
    fn fetch_loops_each_key_in_order() {
        let runner = FakeRunner::new();
        runner.expect(
            &["gh", "issue", "view", "1"],
            ok(&issue_json(
                1,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/1",
            )),
        );
        runner.expect(
            &["gh", "issue", "view", "2"],
            ok(&issue_json(
                2,
                "CLOSED",
                "null",
                "https://github.com/o/r/issues/2",
            )),
        );
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        let snaps = adapter.fetch_snapshots(&["1", "2"]).unwrap();
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].display_id, "gh-1");
        assert_eq!(snaps[1].display_id, "gh-2");
    }

    #[test]
    fn fetch_empty_keys_makes_no_call() {
        let runner = FakeRunner::new(); // no expectations: any call panics
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(adapter.fetch_snapshots(&[]).unwrap().is_empty());
    }

    #[test]
    fn fetch_unparseable_json_is_pull_failed() {
        let runner = FakeRunner::new();
        runner.expect(&["gh", "issue", "view", "1"], ok("not json"));
        let cwd = cwd();
        let mut adapter = GithubAdapter::new(&runner, &cwd);
        assert!(matches!(
            adapter.fetch_snapshots(&["1"]).unwrap_err(),
            PullError::Failed(_)
        ));
    }

    // ---- apply_mutation -------------------------------------------------

    #[test]
    fn apply_update_ticket_edits_title_and_body() {
        let runner = FakeRunner::new();
        runner.expect(
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
    }

    #[test]
    fn apply_set_status_done_closes() {
        let runner = FakeRunner::new();
        runner.expect(&["gh", "issue", "close", "42"], ok(""));
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
    }

    #[test]
    fn apply_set_status_open_or_active_reopens() {
        for status in ["open", "active"] {
            let runner = FakeRunner::new();
            runner.expect(&["gh", "issue", "reopen", "42"], ok(""));
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
        }
    }

    #[test]
    fn apply_exit_zero_with_stderr_is_accepted() {
        // gh's idempotent "already closed" no-op exits 0 but prints to stderr.
        // Success is judged by exit code, so this must be Accepted, not rejected.
        let runner = FakeRunner::new();
        runner.expect(
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
    }

    #[test]
    fn apply_non_zero_is_classified_rejection() {
        let runner = FakeRunner::new();
        runner.expect(
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
    fn apply_relationship_mutations_are_noop_accepted_without_a_call() {
        // FakeRunner has no expectations, so any gh call panics. These arms
        // returning Accepted proves they drive no subprocess (ADR-0021).
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
            (
                MutationType::AddDependency,
                MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "b".into(),
                }),
            ),
            (
                MutationType::RemoveDependency,
                MutationPayload::DependencyRef(DependencyRef {
                    blocking_id: "b".into(),
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
