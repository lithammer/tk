//! Subprocess runner abstraction shared by every command that shells out.
//!
//! ADR-0018 models the subprocess seam as a Rust trait. Every
//! subprocess in tk (git / gh / acli / curl) flows through this seam so tests
//! can substitute a `FakeRunner` without per-call-site changes.

use std::collections::VecDeque;
use std::io::Write;
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

    /// Run `argv` with the supplied bytes as stdin.
    ///
    /// A write failure happens after child creation, so the external outcome
    /// may be unobserved even when tk later terminates the child.
    fn run_with_stdin(
        &self,
        argv: &[&str],
        cwd: &Path,
        stdin: &[u8],
    ) -> Result<RunOutput, ProcError>;
}

/// Production runner backed by `std::process::Command`.
pub struct RealRunner;

impl RealRunner {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn run_inner(argv: &[&str], cwd: &Path, stdin: Option<&[u8]>) -> Result<RunOutput, ProcError> {
        let (program, rest) = argv
            .split_first()
            .expect("ProcRunner contract: argv must contain at least the program");
        let mut child = Command::new(program)
            .args(rest)
            .current_dir(cwd)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => ProcError::ExecutableNotFound,
                _ => ProcError::SpawnFailed,
            })?;
        let output = if let Some(bytes) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .expect("piped stdin must exist after child creation");
            let (write_result, output_result) = std::thread::scope(|scope| {
                let writer = scope.spawn(move || pipe.write_all(bytes));
                let output_result = child.wait_with_output();
                let write_result = writer.join().expect("stdin writer must not panic");
                (write_result, output_result)
            });
            if write_result.is_err() {
                return Err(ProcError::OutcomeUnobserved);
            }
            output_result.map_err(|_| ProcError::OutcomeUnobserved)?
        } else {
            child
                .wait_with_output()
                .map_err(|_| ProcError::OutcomeUnobserved)?
        };
        Ok(RunOutput {
            exit_code: output.status.code().unwrap_or(255),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

impl Default for RealRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcRunner for RealRunner {
    fn run(&self, argv: &[&str], cwd: &Path) -> Result<RunOutput, ProcError> {
        Self::run_inner(argv, cwd, None)
    }

    fn run_with_stdin(
        &self,
        argv: &[&str],
        cwd: &Path,
        stdin: &[u8],
    ) -> Result<RunOutput, ProcError> {
        Self::run_inner(argv, cwd, Some(stdin))
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
    stdin: Option<Vec<u8>>,
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
        self.queue(
            ArgvExpectation::Prefix(strings(argv_prefix)),
            None,
            Ok(output),
            None,
        );
    }

    /// Queue a scripted response that requires an exact argv match.
    ///
    /// Calls are consumed in FIFO order. Differing and trailing arguments
    /// reject the expectation.
    pub fn expect_exact(&self, argv: &[&str], output: RunOutput) {
        self.queue(
            ArgvExpectation::Exact(strings(argv)),
            None,
            Ok(output),
            None,
        );
    }

    /// Queue a scripted response that requires exact argv and stdin bytes.
    pub fn expect_exact_with_stdin(&self, argv: &[&str], stdin: &[u8], output: RunOutput) {
        self.queue(
            ArgvExpectation::Exact(strings(argv)),
            Some(stdin.to_vec()),
            Ok(output),
            None,
        );
    }

    /// Queue a process error that requires exact argv and stdin bytes.
    pub fn expect_exact_with_stdin_error(&self, argv: &[&str], stdin: &[u8], error: ProcError) {
        self.queue(
            ArgvExpectation::Exact(strings(argv)),
            Some(stdin.to_vec()),
            Err(error),
            None,
        );
    }

    /// Queue a scripted process error matched against an argv prefix.
    pub fn expect_error(&self, argv_prefix: &[&str], error: ProcError) {
        self.queue(
            ArgvExpectation::Prefix(strings(argv_prefix)),
            None,
            Err(error),
            None,
        );
    }

    /// Queue a scripted process error that requires an exact argv match.
    pub fn expect_exact_error(&self, argv: &[&str], error: ProcError) {
        self.queue(
            ArgvExpectation::Exact(strings(argv)),
            None,
            Err(error),
            None,
        );
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
            None,
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
        stdin: Option<Vec<u8>>,
        output: Result<RunOutput, ProcError>,
        side_effect_write: Option<(PathBuf, Vec<u8>)>,
    ) {
        self.calls.borrow_mut().push_back(ExpectedCall {
            argv,
            stdin,
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
        self.run_inner(argv, None)
    }

    fn run_with_stdin(
        &self,
        argv: &[&str],
        _cwd: &Path,
        stdin: &[u8],
    ) -> Result<RunOutput, ProcError> {
        self.run_inner(argv, Some(stdin))
    }
}

impl FakeRunner {
    fn run_inner(&self, argv: &[&str], stdin: Option<&[u8]>) -> Result<RunOutput, ProcError> {
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
        assert_eq!(
            expected.stdin.as_deref(),
            stdin,
            "FakeRunner: stdin mismatch for {argv:?}"
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
    fn stdin_expectation_accepts_matching_bytes() {
        let runner = FakeRunner::new();
        runner.expect_exact_with_stdin(
            &["gh", "api", "graphql", "--input", "-"],
            br#"{"query":"query Viewer { viewer { login } }"}"#,
            output("ok"),
        );

        let result = runner
            .run_with_stdin(
                &["gh", "api", "graphql", "--input", "-"],
                Path::new("."),
                br#"{"query":"query Viewer { viewer { login } }"}"#,
            )
            .unwrap();

        assert_eq!(result.stdout, b"ok");
        runner.assert_all_consumed();
    }

    #[cfg(unix)]
    #[test]
    fn real_runner_writes_stdin_while_collecting_child_output() {
        let payload = vec![b'x'; 1024 * 1024];

        let result = RealRunner::new()
            .run_with_stdin(&["/bin/cat"], Path::new("."), &payload)
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, payload);
        assert!(result.stderr.is_empty());
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
        runner.expect(&["git", "status"], output("ok"));

        let result = runner
            .run(&["git", "status", "--short"], Path::new("."))
            .unwrap();

        assert_eq!(result.stdout, b"ok");
        runner.assert_all_consumed();
    }

    #[test]
    fn expectations_are_consumed_in_fifo_order() {
        let runner = FakeRunner::new();
        runner.expect(&["git"], output("first"));
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
