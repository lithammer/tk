//! `tk self-update` — replace the running tk binary with the latest release.
//!
//! Forward-only in v1: downgrades and Linux ABI variant switching go through
//! the install script (`TK_VERSION=…` / `TK_LINUX_ABI=…`). Refuses to run on
//! development builds (embedded triple == [`DEV_TRIPLE`]). ADR-0013 records
//! the staged-binary install contract; tk-32 records the design notes
//! (smoke-check shape, asset-naming rules, two-step API+download flow).
//!
//! Networking shells out to `curl` through [`Deps::runner`] (ADR-0018: every
//! subprocess flows through the same seam). No embedded HTTP client, no
//! async; `tk` keeps zero in-tree networking. Curl writes the asset body to
//! the stage file directly via `-o`, so the stage-and-rename pipeline never
//! buffers a multi-MB payload in memory.

use std::fs;
use std::io::Write;
use std::path::Path;

use clap::Args as ClapArgs;
use rand::Rng;
use thiserror::Error;

use crate::cli::Deps;
use crate::platform;

/// Sentinel for non-release builds. The dev refusal in [`run_with`]
/// compares against this constant; `build.rs` injects the real release
/// triple via `TK_EMBEDDED_TRIPLE` when the release pipeline sets
/// `TK_TRIPLE` at build time.
pub const DEV_TRIPLE: &str = "dev";

/// GitHub Releases API endpoint for the latest published release of `tk`.
/// Returns the `releases/latest` JSON; only `tag_name` is parsed.
const API_URL: &str = "https://api.github.com/repos/lithammer/tk/releases/latest";

/// Browser-facing base URL for a release tag. Both the asset download URL
/// (`<base>/latest/download/<asset>`) and the manual-install diagnostic for
/// a 404 (`<base>/tag/<tag>`) are built from this constant.
const RELEASES_BASE_URL: &str = "https://github.com/lithammer/tk/releases";

/// Stage-file prefix inside the exe directory. Hidden-dot keeps the staged
/// binary out of `ls` output; the hex suffix is 64 random bits via
/// [`Deps::rng`] so concurrent self-updates against the same exe directory
/// cannot collide.
const STAGE_NAME_PREFIX: &str = ".tk.tmp.";

/// Curl's `User-Agent` header is keyed to the running tk version so log
/// readers can correlate self-update fetches with a specific build.
const USER_AGENT_PREFIX: &str = "tk/";

/// Embedded version string consumed by the comparison branch in [`run_with`].
/// Resolved from `TK_VERSION_STRING` injected by `build.rs`, stripped down to
/// just the `vX.Y.Z` head — the trailing ` (<triple>)` lives in the clap
/// `--version` output (where the smoke check expects it) but is not part of
/// the tag the API returns.
fn embedded_version() -> &'static str {
    extract_version_head(env!("TK_VERSION_STRING"))
}

/// `vX.Y.Z (triple)` → `vX.Y.Z`. Anchored on the first space so a missing
/// trailing triple (shouldn't happen, but defensive) returns the whole
/// string unchanged.
fn extract_version_head(s: &'static str) -> &'static str {
    match s.find(' ') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// Embedded release triple. The `DEV_TRIPLE` sentinel routes [`run_with`]
/// through the development-build refusal arm.
fn embedded_triple() -> &'static str {
    env!("TK_EMBEDDED_TRIPLE")
}

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Check whether an update is available; do not download.
    #[arg(long)]
    pub check: bool,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    run_with(deps, args, embedded_version(), embedded_triple())
}

/// Workhorse for `tk self-update`. `embedded_version` and `embedded_triple`
/// are threaded explicitly so tests can exercise the dev refusal, the
/// comparison branches, and the smoke check independently.
///
/// Refusal on dev builds is symmetric across `tk self-update` and
/// `tk self-update --check`: both exit 1 with the same diagnostic before
/// any networking happens. A dev build has no canonical upstream tag to
/// compare against, so even the read-only `--check` query has nothing
/// meaningful to report.
fn run_with(
    mut deps: Deps<'_>,
    args: Args,
    embedded_version: &str,
    embedded_triple: &str,
) -> u8 {
    if embedded_triple == DEV_TRIPLE {
        let _ = writeln!(
            deps.stderr,
            "tk self-update: development builds cannot self-update; install a release via the curl|sh script in README.md"
        );
        return 1;
    }

    let tag = match fetch_latest_tag(deps.runner, deps.cwd, embedded_version) {
        Ok(tag) => tag,
        Err(err) => {
            render_query_error(&mut deps.stderr, &err);
            return 1;
        }
    };

    let cmp = match compare_versions(embedded_version, &tag) {
        Ok(cmp) => cmp,
        Err(err) => {
            render_version_parse_failure(&mut deps.stderr, &err, embedded_version, &tag);
            return 1;
        }
    };

    if args.check {
        return render_check_result(&mut deps.stdout, cmp, embedded_version, &tag);
    }
    match cmp {
        Comparison::UpToDate | Comparison::Ahead => {
            render_check_result(&mut deps.stdout, cmp, embedded_version, &tag)
        }
        Comparison::NewerAvailable => perform_full_update(deps, embedded_triple, &tag),
    }
}

