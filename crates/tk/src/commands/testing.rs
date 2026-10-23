//! Shared unit-test scaffolding for the command modules.
//!
//! Command tests share in-memory writers, an isolated Repository Store, and
//! queued Git discovery and Store pointer reads.
//!
//! Available to crate tests only — `mod testing` is gated on `#[cfg(test)]` in
//! `commands/mod.rs`, mirroring `store/mod.rs`.

use std::path::{Path, PathBuf};

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;

pub(crate) use crate::store::testing::{seed_store, seed_store_at_version};

use crate::cli::Deps;
use crate::clock::FakeClock;
use crate::domain::lifecycle::Lifecycle;
use crate::proc::{FakeRunner, RunOutput};
use crate::render::Styler;
use crate::store::testing::TmpStore;

/// Fixed wall clock for timestamps asserted verbatim in command tests.
pub(crate) const CLOCK_MS: i64 = 1_778_284_800_000;

/// The command cwd tests run against. Discovery is answered by [`expect_git`],
/// so the real directory only has to exist.
pub(crate) fn cwd() -> PathBuf {
    std::env::current_dir().unwrap()
}

/// Injected dependencies plus the buffers a test reads back.
///
/// `stdout` / `stderr` / `stdin` / `runner` / `clock` are public because tests
/// move the buffers out (`String::from_utf8(h.stdout)`) and queue subprocess
/// expectations directly (`h.runner.expect(...)`). `rng`, `cwd`, and `data_root`
/// are only handed to [`Deps`], so they stay private.
pub(crate) struct Harness<'a> {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdin: std::io::Cursor<Vec<u8>>,
    pub runner: FakeRunner,
    pub clock: FakeClock,
    rng: StdRng,
    data_root: PathBuf,
    cwd: &'a Path,
}

impl<'a> Harness<'a> {
    /// Use the fixture's isolated data root, RNG seed 0, and [`CLOCK_MS`].
    pub fn new(cwd: &'a Path, store: &TmpStore) -> Self {
        Self::with_seed(cwd, store, 0)
    }

    /// Use the fixture's isolated data root and an explicit RNG seed.
    /// Tests pin the seed when they assert exact generated IDs.
    pub fn with_seed(cwd: &'a Path, store: &TmpStore, seed: u64) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: std::io::Cursor::new(Vec::new()),
            runner: FakeRunner::new(),
            clock: FakeClock::new(CLOCK_MS),
            rng: StdRng::seed_from_u64(seed),
            data_root: store.data_root.clone(),
            cwd,
        }
    }

    /// `Deps` with colour off.
    ///
    /// Scenario-test default per ADR-0014: both streams stay no-colour so
    /// byte-exact output assertions hold without TTY mocking.
    pub fn deps(&mut self) -> Deps<'_> {
        self.deps_with(Styler::plain())
    }

    /// `Deps` with an explicit [`Styler`], for the renderers whose tests
    /// exercise the coloured path.
    pub fn deps_with(&mut self, styler: Styler) -> Deps<'_> {
        Deps {
            data_root: Some(&self.data_root),
            migration_boundary: |_| Ok(()),
            stdout: &mut self.stdout,
            stderr: &mut self.stderr,
            stdin: &mut self.stdin,
            runner: &self.runner,
            clock: &self.clock,
            rng: &mut self.rng,
            cwd: self.cwd,
            styler,
        }
    }

    pub fn out(&self) -> String {
        String::from_utf8(self.stdout.clone()).unwrap()
    }

    pub fn err(&self) -> String {
        String::from_utf8(self.stderr.clone()).unwrap()
    }
}

/// Queue Git discovery and pointer reads for the fixture.
/// These expectations must precede any Backend calls.
pub(crate) fn expect_git(h: &Harness<'_>, store: &TmpStore) {
    h.runner.expect(
        &["git", "rev-parse"],
        RunOutput {
            exit_code: 0,
            stdout: store.git_rev_parse_stdout(),
            stderr: Vec::new(),
        },
    );
    crate::store::testing::expect_pointer(&h.runner);
}

