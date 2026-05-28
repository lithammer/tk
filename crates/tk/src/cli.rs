//! Top-level CLI dispatch and shared dependencies.
//!
//! ADR-0018 names this seam: every command takes a `Deps` view carrying
//! injectable I/O, clock, runner, RNG, and cwd so command-handler tests can
//! substitute fakes without touching the global environment.
//!
//! Slice 0 keeps dispatch hand-rolled while only `tk init` is ported; once a
//! second command lands (`tk show` is the natural next slice), this module
//! grows to a `clap_derive`-style command tuple. The Zig oracle keeps its
//! own dispatch in `src/cli.zig` until then.

use std::io::Write;
use std::path::Path;

use crate::clock::Clock;
use crate::commands;
use crate::proc::ProcRunner;
use crate::rng::Rng;

/// Dependencies shared by every command. Holds borrowed I/O streams and trait
/// objects for the determinism seams (`runner`, `clock`, `rng`).
pub struct Deps<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    pub runner: &'a dyn ProcRunner,
    pub clock: &'a dyn Clock,
    pub rng: &'a dyn Rng,
    pub cwd: &'a Path,
}

/// Entrypoint that the binary's `main.rs` and the scenario harness share.
///
/// `argv` is the post-`tk` argument vector — the first element is the
/// subcommand name (or `--help` / `--version`). Returns the process exit code.
///
/// # Errors
///
/// This signature carries an `io::Error` slot for symmetry with the Zig
/// oracle's `cli.runArgv`, where unexpected errors propagate to the process
/// shim. Slice 0 commands map their own failures internally and return the
/// exit code; the error variant is reserved for future commands that bubble
/// an unexpected `io::Error` through (`main.rs` already renders these as
/// exit 3).
pub fn run_argv(deps: Deps<'_>, argv: &[String]) -> std::io::Result<u8> {
    if argv.is_empty() {
        return Ok(write_top_help(deps));
    }

    let (cmd, rest) = argv.split_first().expect("argv non-empty checked above");
    match cmd.as_str() {
        "init" => Ok(commands::init::run(deps, rest)),
        "--help" | "-h" => Ok(write_top_help(deps)),
        // `-V` is clap's out-of-the-box version short flag; the hand-rolled
        // dispatch matches it so a future clap-derive migration is a no-op
        // on this surface. (The Zig oracle uses `-v` — that drift goes when
        // the Zig tree thaws.)
        "--version" | "-V" => Ok(write_version(deps)),
        unknown => {
            let _ = writeln!(
                deps.stderr,
                "tk: unknown command '{unknown}'. Run 'tk --help' for usage."
            );
            Ok(2)
        }
    }
}

fn write_top_help(deps: Deps<'_>) -> u8 {
    let help = "\
tk — repository-local work tracker

Usage:
  tk <command> [options]

Commands:
  init   Initialize the Repository Store in the current Git repository.

Options:
  -h, --help     Show this help.
  -V, --version  Show version.
";
    let _ = deps.stdout.write_all(help.as_bytes());
    0
}

fn write_version(deps: Deps<'_>) -> u8 {
    // Mirror the Zig oracle's `tk --version` shape: `<version> (<triple>)`.
    // Slice 0 hard-codes "0.0.0" for the version and uses Cargo's target
    // triple env var (set by `build.rs` or `cargo zigbuild`); fall back to
    // a deterministic placeholder when unset so scenarios stay byte-stable.
    let version = env!("CARGO_PKG_VERSION");
    let triple = option_env!("TARGET").unwrap_or("unknown-triple");
    let _ = writeln!(deps.stdout, "{version} ({triple})");
    0
}