#[derive(Debug, Error)]
enum QueryError {
    #[error("tk self-update: failed to query GitHub Releases API: network error")]
    Network,
    #[error("tk self-update: failed to query GitHub Releases API: TLS handshake failed")]
    Tls,
    #[error("tk self-update: GitHub Releases API returned HTTP {0}")]
    HttpStatus(u16),
    #[error("tk self-update: GitHub Releases API returned an unparseable response")]
    Malformed,
    #[error("tk self-update: GitHub Releases API response did not include a tag_name")]
    MissingTag,
}

fn render_query_error<W: Write + ?Sized>(stderr: &mut W, err: &QueryError) {
    let _ = writeln!(stderr, "{err}");
}

fn fetch_latest_tag<R: crate::proc::ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    embedded_version: &str,
) -> Result<String, QueryError> {
    let user_agent = format!("{USER_AGENT_PREFIX}{embedded_version}");
    let argv = curl_get_argv(&user_agent, API_URL, None);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = runner.run(&argv_refs, cwd).map_err(|_| QueryError::Network)?;
    let (body, status) = split_body_and_status(&output.stdout)?;
    classify_curl_outcome(output.exit_code, status)?;
    parse_release_tag(body)
}

fn parse_release_tag(body: &[u8]) -> Result<String, QueryError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| QueryError::Malformed)?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or(QueryError::MissingTag)?;
    if tag.is_empty() {
        return Err(QueryError::MissingTag);
    }
    Ok(tag.to_string())
}

/// Curl writes the response body, then `%{http_code}` on a trailing line.
/// Split on the final newline; absent split (empty body, no newline)
/// surfaces as [`QueryError::Malformed`].
fn split_body_and_status(stdout: &[u8]) -> Result<(&[u8], u16), QueryError> {
    let bytes = trim_trailing_newline(stdout);
    let last_nl = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .ok_or(QueryError::Malformed)?;
    let (body, rest) = bytes.split_at(last_nl);
    let status_str = std::str::from_utf8(&rest[1..]).map_err(|_| QueryError::Malformed)?;
    let status: u16 = status_str.trim().parse().map_err(|_| QueryError::Malformed)?;
    Ok((body, status))
}

fn trim_trailing_newline(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
        end -= 1;
    }
    &bytes[..end]
}

/// Map curl's exit code and observed HTTP status into the canonical
/// outcome. Curl exit codes from `man curl(1)`:
/// - 0: transfer completed (inspect `%{http_code}`)
/// - 6 / 7 / 28 / 56: DNS, connect, timeout, recv — transport-layer
/// - 35 / 51 / 60: TLS handshake / cert verify failures
/// - else: lump under network so the user retries before reporting
fn classify_curl_outcome(exit_code: i32, status: u16) -> Result<(), QueryError> {
    if exit_code != 0 {
        return Err(match exit_code {
            35 | 51 | 60 => QueryError::Tls,
            _ => QueryError::Network,
        });
    }
    if !(200..300).contains(&status) {
        return Err(QueryError::HttpStatus(status));
    }
    Ok(())
}

fn curl_get_argv(user_agent: &str, url: &str, output_path: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "curl".to_string(),
        "-sSL".to_string(),
        "--max-redirs".to_string(),
        "10".to_string(),
        "-A".to_string(),
        user_agent.to_string(),
        "-w".to_string(),
        "\n%{http_code}".to_string(),
    ];
    if let Some(path) = output_path {
        argv.push("-o".to_string());
        argv.push(path.to_string());
    }
    argv.push(url.to_string());
    argv
}

#[derive(Debug, Clone, Copy)]
enum Comparison {
    UpToDate,
    NewerAvailable,
    Ahead,
}

#[derive(Debug, Error)]
enum VersionParseError {
    #[error("tk self-update: embedded version is not valid semver: {0}")]
    Embedded(String),
    #[error("tk self-update: latest release tag is not valid semver: {0}")]
    Latest(String),
}

fn compare_versions(embedded: &str, latest: &str) -> Result<Comparison, VersionParseError> {
    let lhs = parse_semver(embedded).map_err(|()| VersionParseError::Embedded(embedded.to_string()))?;
    let rhs = parse_semver(latest).map_err(|()| VersionParseError::Latest(latest.to_string()))?;
    Ok(match lhs.cmp(&rhs) {
        std::cmp::Ordering::Equal => Comparison::UpToDate,
        std::cmp::Ordering::Less => Comparison::NewerAvailable,
        std::cmp::Ordering::Greater => Comparison::Ahead,
    })
}

/// Strip a leading `v` (universal release-tag convention, though strictly
/// outside semver.org's grammar) and split `MAJOR.MINOR.PATCH` into a
/// comparable tuple. Pre-release and build metadata are intentionally
/// rejected — every shipped tag is a plain three-component semver.
fn parse_semver(s: &str) -> Result<(u64, u64, u64), ()> {
    let stripped = s.strip_prefix('v').unwrap_or(s);
    let mut parts = stripped.split('.');
    let major: u64 = parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let minor: u64 = parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let patch: u64 = parts.next().ok_or(())?.parse().map_err(|_| ())?;
    if parts.next().is_some() {
        return Err(());
    }
    Ok((major, minor, patch))
}

