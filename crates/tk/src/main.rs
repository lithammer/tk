//! `tk` binary entrypoint.
//!
//! Thin process shim: builds the real `Deps`, dispatches to `cli::run_argv`,
//! and maps an unexpected `Err` to exit code 3. Command logic lives under
//! [`tk::commands`].

use std::io;
use std::process::ExitCode;

use rand::SeedableRng;
use rand::rngs::{StdRng, SysRng};

use tk::cli;
use tk::render::{ColorChoice, resolve_styler_from_env};

fn main() -> ExitCode {
    // Best-effort sweep of a `tk.exe.old` sidecar left by a prior Windows
    // self-update commit. Safe to call early — the helper guards against
    // deleting the user's only recoverable copy when the canonical binary
    // is missing. POSIX self-updates use atomic rename and need no
    // cross-launch cleanup.
    if tk::platform::IS_WINDOWS {
        tk::commands::self_update::cleanup_stale_exe();
    }

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
            return ExitCode::from(cli::Exit::Internal.code());
        }
    };
    let styler = resolve_styler_from_env();

    // Wrap the real streams in `anstream::AutoStream` so SGR escape
    // codes the styler emits reach the terminal unmodified under
    // `Always` and are stripped under `Never`. Clap renders its own
    // help and errors via `StyledStr`'s `Display` impl, which is
    // colour-unaware (see `cli::render_clap_error`); the wrap covers
    // only output written through `deps.stdout` / `deps.stderr` by tk
    // itself.
    let mut stdout = anstream::AutoStream::new(io::stdout().lock(), to_anstream(styler.stdout));
    let mut stderr = anstream::AutoStream::new(io::stderr().lock(), to_anstream(styler.stderr));
    let stdin_handle = io::stdin();
    let mut stdin = stdin_handle.lock();

    let runner = tk::proc::RealRunner::new();
    let clock = tk::clock::RealClock::new();
    // Seed the RNG from OS entropy (rand's SysRng). `try_from_rng` only fails
    // when the OS RNG is unavailable (e.g. exhausted /dev/random on Linux);
    // treat that as fatal at startup rather than surfacing a confusing
    // partial-entropy state to commands.
    let mut rng: StdRng = StdRng::try_from_rng(&mut SysRng).unwrap_or_else(|err| {
        // Fatal-startup path: the AutoStream stderr hasn't been built yet (the
        // styler is constructed but `stderr` above captures the lock by move).
        // Write straight to the raw stderr lock so the diagnostic is unaffected
        // by policy.
        let _ = io::Write::write_all(
            &mut io::stderr().lock(),
            format!("tk: failed to seed RNG from OS entropy: {err}\n").as_bytes(),
        );
        std::process::exit(cli::Exit::Internal.code().into());
    });

    let deps = cli::Deps {
        stdout: &mut stdout,
        stderr: &mut stderr,
        stdin: &mut stdin,
        runner: &runner,
        clock: &clock,
        rng: &mut rng,
        cwd: cwd.as_path(),
        styler,
    };

    let exit = match cli::run_argv(deps, &argv) {
        Ok(exit) => exit,
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
            cli::Exit::Internal
        }
    };
    ExitCode::from(exit.code())
}

/// Translate tk's resolved [`ColorChoice`] into the `anstream`
/// equivalent. ADR-0014 keeps tk to SGR escape codes, so [`Always`]
/// maps to [`anstream::ColorChoice::AlwaysAnsi`] (which guarantees raw
/// SGR passthrough on every platform) rather than
/// [`anstream::ColorChoice::Always`] (which on Windows enables the
/// legacy console-API translation path tk does not target).
///
/// [`Always`]: ColorChoice::Always
fn to_anstream(choice: ColorChoice) -> anstream::ColorChoice {
    match choice {
        ColorChoice::Always => anstream::ColorChoice::AlwaysAnsi,
        ColorChoice::Never => anstream::ColorChoice::Never,
    }
}