/// Queue one exact GraphQL Pull request and its matching Issue response.
///
/// `lifecycle` selects the GitHub issue `state` the response carries, mirroring
/// the decode direction in [`crate::remote::github`]'s Pull `lifecycle()`:
/// `Lifecycle::Open` -> `"OPEN"`, `Lifecycle::Done` -> `"CLOSED"`.
pub(crate) fn expect_github_pull(
    h: &Harness<'_>,
    owner: &str,
    name: &str,
    number: i64,
    title: &str,
    body: &str,
    lifecycle: Lifecycle,
) {
    let state = match lifecycle {
        Lifecycle::Open => "OPEN",
        Lifecycle::Done => "CLOSED",
    };
    let request = crate::remote::github::single_pull_request_body(owner, name, number);
    let response = serde_json::to_vec(&json!({
        "data": {
            "item_0": {
                "issueFields": priority_fields(),
                "item": {
                    "__typename": "Issue",
                    "number": number,
                    "title": title,
                    "body": body,
                    "state": state,
                    "url": format!("https://github.com/{owner}/{name}/issues/{number}"),
                    "issueType": null,
                },
            },
        },
    }))
    .unwrap();
    h.runner.expect_exact_with_stdin(
        &[
            "gh",
            "api",
            "graphql",
            "--hostname",
            "github.com",
            "-H",
            "Content-Type: application/json",
            "--input",
            "-",
        ],
        &request,
        RunOutput {
            exit_code: 0,
            stdout: response,
            stderr: Vec::new(),
        },
    );
}

/// Queue the `gh repo view` read that names the repository for taxonomy reads.
fn expect_repo_view(h: &Harness<'_>, owner: &str, name: &str) {
    h.runner.expect_exact(
        &["gh", "repo", "view", "--json", "id,nameWithOwner,url"],
        RunOutput {
            exit_code: 0,
            stdout: serde_json::to_vec(&json!({
                "id": "R_1",
                "nameWithOwner": format!("{owner}/{name}"),
                "url": format!("https://github.com/{owner}/{name}"),
            }))
            .unwrap(),
            stderr: Vec::new(),
        },
    );
}

/// The account's Priority field: `F_PRIORITY`, with the options `Urgent`..`Low`
/// as `O_P0`..`O_P3`.
fn priority_fields() -> serde_json::Value {
    json!({
        "nodes": [{"id": "F_PRIORITY", "name": "Priority", "options": [
            {"id": "O_P0", "name": "Urgent"},
            {"id": "O_P1", "name": "High"},
            {"id": "O_P2", "name": "Medium"},
            {"id": "O_P3", "name": "Low"},
        ]}],
        "pageInfo": {"hasNextPage": false},
    })
}

/// Queue the repository read and Issue Type page that resolve `type_name` to
/// `type_id` for an Epic or Bug requirement.
pub(crate) fn expect_github_issue_type(
    h: &Harness<'_>,
    owner: &str,
    name: &str,
    type_name: &str,
    type_id: &str,
) {
    expect_repo_view(h, owner, name);
    h.runner.expect_exact_with_stdin(
        &GRAPHQL_ARGV,
        &crate::remote::github::issue_types_request_body(owner, name),
        RunOutput {
            exit_code: 0,
            stdout: serde_json::to_vec(&json!({
                "data": {"repository": {"issueTypes": {
                    "nodes": [{"id": type_id, "name": type_name, "isEnabled": true}],
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                }}},
            }))
            .unwrap(),
            stderr: Vec::new(),
        },
    );
}

/// Queue the repository read and issue field read that resolve the Priority
/// field for a Ticket requirement (see `priority_fields`).
pub(crate) fn expect_github_priority_field(h: &Harness<'_>, owner: &str, name: &str) {
    expect_repo_view(h, owner, name);
    h.runner.expect_exact_with_stdin(
        &GRAPHQL_ARGV,
        &crate::remote::github::issue_fields_request_body(owner, name),
        RunOutput {
            exit_code: 0,
            stdout: serde_json::to_vec(&json!({
                "data": {"repository": {"issueFields": priority_fields()}},
            }))
            .unwrap(),
            stderr: Vec::new(),
        },
    );
}

/// Queue one `createIssue` mutation that returns issue `number`. `priority`
/// is the Priority option ID set in the same call, as `expect_github_priority_field`
/// names them.
pub(crate) fn expect_github_create(
    h: &Harness<'_>,
    owner: &str,
    name: &str,
    issue: (&str, &str),
    type_id: Option<&str>,
    priority: Option<&str>,
    number: i64,
) {
    let (title, body) = issue;
    h.runner.expect_exact_with_stdin(
        &GRAPHQL_ARGV,
        &crate::remote::github::create_issue_request_body(
            "R_1",
            title,
            body,
            type_id,
            priority.map(|option_id| ("F_PRIORITY", option_id)),
        ),
        RunOutput {
            exit_code: 0,
            stdout: serde_json::to_vec(&json!({
                "data": {"createIssue": {"issue": {
                    "url": format!("https://github.com/{owner}/{name}/issues/{number}"),
                    "number": number,
                }}},
            }))
            .unwrap(),
            stderr: Vec::new(),
        },
    );
}

const GRAPHQL_ARGV: [&str; 9] = [
    "gh",
    "api",
    "graphql",
    "--hostname",
    "github.com",
    "-H",
    "Content-Type: application/json",
    "--input",
    "-",
];