fn render_check_result<W: Write + ?Sized>(
    stdout: &mut W,
    cmp: Comparison,
    embedded_version: &str,
    latest_tag: &str,
) -> u8 {
    match cmp {
        Comparison::UpToDate => {
            let _ = writeln!(
                stdout,
                "tk self-update: already on latest release {latest_tag}"
            );
            0
        }
        Comparison::NewerAvailable => {
            let _ = writeln!(
                stdout,
                "tk self-update: newer release available: {latest_tag} (current: {embedded_version})"
            );
            1
        }
        Comparison::Ahead => {
            let _ = writeln!(
                stdout,
                "tk self-update: local build {embedded_version} is ahead of latest published release {latest_tag}; nothing to do"
            );
            0
        }
    }
}

fn render_version_parse_failure<W: Write + ?Sized>(
    stderr: &mut W,
    err: &VersionParseError,
    _embedded_version: &str,
    _latest_tag: &str,
) {
    let _ = writeln!(stderr, "{err}");
}

/// Resolve the running binary's exe directory and basename, then delegate
/// to [`perform_update`]. Tests bypass the `current_exe` resolution by
/// calling [`perform_update`] directly with a tmpdir.
fn perform_full_update(deps: Deps<'_>, embedded_triple: &str, latest_tag: &str) -> u8 {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(
                deps.stderr,
                "tk self-update: cannot resolve current binary path: {err}"
            );
            return 1;
        }
    };
    let Some(exe_dir) = exe_path.parent() else {
        let _ = writeln!(
            deps.stderr,
            "tk self-update: cannot resolve current binary path: no parent directory"
        );
        return 1;
    };
    let Some(target_name) = exe_path.file_name().and_then(|n| n.to_str()) else {
        let _ = writeln!(
            deps.stderr,
            "tk self-update: cannot resolve current binary path: non-utf8 basename"
        );
        return 1;
    };
    perform_update(deps, exe_dir, target_name, embedded_triple, latest_tag)
}

