//! Subprocess runner abstraction shared by every command that shells out.
//!
//! ADR-0018 models the subprocess seam as a Rust trait. Every
//! subprocess in tk (git / gh / acli / curl) flows through this seam so tests
//! can substitute a `FakeRunner` without per-call-site changes.
//!
//! `tk init` only spawns `git rev-parse`, but the trait must already be shaped
//! correctly for downstream callers (see [`crate::git::discovery`]).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;

/// Captured outcome of a subprocess invocation.
#[derive(Debug, Clone)]
pub struct RunOutput {
    /// `0` on clean exit; non-zero on a non-zero exit status.
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    /// True when the child exited with status 0.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

/// Failure modes returned by [`ProcRunner`].
///
/// Bare distinguishing-only tags: callers map them to their own typed errors
/// (e.g. [`crate::git::discovery::DiscoveryError`]). The variants are
/// payload-free because consumers render a fixed stderr line from the tag
/// alone; their own `#[error]` strings carry the user-visible phrasing.
#[derive(Debug, Clone, Copy, Error)]
pub enum ProcError {
    /// The binary was not found on PATH (POSIX `ENOENT`).
    #[error("executable not found on PATH")]
    ExecutableNotFound,
    /// Spawning the child failed for a reason other than missing binary
    /// (permissions, fork failure, …).
    #[error("failed to spawn child process")]
    SpawnFailed,
    /// The child started, but tk could not wait for it or capture its output.
    /// The external effect is therefore unknown.
    #[error("child process started but its outcome could not be observed")]
    OutcomeUnobserved,
}

/// Common subprocess seam. Implementations decide whether to spawn a real
/// child, replay scripted expectations, or inject an error.
pub trait ProcRunner {
    /// Run `argv` with working directory `cwd`, capturing stdout and stderr.
    ///
    /// `argv[0]` is the program; remaining slots are arguments. The runner
    /// never inherits stdin from the calling process — `tk init` and most
    /// downstream commands do not pipe input into subprocesses.
    fn run(&self, argv: &[&str], cwd: &Path) -> Result<RunOutput, ProcError>;
}

/// Production runner backed by `std::process::Command`.
pub struct RealRunner;

impl RealRunner {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcRunner for RealRunner {
    fn run(&self, argv: &[&str], cwd: &Path) -> Result<RunOutput, ProcError> {
        let (program, rest) = argv
            .split_first()
            .expect("ProcRunner contract: argv must contain at least the program");
        let child = Command::new(program)
            .args(rest)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => ProcError::ExecutableNotFound,
                _ => ProcError::SpawnFailed,
            })?;
        let output = child
            .wait_with_output()
            .map_err(|_| ProcError::OutcomeUnobserved)?;
        Ok(RunOutput {
            exit_code: output.status.code().unwrap_or(255),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

// ---- Fakes ---------------------------------------------------------------

#[derive(Debug, Clone)]
enum ArgvExpectation {
    Prefix(Vec<String>),
    Exact(Vec<String>),
}

impl ArgvExpectation {
    fn matches(&self, actual: &[&str]) -> bool {
        match self {
            Self::Prefix(expected) => {
                expected.iter().zip(actual).all(|(a, b)| a == b) && actual.len() >= expected.len()
            }
            Self::Exact(expected) => expected
                .iter()
                .map(String::as_str)
                .eq(actual.iter().copied()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Prefix(_) => "prefix",
            Self::Exact(_) => "exact argv",
        }
    }

    fn argv(&self) -> &[String] {
        match self {
            Self::Prefix(argv) | Self::Exact(argv) => argv,
        }
    }
}

/// One scripted invocation expected by [`FakeRunner`].
#[derive(Debug, Clone)]
struct ExpectedCall {
    argv: ArgvExpectation,
    output: Result<RunOutput, ProcError>,
    /// Optional file write performed before the call returns. This models a
    /// subprocess that writes a file without mutating PATH or using a shell
    /// shim in tests.
    side_effect_write: Option<(PathBuf, Vec<u8>)>,
}

/// Strict subprocess fake: an unmatched call panics so a regression that
/// changes argv shape fails loudly during tests.
pub struct FakeRunner {
    calls: std::cell::RefCell<VecDeque<ExpectedCall>>,
}

impl FakeRunner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: std::cell::RefCell::new(VecDeque::new()),
        }
    }

    /// Queue a scripted response matched against an argv prefix.
    ///
    /// Calls are consumed in FIFO order; arguments after `argv_prefix` are
    /// accepted. Use [`Self::expect_exact`] when the command shape is part of
    /// the behavior under test.
    pub fn expect(&self, argv_prefix: &[&str], output: RunOutput) {
        self.expect_prefix(argv_prefix, output);
    }

    /// Queue a scripted response matched against an argv prefix.
    pub fn expect_prefix(&self, argv_prefix: &[&str], output: RunOutput) {
        self.queue(
            ArgvExpectation::Prefix(strings(argv_prefix)),
            Ok(output),
            None,
        );
    }

    /// Queue a scripted response that requires an exact argv match.
    ///
    /// Calls are consumed in FIFO order. Differing and trailing arguments
    /// reject the expectation.
    pub fn expect_exact(&self, argv: &[&str], output: RunOutput) {
        self.queue(ArgvExpectation::Exact(strings(argv)), Ok(output), None);
    }

    /// Queue a scripted process error matched against an argv prefix.
    pub fn expect_error(&self, argv_prefix: &[&str], error: ProcError) {
        self.queue(
            ArgvExpectation::Prefix(strings(argv_prefix)),
            Err(error),
            None,
        );
    }

    /// Queue a scripted process error that requires an exact argv match.
    pub fn expect_exact_error(&self, argv: &[&str], error: ProcError) {
        self.queue(ArgvExpectation::Exact(strings(argv)), Err(error), None);
    }

    /// Queue a prefix-matched response that writes `body` to `path` before
    /// returning. Models `curl -o <stage_path>` so [`crate::commands::self_update`]
    /// tests can exercise stage → smoke → rename end-to-end without a real
    /// curl binary on PATH.
    pub fn expect_writing(
        &self,
        argv_prefix: &[&str],
        output: RunOutput,
        path: PathBuf,
        body: Vec<u8>,
    ) {
        self.queue(
            ArgvExpectation::Prefix(strings(argv_prefix)),
            Ok(output),
            Some((path, body)),
        );
    }

    /// Panic unless every scripted subprocess call has been consumed.
    ///
    /// Tests can use this to make omitted calls visible without relying on a
    /// `Drop` assertion that would change existing prefix-based fixtures.
    pub fn assert_all_consumed(&self) {
        let calls = self.calls.borrow();
        assert!(
            calls.is_empty(),
            "FakeRunner: unconsumed subprocess expectations: {calls:?}"
        );
    }

    fn queue(
        &self,
        argv: ArgvExpectation,
        output: Result<RunOutput, ProcError>,
        side_effect_write: Option<(PathBuf, Vec<u8>)>,
    ) {
        self.calls.borrow_mut().push_back(ExpectedCall {
            argv,
            output,
            side_effect_write,
        });
    }
}

impl Default for FakeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcRunner for FakeRunner {
    fn run(&self, argv: &[&str], _cwd: &Path) -> Result<RunOutput, ProcError> {
        let mut calls = self.calls.borrow_mut();
        assert!(
            !calls.is_empty(),
            "FakeRunner: unexpected subprocess call: {argv:?}"
        );
        let expected = calls
            .pop_front()
            .expect("FakeRunner checked that the queue is non-empty");
        let matches = expected.argv.matches(argv);
        assert!(
            matches,
            "FakeRunner: argv mismatch.\n  expected {:?}: {:?}\n  actual: {:?}",
            expected.argv.kind(),
            expected.argv.argv(),
            argv
        );
        if let Some((ref path, ref body)) = expected.side_effect_write {
            std::fs::write(path, body).unwrap_or_else(|err| {
                panic!("FakeRunner side-effect write to {}: {err}", path.display())
            });
        }
        expected.output
    }
}

fn strings(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str) -> RunOutput {
        RunOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn exact_expectation_accepts_matching_argv() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["git", "status", "--short"], output("ok"));

