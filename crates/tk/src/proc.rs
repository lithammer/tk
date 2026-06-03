//! Subprocess runner abstraction shared by every command that shells out.
//!
//! ADR-0018 models the subprocess seam as a Rust trait. Every
//! subprocess in tk (git / gh / acli / curl) flows through this seam so tests
//! can substitute a `FakeRunner` without per-call-site changes.
//!
//! `tk init` only spawns `git rev-parse`, but the trait must already be shaped
//! correctly for downstream callers (see [`crate::git::discovery`]).

use std::path::{Path, PathBuf};
use std::process::Command;

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
        let output = Command::new(program)
            .args(rest)
            .current_dir(cwd)
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => ProcError::ExecutableNotFound,
                _ => ProcError::SpawnFailed,
            })?;
        Ok(RunOutput {
            exit_code: output.status.code().unwrap_or(255),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

// ---- Fakes ---------------------------------------------------------------

/// One scripted invocation expected by [`FakeRunner`].
#[derive(Debug, Clone)]
pub struct FakeCall {
    /// Argv prefix that must match (e.g. `["git", "rev-parse"]`). Extra args
    /// beyond the prefix are allowed.
    pub argv_prefix: Vec<String>,
    pub output: RunOutput,
    /// Optional file write performed before the call returns. Models
    /// commands that drop bytes to disk as a side effect — currently
    /// `curl -o <stage_path>` in [`crate::commands::self_update`] — so the
    /// in-process FakeRunner stays a sufficient seam without mutating PATH
    /// or shelling out to a real shim.
    pub side_effect_write: Option<(PathBuf, Vec<u8>)>,
}

/// Strict subprocess fake: an unmatched call panics so a regression that
/// changes argv shape fails loudly during tests.
pub struct FakeRunner {
    calls: std::cell::RefCell<Vec<FakeCall>>,
}

impl FakeRunner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Queue a scripted response. Calls are consumed in FIFO order.
    pub fn expect(&self, argv_prefix: &[&str], output: RunOutput) {
        self.calls.borrow_mut().push(FakeCall {
            argv_prefix: argv_prefix.iter().map(|s| (*s).to_string()).collect(),
            output,
            side_effect_write: None,
        });
    }

    /// Queue a scripted response that also writes `body` to `path` before
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
        self.calls.borrow_mut().push(FakeCall {
            argv_prefix: argv_prefix.iter().map(|s| (*s).to_string()).collect(),
            output,
            side_effect_write: Some((path, body)),
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
        let expected = calls.remove(0);
        let matches = expected
            .argv_prefix
            .iter()
            .zip(argv.iter())
            .all(|(a, b)| a == b);
        assert!(
            matches && argv.len() >= expected.argv_prefix.len(),
            "FakeRunner: argv mismatch.\n  expected prefix: {:?}\n  actual: {:?}",
            expected.argv_prefix,
            argv
        );
        if let Some((ref path, ref body)) = expected.side_effect_write {
            std::fs::write(path, body).unwrap_or_else(|err| {
                panic!("FakeRunner side-effect write to {}: {err}", path.display())
            });
        }
        Ok(expected.output)
    }
}

/// Test runner that returns a single pre-configured error on every call.
/// Used to cover the `ExecutableNotFound` / `SpawnFailed` discovery arms.
pub struct ErrorInjectingRunner {
    pub err: ProcError,
}

impl ProcRunner for ErrorInjectingRunner {
    fn run(&self, _argv: &[&str], _cwd: &Path) -> Result<RunOutput, ProcError> {
        Err(self.err)
    }
}