/// Stage → download → smoke → atomic rename. Pure of `current_exe` so
/// tests can pass a tmpdir as `target_dir_path` and exercise the full
/// pipeline without relying on the running test-binary's location.
///
/// On POSIX, the final rename is atomic and the running inode stays
/// alive through the swap. On Windows, a rename-self pattern is used
/// because the live `.exe` cannot be overwritten; see [`commit_install`].
fn perform_update(
    deps: Deps<'_>,
    target_dir: &Path,
    target_name: &str,
    embedded_triple: &str,
    latest_tag: &str,
) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        rng,
        cwd,
        ..
    } = deps;

    let stage_name = render_stage_name(rng);
    let target_path = target_dir.join(target_name);
    let stage_path = target_dir.join(&stage_name);

    let asset_name = build_asset_name(embedded_triple);
    let asset_url = format!("{RELEASES_BASE_URL}/latest/download/{asset_name}");

    // The exclusive-create probe is the write-access gate before any
    // network traffic. AccessDenied here means the exe directory is
    // read-only; surface the canonical permission-denied diagnostic.
    if let Err(err) = create_exclusive(&stage_path) {
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            let _ = writeln!(
                stderr,
                "tk self-update: cannot write to {}: permission denied",
                target_path.display()
            );
        } else {
            let _ = writeln!(
                stderr,
                "tk self-update: failed to stage download at {}: {err}",
                stage_path.display()
            );
        }
        return 1;
    }

    let user_agent = format!("{USER_AGENT_PREFIX}{}", embedded_version());
    let Some(stage_path_str) = stage_path.to_str() else {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: failed to stage download at {}: non-utf8 path",
            stage_path.display()
        );
        return 1;
    };
    let argv = curl_get_argv(&user_agent, &asset_url, Some(stage_path_str));
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

    let Ok(dl_output) = runner.run(&argv_refs, cwd) else {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: failed to download release asset: network error"
        );
        return 1;
    };
    let Some(status) = parse_status_only(&dl_output.stdout) else {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: failed to download release asset: server returned an unparseable response (redirect loop or invalid headers)"
        );
        return 1;
    };
    if let Err(err) = classify_curl_outcome(dl_output.exit_code, status) {
        let _ = fs::remove_file(&stage_path);
        match err {
            QueryError::Tls => {
                let _ = writeln!(
                    stderr,
                    "tk self-update: failed to download release asset: TLS handshake failed"
                );
            }
            QueryError::Network => {
                let _ = writeln!(
                    stderr,
                    "tk self-update: failed to download release asset: network error"
                );
            }
            QueryError::HttpStatus(404) => {
                let _ = writeln!(
                    stderr,
                    "tk self-update: {asset_name} is not in release {latest_tag}. Install manually from {RELEASES_BASE_URL}/tag/{latest_tag}"
                );
            }
            QueryError::HttpStatus(code) => {
                let _ = writeln!(stderr, "tk self-update: asset download returned HTTP {code}");
            }
            QueryError::Malformed | QueryError::MissingTag => unreachable!(),
        }
        return 1;
    }

    // Make the staged file executable on POSIX so the smoke subprocess can
    // exec it directly without an explicit chmod step. Windows ignores
    // POSIX permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o755)) {
            let _ = fs::remove_file(&stage_path);
            let _ = writeln!(
                stderr,
                "tk self-update: failed to write staged binary: {err}"
            );
            return 1;
        }
    }

    // Smoke verify: `<stage> --version` must exit 0 AND stdout must
    // contain both the tag and the triple as whole tokens. Whole-token
    // containment defends against prefix collisions
    // (`v0.6.0` matching `v0.6.0-rc1`) and forward-compat across
    // `--version` formatting changes between releases.
    let smoke_argv: Vec<&str> = vec![stage_path_str, "--version"];
    let smoke = match runner.run(&smoke_argv, cwd) {
        Ok(o) => o,
        Err(err) => {
            let _ = fs::remove_file(&stage_path);
            let _ = writeln!(
                stderr,
                "tk self-update: staged binary smoke check failed: spawn failed ({err})"
            );
            return 1;
        }
    };
    if smoke.exit_code != 0 {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: staged binary smoke check failed: exit {}",
            smoke.exit_code
        );
        let trimmed = trim_whitespace(&smoke.stderr);
        if !trimmed.is_empty() {
            let _ = stderr.write_all(trimmed);
            let _ = stderr.write_all(b"\n");
        }
        return 1;
    }
    if !smoke_output_contains_token(&smoke.stdout, latest_tag) {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: staged binary did not report expected version: {latest_tag}"
        );
        return 1;
    }
    if !smoke_output_contains_token(&smoke.stdout, embedded_triple) {
        let _ = fs::remove_file(&stage_path);
        let _ = writeln!(
            stderr,
            "tk self-update: staged binary did not report expected triple: {embedded_triple}"
        );
        return 1;
    }

    match commit_install(target_dir, &stage_name, target_name, platform::IS_WINDOWS) {
        CommitOutcome::Ok => {}
        CommitOutcome::PrimaryFailed(err) => {
            let _ = fs::remove_file(&stage_path);
            let _ = writeln!(stderr, "tk self-update: failed to install new binary: {err}");
            return 1;
        }
        CommitOutcome::PrimaryRecovered(err) => {
            let _ = fs::remove_file(&stage_path);
            let _ = writeln!(
                stderr,
                "tk self-update: failed to install new binary: {err}; rolled back to previous binary"
            );
            return 1;
        }
        CommitOutcome::RollbackFailed { primary, rollback } => {
            let _ = writeln!(
                stderr,
                "tk self-update: cannot recover from rename failure: primary={primary}, rollback={rollback}; original preserved at {target_name}.old (restore with: mv {target_name}.old {target_name})"
            );
            return 1;
        }
    }

    let _ = writeln!(stdout, "tk self-update: updated to {latest_tag}");

    // Delegate the manpage install to the newly-installed binary so its
    // own embedded bytes are written (matched-version docs). Failure is
    // warn-and-continue per tk-32: the binary swap stands.
    let target_path_str = target_path.to_str().unwrap_or(target_name);
    let manpage_argv: Vec<&str> = vec![target_path_str, "manpage", "--install"];
    let manpage = match runner.run(&manpage_argv, cwd) {
        Ok(o) => o,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "tk self-update: manpage update failed; run `tk manpage --install` to retry: spawn failed: {err}"
            );
            return 1;
        }
    };
    if manpage.exit_code != 0 {
        let _ = writeln!(
            stderr,
            "tk self-update: manpage update failed; run `tk manpage --install` to retry: exit {}",
            manpage.exit_code
        );
        let trimmed = trim_whitespace(&manpage.stderr);
        if !trimmed.is_empty() {
            let _ = stderr.write_all(trimmed);
            let _ = stderr.write_all(b"\n");
        }
        return 1;
    }
    0
}

