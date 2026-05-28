//! `tk` binary entrypoint.
//!
//! Mirrors the role `src/main.zig` plays in the Zig oracle: a thin process
//! shim that constructs real `Deps`, dispatches to `cli::run_argv`, and maps
//! an internal error to exit code 3. Command logic lives in `commands/`.

use std::io;
use std::process::ExitCode;

use tk::cli;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(err) => {
            // A vanished cwd (e.g. `rmdir`'d under the shell) would otherwise
            // manifest as a confusing `git rev-parse` failure downstream; the
            // exit-3 path matches the internal-error class `cli::run_argv`
            // reserves for unexpected I/O failure.
            let _ = io::Write::write_all(
                &mut io::stderr().lock(),
                format!("tk: failed to read current directory: {err}\n").as_bytes(),
            );
            return ExitCode::from(3);
        }
    };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();

    let runner = tk::proc::RealRunner::new();
    let clock = tk::clock::RealClock::new();
    let rng = tk::rng::RealRng::new();

    let deps = cli::Deps {
        stdout: &mut stdout,
        stderr: &mut stderr,
        runner: &runner,
        clock: &clock,
        rng: &rng,
        cwd: cwd.as_path(),
    };

    let code = match cli::run_argv(deps, &argv) {
        Ok(code) => code,
        Err(err) => {
            // Internal/unexpected error: surface the type as a last-resort
            // diagnostic and exit 3. Per ADR-0017, command-side stderr already
            // covers the curated error paths; this fallback only fires when a
            // command bubbles an `io::Error` or `rusqlite::Error` that wasn't
            // mapped.
            let _ = std::io::Write::write_all(
                &mut io::stderr().lock(),
                format!("tk: internal error: {err}\n").as_bytes(),
            );
            3
        }
    };
    ExitCode::from(code)
}
