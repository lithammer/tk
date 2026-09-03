//! Shared unit-test scaffolding for the command modules.
//!
//! Every command test runs the same prologue: a [`Deps`] over in-memory
//! writers, a Repository Store seeded inside a temp directory, and a queued
//! `git rev-parse` discovery call for the runner to answer. This module owns
//! that prologue so the command modules only carry what is specific to them.
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

/// Fixed wall clock for command tests. Timestamps and the ULIDs derived from
/// them appear verbatim in assertions, so this value is load-bearing.
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
/// expectations directly (`h.runner.expect(...)`). `rng` and `cwd` are only
/// ever handed to [`Deps`], so they stay private.
pub(crate) struct Harness<'a> {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdin: std::io::Cursor<Vec<u8>>,
    pub runner: FakeRunner,
    pub clock: FakeClock,
    rng: StdRng,
    cwd: &'a Path,
}

impl<'a> Harness<'a> {
    /// Harness seeded at RNG seed 0 and [`CLOCK_MS`].
    pub fn new(cwd: &'a Path) -> Self {
        Self::with_seed(cwd, 0)
    }

    /// Harness at an explicit RNG seed. Commands that mint identifiers assert
    /// the exact ULIDs a seed produces, so each module pins its own.
    pub fn with_seed(cwd: &'a Path, seed: u64) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: std::io::Cursor::new(Vec::new()),
            runner: FakeRunner::new(),
            clock: FakeClock::new(CLOCK_MS),
            rng: StdRng::seed_from_u64(seed),
            cwd,
        }
    }

    /// Override the fake clock for a module whose assertions pin a different
    /// stamp than [`CLOCK_MS`].
    pub fn with_clock_ms(mut self, millis: i64) -> Self {
        self.clock = FakeClock::new(millis);
        self
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

/// Queue the `git rev-parse` discovery call `open_for_command` makes. FIFO, so
/// this must precede any `gh` expectation.
pub(crate) fn expect_git(h: &Harness<'_>, store: &TmpStore) {
    h.runner.expect(
        &["git", "rev-parse"],
        RunOutput {
            exit_code: 0,
            stdout: store.git_rev_parse_stdout(),
            stderr: Vec::new(),
        },
    );
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
                "isInOrganization": true,
                "item": {
                    "__typename": "Issue",
                    "number": number,
                    "title": title,
                    "body": body,
                    "state": state,
                    "url": format!("https://github.com/{owner}/{name}/issues/{number}"),
                    "issueType": null,
                    "labels": { "nodes": [] },
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