/// Exclusive create that surfaces `PermissionDenied` distinctly from
/// other I/O failures, mirroring the Zig oracle's AccessDenied probe.
fn create_exclusive(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Extract `%{http_code}` written by curl with `-w '\n%{http_code}'`
/// when `-o` consumes the body to disk (asset download path; the body
/// never reaches stdout).
fn parse_status_only(stdout: &[u8]) -> Option<u16> {
    let trimmed = trim_trailing_newline(stdout);
    let last_nl = trimmed.iter().rposition(|b| *b == b'\n');
    let code_bytes = match last_nl {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    };
    std::str::from_utf8(code_bytes).ok()?.trim().parse().ok()
}

fn trim_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !matches!(*b, b' ' | b'\t' | b'\r' | b'\n'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !matches!(*b, b' ' | b'\t' | b'\r' | b'\n'))
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

/// Outcome of [`commit_install`]. POSIX collapses to `Ok` or
/// `PrimaryFailed`. Windows can also reach `PrimaryRecovered` (step 2
/// failed but the rollback restored the original binary) and
/// `RollbackFailed` (step 2 failed AND the rollback also failed — the
/// canonical path is empty and the original survives at `<target>.old`).
#[derive(Debug)]
enum CommitOutcome {
    Ok,
    PrimaryFailed(std::io::Error),
    PrimaryRecovered(std::io::Error),
    RollbackFailed {
        primary: std::io::Error,
        rollback: std::io::Error,
    },
}

/// Commit a staged binary into place.
///
/// On POSIX, one atomic rename does the job; the running process's inode
/// stays alive across the swap. On Windows (`use_windows_pattern == true`),
/// the running `.exe` cannot be overwritten directly, so a rename-self
/// pattern is used: the current target is moved aside to `<target>.old`
/// first, then the stage is renamed into place. A second-rename failure
/// attempts to roll back by renaming `.old` back to the target name; if
/// both fail, the catastrophic state surfaces and the original is
/// preserved at `<target>.old` for manual recovery.
///
/// The Windows branch is exercised on POSIX by setting
/// `use_windows_pattern == true` explicitly in tests. POSIX rename
/// semantics happen to support the rename-self steps too, so the rename
/// mechanics are testable without a Windows host. Actual Windows open-
/// handle semantics still need a Windows runner.
fn commit_install(
    target_dir: &Path,
    stage_name: &str,
    target_name: &str,
    use_windows_pattern: bool,
) -> CommitOutcome {
    let stage_path = target_dir.join(stage_name);
    let target_path = target_dir.join(target_name);

    if !use_windows_pattern {
        return match fs::rename(&stage_path, &target_path) {
            Ok(()) => CommitOutcome::Ok,
            Err(err) => CommitOutcome::PrimaryFailed(err),
        };
    }

    let old_path = target_dir.join(format!("{target_name}.old"));

    // Step 1: move current target aside. A missing target file is OK —
    // first-time installs land in directories without a prior binary.
    if let Err(err) = fs::rename(&target_path, &old_path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return CommitOutcome::PrimaryFailed(err);
        }
    }

    // Step 2: place new bytes at the target name. On failure, undo
    // step 1 so the user is left with a working binary at the original
    // path rather than a missing one. If rollback also fails, surface the
    // catastrophic state — the original survives at `<target>.old`.
    if let Err(primary) = fs::rename(&stage_path, &target_path) {
        if let Err(rollback) = fs::rename(&old_path, &target_path) {
            return CommitOutcome::RollbackFailed { primary, rollback };
        }
        return CommitOutcome::PrimaryRecovered(primary);
    }

    CommitOutcome::Ok
}

/// Best-effort cleanup of a stale `<exe-dir>/tk.exe.old` left behind by a
/// previous Windows self-update. Called early from `main` under
/// [`platform::IS_WINDOWS`]. Failure is silent: the file may not exist,
/// may be held open, or the user may lack permission — none of those
/// should block a normal `tk` invocation.
///
/// Safety: only delete the `.old` sidecar when the canonical binary
/// exists at the exe path. Otherwise we may be removing the user's only
/// recoverable copy after a [`CommitOutcome::RollbackFailed`] outcome.
pub fn cleanup_stale_exe() {
    let Ok(exe_path) = std::env::current_exe() else {
        return;
    };
    cleanup_stale_exe_at(&exe_path);
}

fn cleanup_stale_exe_at(exe_path: &Path) {
    let Some(exe_dir) = exe_path.parent() else {
        return;
    };
    let old_path = exe_dir.join("tk.exe.old");
    if exe_path.exists() {
        let _ = fs::remove_file(old_path);
    }
}

/// Construct the asset basename for the running triple. `.exe` is
/// appended for Windows triples so the manual download URL matches what
/// GitHub serves and the Windows CreateProcessW path finds the staged
/// file by extension.
fn build_asset_name(triple: &str) -> String {
    let ext = if is_windows_triple(triple) { ".exe" } else { "" };
    format!("tk-{triple}{ext}")
}

/// Match any Windows ABI suffix — `-windows-gnu`, `-windows-msvc`, etc. —
/// so future Windows triples pick up `.exe` without silently 404ing.
fn is_windows_triple(triple: &str) -> bool {
    triple.contains("-windows-")
}

/// Token-anchored substring check for smoke verification: returns true
/// only if `token` appears as a whole word in `text`, with whitespace or
/// `()` as separators. Empty `token` returns false. Defends against
/// prefix collisions like `v0.6.0` matching `v0.6.0-rc1`.
fn smoke_output_contains_token(text: &[u8], token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = token.as_bytes();
    let separators = [b' ', b'\t', b'\r', b'\n', b'(', b')'];
    let mut start = 0;
    let len = text.len();
    while start < len {
        while start < len && separators.contains(&text[start]) {
            start += 1;
        }
        let mut end = start;
        while end < len && !separators.contains(&text[end]) {
            end += 1;
        }
        if &text[start..end] == bytes {
            return true;
        }
        start = end;
    }
    false
}

