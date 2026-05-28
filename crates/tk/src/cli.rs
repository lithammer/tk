//! Top-level CLI dispatch and shared dependencies.
//!
//! ADR-0018 names this seam: every command takes a `Deps` view carrying
//! injectable I/O, clock, runner, RNG, and cwd so command-handler tests can
//! substitute fakes without touching the global environment.
//!
//! Dispatch + help/version/usage rendering are owned by `clap`'s derive API,
//! which retires the hand-rolled `writeHelp` per Zig command. Adding a new
//! command is a matter of appending a variant to [`Command`] and a handler in
//! [`commands`]; clap takes care of `--help`, `-h`, `--version`, `-V`, and
//! suggestion-style errors on typos.

use std::io::Write;
use std::path::Path;

use clap::{Parser, Subcommand};
use rand::Rng;

use crate::clock::Clock;
use crate::commands;
use crate::proc::ProcRunner;

/// Dependencies shared by every command. Holds borrowed I/O streams and trait
/// objects for the determinism seams (`runner`, `clock`, `rng`).
///
/// `rng` is `&mut dyn rand::Rng` — the low-level dyn-compatible trait in
/// `rand_core` 0.10 (its `RngCore` alias still exists as a marker `trait
/// RngCore: Rng {}`, but `Rng` is the primary surface). The convenience
/// `RngExt` trait is not object-safe and is auto-derived for any `Rng`
/// implementor, so commands can call `gen_range` etc. on `*deps.rng` once a
/// downstream slice imports the extension trait.
pub struct Deps<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    pub runner: &'a dyn ProcRunner,
    pub clock: &'a dyn Clock,
    pub rng: &'a mut dyn Rng,
    pub cwd: &'a Path,
}

/// Top-level argument parser.
#[derive(Debug, Parser)]
#[command(
    name = "tk",
    version,
    about = "Repository-local work tracker",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Subcommand registry. Add a variant here and a module under [`commands`]
/// to land a new command.
#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize the Repository Store in the current Git repository.
    Init(commands::init::Args),
}

/// Entrypoint that the binary's `main.rs` and the scenario harness share.
///
/// `argv` is the post-`tk` argument vector. Returns the process exit code.
pub fn run_argv(deps: Deps<'_>, argv: &[String]) -> std::io::Result<u8> {
    // `try_parse_from` expects argv[0] to be the binary name; prepend it.
    let full_argv: Vec<&str> = std::iter::once("tk")
        .chain(argv.iter().map(String::as_str))
        .collect();
    let cli = match Cli::try_parse_from(full_argv) {
        Ok(cli) => cli,
        Err(err) => return Ok(render_clap_error(deps, &err)),
    };
    match cli.command {
        Command::Init(args) => Ok(commands::init::run(deps, args)),
    }
}

/// Route `clap::Error` through `Deps` writers so command-handler tests can
/// capture --help / --version output, then map clap's exit code into our
/// process exit:
/// - `DisplayHelp` / `DisplayVersion`: success (exit 0), rendered to stdout.
/// - any other error (unknown flag, missing subcommand, …): exit 2,
///   rendered to stderr.
fn render_clap_error(deps: Deps<'_>, err: &clap::Error) -> u8 {
    use clap::error::ErrorKind;
    let rendered = err.render();
    let bytes = rendered.to_string();
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = deps.stdout.write_all(bytes.as_bytes());
            0
        }
        _ => {
            let _ = deps.stderr.write_all(bytes.as_bytes());
            2
        }
    }
}