        let result = runner
            .run(&["git", "status", "--short"], Path::new("."))
            .unwrap();

        assert_eq!(result.stdout, b"ok");
        runner.assert_all_consumed();
    }

    #[test]
    #[should_panic(expected = "expected \"exact argv\"")]
    fn exact_expectation_rejects_trailing_arguments() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["git", "status"], output("ok"));

        let _ = runner.run(&["git", "status", "--short"], Path::new("."));
    }

    #[test]
    #[should_panic(expected = "expected \"exact argv\"")]
    fn exact_expectation_rejects_differing_arguments() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["git", "status"], output("ok"));

        let _ = runner.run(&["git", "log"], Path::new("."));
    }

    #[test]
    #[should_panic(expected = "expected \"exact argv\"")]
    fn exact_expectation_rejects_missing_arguments() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["git", "status", "--short"], output("ok"));

        let _ = runner.run(&["git", "status"], Path::new("."));
    }

    #[test]
    fn prefix_expectation_accepts_trailing_arguments() {
        let runner = FakeRunner::new();
        runner.expect_prefix(&["git", "status"], output("ok"));

        let result = runner
            .run(&["git", "status", "--short"], Path::new("."))
            .unwrap();

        assert_eq!(result.stdout, b"ok");
        runner.assert_all_consumed();
    }

    #[test]
    fn expectations_are_consumed_in_fifo_order() {
        let runner = FakeRunner::new();
        runner.expect_prefix(&["git"], output("first"));
        runner.expect_exact(&["git", "rev-parse"], output("second"));

        let first = runner.run(&["git", "status"], Path::new(".")).unwrap();
        let second = runner.run(&["git", "rev-parse"], Path::new(".")).unwrap();

        assert_eq!(first.stdout, b"first");
        assert_eq!(second.stdout, b"second");
        runner.assert_all_consumed();
    }

    #[test]
    #[should_panic(expected = "unconsumed subprocess expectations")]
    fn assert_all_consumed_rejects_unconsumed_exact_expectations() {
        let runner = FakeRunner::new();
        runner.expect_exact(&["git", "status"], output("ok"));

        runner.assert_all_consumed();
    }
}