/// Build the staged binary's filename: `.tk.tmp.<8-byte-hex>`. The hex
/// suffix is 64 random bits so concurrent self-updates against the same
/// exe directory cannot collide on a stage path.
fn render_stage_name<R: Rng + ?Sized>(rng: &mut R) -> String {
    let mut bytes = [0u8; 8];
    rng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(STAGE_NAME_PREFIX.len() + 16);
    s.push_str(STAGE_NAME_PREFIX);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const TEST_TRIPLE: &str = "x86_64-linux-musl";

    fn make_deps<'a>(
        stdout: &'a mut Vec<u8>,
        stderr: &'a mut Vec<u8>,
        stdin: &'a mut std::io::Cursor<Vec<u8>>,
        runner: &'a FakeRunner,
        clock: &'a FakeClock,
        rng: &'a mut StdRng,
        cwd: &'a Path,
    ) -> Deps<'a> {
        Deps {
            stdout,
            stderr,
            stdin,
            runner,
            clock,
            rng,
            cwd,
            styler: Styler::plain(),
        }
    }

    fn ok_body(body: &str, status: u16) -> RunOutput {
        let mut bytes = body.as_bytes().to_vec();
        bytes.push(b'\n');
        bytes.extend_from_slice(status.to_string().as_bytes());
        RunOutput {
            exit_code: 0,
            stdout: bytes,
            stderr: Vec::new(),
        }
    }

    fn ok_status_only(status: u16) -> RunOutput {
        let mut bytes = Vec::new();
        bytes.push(b'\n');
        bytes.extend_from_slice(status.to_string().as_bytes());
        RunOutput {
            exit_code: 0,
            stdout: bytes,
            stderr: Vec::new(),
        }
    }

    #[test]
    fn dev_build_refuses_without_flags() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: false }, "v0.0.1", DEV_TRIPLE);
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("development builds cannot self-update"));
    }

    #[test]
    fn dev_build_refuses_check_the_same_way() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.0.1", DEV_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("development builds cannot self-update"));
    }

    #[test]
    fn check_up_to_date_exits_0() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":"v0.5.0"}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 0);
        let s = String::from_utf8(stdout).unwrap();
        assert!(s.contains("already on latest release v0.5.0"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn check_newer_available_exits_1() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":"v0.6.0"}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stdout).unwrap();
        assert!(s.contains("newer release available: v0.6.0"));
        assert!(s.contains("current: v0.5.0"));
    }

    #[test]
    fn check_ahead_exits_0() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":"v0.4.0"}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 0);
        let s = String::from_utf8(stdout).unwrap();
        assert!(s.contains("local build v0.5.0"));
        assert!(s.contains("latest published release v0.4.0"));
    }

    #[test]
    fn check_ignores_unknown_json_fields() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(
            &["curl"],
            ok_body(
                r#"{"tag_name":"v0.5.0","name":"Release v0.5.0","prerelease":false}"#,
                200,
            ),
        );
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 0);
        let s = String::from_utf8(stdout).unwrap();
        assert!(s.contains("already on latest release"));
    }

    #[test]
    fn check_http_5xx_surfaces_status_code() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body("", 503));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("GitHub Releases API returned HTTP 503"));
    }

    #[test]
    fn check_network_error_surfaces_transport_diagnostic() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        // curl exit 6 = "couldn't resolve host"
        runner.expect(
            &["curl"],
            RunOutput {
                exit_code: 6,
                stdout: b"\n000".to_vec(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("failed to query GitHub Releases API: network error"));
    }

    #[test]
    fn check_tls_error_surfaces_handshake_diagnostic() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        // curl exit 60 = "Peer certificate cannot be authenticated"
        runner.expect(
            &["curl"],
            RunOutput {
                exit_code: 60,
                stdout: b"\n000".to_vec(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("TLS handshake failed"));
    }

    #[test]
    fn check_malformed_json_surfaces_parse_failure() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body("{not valid json", 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("unparseable response"));
    }

    #[test]
    fn check_missing_tag_field() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":""}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("did not include a tag_name"));
    }

    #[test]
    fn check_unparseable_latest_surfaces_tag() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":"not-semver"}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "v0.5.0", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("latest release tag is not valid semver: not-semver"));
    }

    #[test]
    fn check_unparseable_embedded_surfaces_version() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        runner.expect(&["curl"], ok_body(r#"{"tag_name":"v0.5.0"}"#, 200));
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = run_with(deps, Args { check: true }, "not-semver", TEST_TRIPLE);
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("embedded version is not valid semver: not-semver"));
    }

    fn predict_stage_name(seed: u64) -> String {
        let mut rng = StdRng::seed_from_u64(seed);
        render_stage_name(&mut rng)
    }

    #[test]
    fn perform_update_posix_happy_path_renames_stage_into_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);
        let target_path = target_dir.join("tk");

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(200),
            stage_path.clone(),
            b"new-bytes".to_vec(),
        );
        runner.expect(
            &[stage_path.to_str().unwrap(), "--version"],
            RunOutput {
                exit_code: 0,
                stdout: b"tk v0.6.0 (x86_64-linux-musl)\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        runner.expect(
            &[target_path.to_str().unwrap(), "manpage", "--install"],
            RunOutput {
                exit_code: 0,
                stdout: b"ok\n".to_vec(),
                stderr: Vec::new(),
            },
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
        assert_eq!(fs::read(&target_path).unwrap(), b"new-bytes");
        assert!(!stage_path.exists());
    }

    #[test]
    fn perform_update_asset_404_renders_unified_missing_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(404),
            stage_path.clone(),
            Vec::new(),
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", "aarch64-windows-gnu", "v0.6.0");
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("tk-aarch64-windows-gnu.exe is not in release v0.6.0"));
        assert!(s.contains("releases/tag/v0.6.0"));
        assert!(!stage_path.exists());
        assert!(!target_dir.join("tk").exists());
    }

    #[test]
    fn perform_update_asset_5xx_surfaces_status_code() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(503),
            stage_path.clone(),
            Vec::new(),
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("asset download returned HTTP 503"));
    }

    #[test]
    fn perform_update_smoke_exit_nonzero_leaves_target_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(200),
            stage_path.clone(),
            b"junk".to_vec(),
        );
        runner.expect(
            &[stage_path.to_str().unwrap(), "--version"],
            RunOutput {
                exit_code: 7,
                stdout: Vec::new(),
                stderr: b"tk: corrupt embedded payload\n".to_vec(),
            },
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("staged binary smoke check failed: exit 7"));
        assert!(s.contains("tk: corrupt embedded payload"));
        assert!(!stage_path.exists());
        assert!(!target_dir.join("tk").exists());
    }

    #[test]
    fn perform_update_smoke_version_mismatch_leaves_target_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(200),
            stage_path.clone(),
            b"bytes".to_vec(),
        );
        runner.expect(
            &[stage_path.to_str().unwrap(), "--version"],
            RunOutput {
                exit_code: 0,
                stdout: b"tk v9.9.9 (x86_64-linux-musl)\n".to_vec(),
                stderr: Vec::new(),
            },
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("did not report expected version: v0.6.0"));
        assert!(!target_dir.join("tk").exists());
    }

    #[test]
    fn perform_update_smoke_triple_mismatch_leaves_target_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(200),
            stage_path.clone(),
            b"bytes".to_vec(),
        );
        runner.expect(
            &[stage_path.to_str().unwrap(), "--version"],
            RunOutput {
                exit_code: 0,
                stdout: b"tk v0.6.0 (aarch64-macos)\n".to_vec(),
                stderr: Vec::new(),
            },
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 1);
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("did not report expected triple: x86_64-linux-musl"));
        assert!(!target_dir.join("tk").exists());
    }

    #[test]
    fn perform_update_manpage_failure_warns_but_preserves_binary_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        let stage_name = predict_stage_name(0);
        let stage_path = target_dir.join(&stage_name);
        let target_path = target_dir.join("tk");

        let runner = FakeRunner::new();
        runner.expect_writing(
            &["curl"],
            ok_status_only(200),
            stage_path.clone(),
            b"new-bytes".to_vec(),
        );
        runner.expect(
            &[stage_path.to_str().unwrap(), "--version"],
            RunOutput {
                exit_code: 0,
                stdout: b"tk v0.6.0 (x86_64-linux-musl)\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        runner.expect(
            &[target_path.to_str().unwrap(), "manpage", "--install"],
            RunOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: b"tk manpage: install failed at /some/path\n".to_vec(),
            },
        );

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let cwd = std::env::current_dir().unwrap();
        let deps = make_deps(
            &mut stdout,
            &mut stderr,
            &mut stdin,
            &runner,
            &clock,
            &mut rng,
            &cwd,
        );
        let code = perform_update(deps, target_dir, "tk", TEST_TRIPLE, "v0.6.0");
        assert_eq!(code, 1);
        let out = String::from_utf8(stdout).unwrap();
        assert!(out.contains("updated to v0.6.0"));
        assert_eq!(fs::read(&target_path).unwrap(), b"new-bytes");
        let s = String::from_utf8(stderr).unwrap();
        assert!(s.contains("manpage update failed"));
        assert!(s.contains("exit 1"));
        assert!(s.contains("tk manpage: install failed at /some/path"));
    }

    #[test]
    fn build_asset_name_appends_exe_for_any_windows_abi() {
        let cases = [
            ("x86_64-linux-musl", "tk-x86_64-linux-musl"),
            ("aarch64-linux-musl", "tk-aarch64-linux-musl"),
            ("aarch64-apple-darwin", "tk-aarch64-apple-darwin"),
            ("x86_64-pc-windows-gnu", "tk-x86_64-pc-windows-gnu.exe"),
            ("aarch64-windows-msvc", "tk-aarch64-windows-msvc.exe"),
        ];
        for (triple, expected) in cases {
            assert_eq!(build_asset_name(triple), expected, "for {triple}");
        }
    }

    #[test]
    fn commit_install_posix_atomically_replaces_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        fs::write(target_dir.join(".tk.tmp.aaaa"), "new-bytes").unwrap();
        fs::write(target_dir.join("tk"), "old-bytes").unwrap();

        let outcome = commit_install(target_dir, ".tk.tmp.aaaa", "tk", false);
        assert!(matches!(outcome, CommitOutcome::Ok));
        assert_eq!(fs::read(target_dir.join("tk")).unwrap(), b"new-bytes");
        assert!(!target_dir.join(".tk.tmp.aaaa").exists());
        assert!(!target_dir.join("tk.old").exists());
    }

    #[test]
    fn commit_install_windows_moves_current_to_old_then_places_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        fs::write(target_dir.join(".tk.tmp.bbbb"), "new-bytes").unwrap();
        fs::write(target_dir.join("tk.exe"), "old-bytes").unwrap();

        let outcome = commit_install(target_dir, ".tk.tmp.bbbb", "tk.exe", true);
        assert!(matches!(outcome, CommitOutcome::Ok));
        assert_eq!(fs::read(target_dir.join("tk.exe")).unwrap(), b"new-bytes");
        assert_eq!(fs::read(target_dir.join("tk.exe.old")).unwrap(), b"old-bytes");
        assert!(!target_dir.join(".tk.tmp.bbbb").exists());
    }

    #[test]
    fn commit_install_windows_tolerates_absent_target_first_install() {
        let tmp = tempfile::tempdir().unwrap();
        let target_dir = tmp.path();
        fs::write(target_dir.join(".tk.tmp.cccc"), "first-bytes").unwrap();

        let outcome = commit_install(target_dir, ".tk.tmp.cccc", "tk.exe", true);
        assert!(matches!(outcome, CommitOutcome::Ok));
        assert_eq!(fs::read(target_dir.join("tk.exe")).unwrap(), b"first-bytes");
        assert!(!target_dir.join("tk.exe.old").exists());
    }

    #[test]
    fn cleanup_stale_exe_deletes_old_sidecar_when_canonical_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_path = tmp.path().join("tk.exe");
        let old_path = tmp.path().join("tk.exe.old");
        fs::write(&exe_path, "current").unwrap();
        fs::write(&old_path, "stale").unwrap();

        cleanup_stale_exe_at(&exe_path);
        assert!(!old_path.exists());
    }

    #[test]
    fn cleanup_stale_exe_preserves_old_when_canonical_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_path = tmp.path().join("tk.exe");
        let old_path = tmp.path().join("tk.exe.old");
        fs::write(&old_path, "the-only-copy").unwrap();
        // No tk.exe at all.

        cleanup_stale_exe_at(&exe_path);
        assert_eq!(fs::read(&old_path).unwrap(), b"the-only-copy");
    }

    #[test]
    fn cleanup_stale_exe_noop_when_old_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let exe_path = tmp.path().join("tk.exe");
        fs::write(&exe_path, "current").unwrap();
        // No tk.exe.old.

        cleanup_stale_exe_at(&exe_path);
    }

    #[test]
    fn render_stage_name_prefix_and_hex_suffix() {
        let mut rng = StdRng::seed_from_u64(42);
        let name = render_stage_name(&mut rng);
        assert!(name.starts_with(STAGE_NAME_PREFIX));
        let suffix = &name[STAGE_NAME_PREFIX.len()..];
        assert_eq!(suffix.len(), 16);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn smoke_output_contains_token_whole_word_only() {
        assert!(smoke_output_contains_token(
            b"tk v0.6.0 (x86_64-linux-musl)\n",
            "v0.6.0"
        ));
        assert!(smoke_output_contains_token(
            b"tk v0.6.0 (x86_64-linux-musl)\n",
            "x86_64-linux-musl"
        ));
        // Prefix collision: "v0.6.0" should NOT match inside "v0.6.0-rc1"
        assert!(!smoke_output_contains_token(b"tk v0.6.0-rc1\n", "v0.6.0"));
        // Empty token never matches.
        assert!(!smoke_output_contains_token(b"anything", ""));
    }

    #[test]
    fn parse_semver_strips_leading_v() {
        assert_eq!(parse_semver("v0.5.0"), Ok((0, 5, 0)));
        assert_eq!(parse_semver("0.5.0"), Ok((0, 5, 0)));
        assert!(parse_semver("not-semver").is_err());
        assert!(parse_semver("0.5").is_err());
        assert!(parse_semver("0.5.0.1").is_err());
    }

    #[test]
    fn parse_release_tag_rejects_empty_string() {
        assert!(matches!(
            parse_release_tag(br#"{"tag_name":""}"#),
            Err(QueryError::MissingTag)
        ));
    }

    #[test]
    fn extract_version_head_keeps_prefix_before_space() {
        assert_eq!(extract_version_head("v0.6.0 (x86_64-linux-musl)"), "v0.6.0");
        assert_eq!(extract_version_head("v0.6.0"), "v0.6.0");
    }
}
