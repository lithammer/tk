//! Top-level CLI dispatch and shared dependencies.
//!
//! ADR-0018 names this seam: every command takes a `Deps` view carrying
//! injectable I/O, clock, runner, RNG, and cwd so command-handler tests can
//! substitute fakes without touching the global environment.
//!
//! Dispatch + help/version/usage rendering are owned by `clap`'s derive API.
//! Adding a new command is a matter of appending a variant to [`Command`] and
//! a handler in [`commands`]; clap takes care of `--help`, `-h`, `--version`,
//! `-V`, and suggestion-style errors on typos.

use std::io::{Read, Write};
use std::path::Path;

use clap::{Parser, Subcommand};
use rand::Rng;

use crate::clock::Clock;
use crate::commands;
use crate::proc::ProcRunner;
use crate::render::Styler;

/// Process exit status returned by every command handler.
///
/// The numeric `code()` values are a frozen contract (ADR-0018, "CLI flags /
/// stdout-stderr bytes / exit codes"): commands return the semantic variant and
/// `main` maps it to [`std::process::ExitCode`], so the magic `0`/`1`/`2`
/// literals never leak into command control flow.
///
/// - [`Exit::Ok`] (`0`) — success.
/// - [`Exit::Failure`] (`1`) — a curated command failure; the handler has
///   already written its ADR-0017 diagnostic to stderr.
/// - [`Exit::Usage`] (`2`) — a usage error (bad flags / arguments), including
///   clap's own parse failures.
/// - [`Exit::Internal`] (`3`) — an unexpected error bubbled out of a handler as
///   `io::Error` / `rusqlite::Error`; constructed only by `main`, never by a
///   command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok,
    Failure,
    Usage,
    Internal,
}

impl Exit {
    /// The frozen process exit code this status maps to.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failure => 1,
            Self::Usage => 2,
            Self::Internal => 3,
        }
    }
}

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
    /// Stdin reader; consumed only by write commands that accept
    /// `-F -` to read a Ticket message from a pipe.
    pub stdin: &'a mut dyn Read,
    pub runner: &'a dyn ProcRunner,
    pub clock: &'a dyn Clock,
    pub rng: &'a mut dyn Rng,
    pub cwd: &'a Path,
    /// Resolved per-stream colour choice (ADR-0014). Commands emitting
    /// styled output reach for `deps.styler.for_stdout()` /
    /// `for_stderr()` and let the returned `SubStyler` gate emission.
    /// The choice is resolved once at process startup from `NO_COLOR`,
    /// `CLICOLOR_FORCE`, and per-stream `IsTerminal`; command handlers
    /// never re-resolve it.
    pub styler: Styler,
}

/// Top-level argument parser.
///
/// `version = env!("TK_VERSION_STRING")` makes `tk --version` emit
/// `v<crate-version> (<triple>)` — the shape `tk self-update`'s smoke
/// verification (ADR-0013) scans for the embedded tag and triple as whole
/// tokens. `build.rs` injects `TK_VERSION_STRING`; the dev-build refusal
/// branch in `commands::self_update` keys off the `dev` triple sentinel.
#[derive(Debug, Parser)]
#[command(
    name = "tk",
    version = env!("TK_VERSION_STRING"),
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
    /// Create a local Ticket or Epic.
    Add(commands::add::Args),
    /// Initialize the Repository Store in the current Git repository.
    Init(commands::init::Args),
    /// Render the Repository Store List Tree.
    List(commands::list::Args),
    /// Select the next ready Ticket.
    Next(commands::next::Args),
    /// Render one Ticket or Epic with current state.
    Show(commands::show::Args),
    /// Update the title, body, priority, or parent of a Ticket or Epic.
    Update(commands::update::Args),
    /// Mark a Ticket or Epic active.
    Start(commands::start::Args),
    /// Return a Ticket or Epic to open.
    Stop(commands::stop::Args),
    /// Close a Ticket or Epic.
    Done(commands::done::Args),
    /// Record that one item blocks another.
    Block(commands::block::Args),
    /// Remove a blocking relationship.
    Unblock(commands::unblock::Args),
    /// Print the agent workflow briefing.
    Prime(commands::prime::Args),
    /// Print or install the tk manpage.
    Manpage(commands::manpage::Args),
    /// Replace the running tk binary with the latest release.
    SelfUpdate(commands::self_update::Args),
    /// Apply pending Mutations through the configured Remote.
    Sync(commands::sync::Args),
    /// Promote a Local Ticket or Epic through the configured Remote.
    Promote(commands::promote::Args),
}

/// Entrypoint that the binary's `main.rs` and the scenario harness share.
///
/// `argv` is the post-`tk` argument vector. Returns the process [`Exit`] status.
pub fn run_argv(deps: Deps<'_>, argv: &[String]) -> std::io::Result<Exit> {
    // `try_parse_from` expects argv[0] to be the binary name; prepend it.
    let full_argv: Vec<&str> = std::iter::once("tk")
        .chain(argv.iter().map(String::as_str))
        .collect();
    let cli = match Cli::try_parse_from(full_argv) {
        Ok(cli) => cli,
        Err(err) => return Ok(render_clap_error(deps, &err)),
    };
    match cli.command {
        Command::Add(args) => Ok(commands::add::run(deps, args)),
        Command::Init(args) => Ok(commands::init::run(deps, args)),
        Command::List(args) => Ok(commands::list::run(deps, args)),
        Command::Next(args) => Ok(commands::next::run(deps, args)),
        Command::Show(args) => Ok(commands::show::run(deps, args)),
        Command::Update(args) => Ok(commands::update::run(deps, args)),
        Command::Start(args) => Ok(commands::start::run(deps, args)),
        Command::Stop(args) => Ok(commands::stop::run(deps, args)),
        Command::Done(args) => Ok(commands::done::run(deps, args)),
        Command::Block(args) => Ok(commands::block::run(deps, args)),
        Command::Unblock(args) => Ok(commands::unblock::run(deps, args)),
        Command::Prime(args) => Ok(commands::prime::run(deps, args)),
        Command::Manpage(args) => Ok(commands::manpage::run(deps, args)),
        Command::SelfUpdate(args) => Ok(commands::self_update::run(deps, args)),
        Command::Sync(args) => Ok(commands::sync::run(deps, args)),
        Command::Promote(args) => Ok(commands::promote::run(deps, args)),
    }
}

/// Route `clap::Error` through `Deps` writers so command-handler tests can
/// capture --help / --version output, then map clap's exit code into our
/// process exit:
/// - `DisplayHelp` / `DisplayVersion`: success (exit 0), rendered to stdout.
/// - any other error (unknown flag, missing subcommand, …): exit 2,
///   rendered to stderr.
fn render_clap_error(deps: Deps<'_>, err: &clap::Error) -> Exit {
    use clap::error::ErrorKind;
    let Deps { stdout, stderr, .. } = deps;
    let rendered = err.render();
    let bytes = rendered.to_string();
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = stdout.write_all(bytes.as_bytes());
            Exit::Ok
        }
        _ => {
            let _ = stderr.write_all(bytes.as_bytes());
            Exit::Usage
        }
    }
}
