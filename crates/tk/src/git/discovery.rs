//! Git `rev-parse` path discovery used to locate the Repository Store.
//!
//! `tk init` (and future commands that open the store) need Git's common
//! directory and toplevel to place or find `<git-common-dir>/tk/tk.db`.
//! This module owns the `git rev-parse` invocation and returns the discovered
//! paths as a typed result so callers can render diagnostics without this
//! module reaching for stderr itself.
//!
//! Discovery failures are a typed [`DiscoveryError`]: each variant carries its
//! own user-visible phrasing via `#[error]` (ADR-0017 stable strings), so the
//! caller renders one line without a central message catalogue.

use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::platform;
use crate::proc::{ProcError, ProcRunner};

/// Paths reported by Git for the current repository.
#[derive(Debug, Clone)]
pub struct DiscoveredPaths {
    /// Shared Git common directory; parent of the Repository Store.
    pub git_common_dir: PathBuf,
    /// Repository working-tree root; used only for display_prefix derivation.
    pub toplevel: PathBuf,
}

/// Why [`discover_paths`] could not locate the Repository Store.
///
/// Each variant's `#[error]` string is the stable user-visible phrasing
/// (ADR-0017); [`render_failure`] prints it verbatim behind the `tk <command>:`
/// prefix.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// `git` is not on PATH (runner returned `ExecutableNotFound`).
    #[error("git not found on PATH")]
    GitMissing,
    /// Spawning `git` failed for a reason other than a missing binary.
    #[error("failed to invoke git")]
    SpawnFailed,
    /// `git rev-parse` exited non-zero. `Some` carries git's trimmed stderr,
    /// rendered verbatim; `None` (git failed silently) renders the default
    /// not-in-repo line.
    #[error("{}", .0.as_deref().unwrap_or("not in a git repository"))]
    GitRejected(Option<String>),
    /// `git rev-parse` exited zero but stdout did not contain both expected
    /// lines.
    #[error("git produced unexpected rev-parse output")]
    GitOutputUnparseable,
}

/// Run `git rev-parse --path-format=absolute --git-common-dir --show-toplevel`
/// through `runner` and classify the result.
pub fn discover_paths<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
) -> Result<DiscoveredPaths, DiscoveryError> {
    let argv = [
        "git",
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
        "--show-toplevel",
    ];
    let out = match runner.run(&argv, cwd) {
        Ok(out) => out,
        Err(ProcError::ExecutableNotFound) => return Err(DiscoveryError::GitMissing),
        Err(ProcError::SpawnFailed) => return Err(DiscoveryError::SpawnFailed),
    };

    if !out.succeeded() {
        // Reuse git's stderr buffer in place — `String::from_utf8` keeps the
        // allocation when the bytes are valid UTF-8, and a non-UTF-8 stderr
        // is still useful to surface; we fall back to lossy only there.
        let stderr = String::from_utf8(out.stderr)
            .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
        let trimmed = stderr.trim();
        if trimmed.is_empty() {
            return Err(DiscoveryError::GitRejected(None));
        }
        return Err(DiscoveryError::GitRejected(Some(trimmed.to_string())));
    }

    // Path bytes must round-trip to a real on-disk location, so refuse rather
    // than silently lossy-decode a non-UTF-8 path. The `--path-format=absolute`
    // flag asks git to emit decoded paths; any non-UTF-8 here means the repo's
    // path uses an encoding we cannot represent as `Path` without OS-specific
    // handling (deferred to a follow-up slice).
    let Ok(stdout) = String::from_utf8(out.stdout) else {
        return Err(DiscoveryError::GitOutputUnparseable);
    };
    let mut lines = stdout.split('\n').filter(|line| !line.trim().is_empty());
    let (Some(common_raw), Some(toplevel_raw)) = (lines.next(), lines.next()) else {
        return Err(DiscoveryError::GitOutputUnparseable);
    };

    let common = normalize_native_sep(common_raw.trim().to_string());
    let toplevel = normalize_native_sep(toplevel_raw.trim().to_string());

    Ok(DiscoveredPaths {
        git_common_dir: PathBuf::from(common),
        toplevel: PathBuf::from(toplevel),
    })
}

/// Git emits forward slashes on every OS. Downstream `std::path` joins use the
/// native separator; normalise here at the discovery boundary so printed paths
/// don't come out mixed (e.g. `D:/repo/.git\tk\tk.db`).
fn normalize_native_sep(s: String) -> String {
    if platform::IS_WINDOWS {
        s.replace('/', "\\")
    } else {
        s
    }
}

/// Render a [`DiscoveryError`] to `stderr` behind a `tk <command>: ` prefix.
///
/// The error's `Display` is the stable message (ADR-0017), so this is a single
/// formatted line shared by every command that opens a Repository Store.
///
/// `command` is the bare subcommand name (`"init"`, `"add"`), without the
/// `tk ` or the trailing colon — those are formatted in.
pub fn render_failure<W: Write + ?Sized>(stderr: &mut W, command: &str, err: &DiscoveryError) {
    let _ = writeln!(stderr, "tk {command}: {err}");
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::{ErrorInjectingRunner, FakeRunner, ProcError, RunOutput};
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    #[test]
    fn returns_ok_with_both_paths_on_success() {
        let fake = FakeRunner::new();
        fake.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: b"/repo/.git\n/repo\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let outcome = discover_paths(&fake, &cwd());
        match outcome {
            Ok(paths) => {
                if platform::IS_WINDOWS {
                    assert_eq!(paths.git_common_dir, PathBuf::from("\\repo\\.git"));
                    assert_eq!(paths.toplevel, PathBuf::from("\\repo"));
                } else {
                    assert_eq!(paths.git_common_dir, PathBuf::from("/repo/.git"));
                    assert_eq!(paths.toplevel, PathBuf::from("/repo"));
                }
            }
            Err(other) => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn returns_git_rejected_with_trimmed_stderr_on_exit_128() {
        let fake = FakeRunner::new();
        fake.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: b"fatal: not a git repository (or any of the parent directories): .git\n"
                    .to_vec(),
            },
        );
        match discover_paths(&fake, &cwd()) {
            Err(DiscoveryError::GitRejected(Some(msg))) => assert_eq!(
                msg,
                "fatal: not a git repository (or any of the parent directories): .git"
            ),
            other => panic!("expected GitRejected(Some), got {other:?}"),
        }
    }

    #[test]
    fn returns_git_rejected_none_when_stderr_is_whitespace() {
        let fake = FakeRunner::new();
        fake.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 128,
                stdout: Vec::new(),
                stderr: b"  \r\n  ".to_vec(),
            },
        );
        assert!(matches!(
            discover_paths(&fake, &cwd()),
            Err(DiscoveryError::GitRejected(None))
        ));
    }

    #[test]
    fn returns_unparseable_when_stdout_has_no_lines() {
        let fake = FakeRunner::new();
        fake.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        assert!(matches!(
            discover_paths(&fake, &cwd()),
            Err(DiscoveryError::GitOutputUnparseable)
        ));
    }

    #[test]
    fn returns_unparseable_when_stdout_has_only_one_line() {
        let fake = FakeRunner::new();
        fake.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: b"/repo/.git\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        assert!(matches!(
            discover_paths(&fake, &cwd()),
            Err(DiscoveryError::GitOutputUnparseable)
        ));
    }

    #[test]
    fn maps_executable_not_found_to_git_missing() {
        let runner = ErrorInjectingRunner {
            err: ProcError::ExecutableNotFound,
        };
        assert!(matches!(
            discover_paths(&runner, &cwd()),
            Err(DiscoveryError::GitMissing)
        ));
    }

    #[test]
    fn maps_spawn_failed_to_spawn_failed() {
        let runner = ErrorInjectingRunner {
            err: ProcError::SpawnFailed,
        };
        assert!(matches!(
            discover_paths(&runner, &cwd()),
            Err(DiscoveryError::SpawnFailed)
        ));
    }

    #[test]
    fn render_failure_formats_each_arm() {
        let mut buf = Vec::new();
        render_failure(&mut buf, "init", &DiscoveryError::GitMissing);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "tk init: git not found on PATH\n"
        );

        let mut buf = Vec::new();
        render_failure(&mut buf, "add", &DiscoveryError::SpawnFailed);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "tk add: failed to invoke git\n"
        );

        let mut buf = Vec::new();
        render_failure(&mut buf, "init", &DiscoveryError::GitOutputUnparseable);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "tk init: git produced unexpected rev-parse output\n"
        );

        let mut buf = Vec::new();
        render_failure(&mut buf, "init", &DiscoveryError::GitRejected(None));
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "tk init: not in a git repository\n"
        );

        let mut buf = Vec::new();
        render_failure(
            &mut buf,
            "add",
            &DiscoveryError::GitRejected(Some("fatal: not a git repository".to_string())),
        );
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "tk add: fatal: not a git repository\n"
        );
    }
}
