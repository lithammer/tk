//! `tk worktree` — inspect and configure Workspace Scope.
//!
//! Four subcommands:
//!
//! - bare `tk worktree`: render configured / inferred Workspace Scope.
//! - `tk worktree set <id>`: persist `tk.scope` for this worktree via
//!   `git config --worktree`.
//! - `tk worktree clear`: remove the configured scope (idempotent — git
//!   exit-5 "key already absent" treated as success).
//! - `tk worktree start <id> [path] [--no-status]`: create a `tk/<id>[-slug]`
//!   branch and a scoped git worktree, optionally marking the item active.
//!
//! The branch/path slugs are derived from the item's title via
//! [`worktree::scope::sanitize`]; the default worktree path is
//! `<parent of toplevel>/<repo>.<id>-<slug>`.

use std::io::Write;
use std::path::Path;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::Deps;
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::status::ItemStatus;
use crate::git::discovery;
use crate::proc::{ProcError, ProcRunner};
use crate::store::repository::Store;
use crate::store::repository::status::{
    self as set_status, SetStatusError, SetStatusRequest,
};
use crate::worktree::scope::{self as worktree_scope, ScopeError, ScopeSource};

/// Flags for `tk worktree`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub subcommand: Option<Sub>,
}

#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Configure Workspace Scope for this worktree.
    Set(SetArgs),
    /// Remove the configured Workspace Scope.
    Clear,
    /// Create a Ticket branch and scoped git worktree.
    Start(StartArgs),
}

#[derive(Debug, ClapArgs)]
pub struct SetArgs {
    /// Display ID or Alias of the Ticket or Epic.
    pub id: String,
}

#[derive(Debug, ClapArgs)]
pub struct StartArgs {
    /// Display ID or Alias of the Ticket or Epic to scope.
    pub id: String,
    /// Optional explicit worktree path. Defaults to
    /// `<parent of toplevel>/<repo>.<id>-<slug>`.
    pub path: Option<String>,
    /// Skip marking the scoped item active.
    #[arg(long = "no-status")]
    pub no_status: bool,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    match args.subcommand {
        None => run_status(deps),
        Some(Sub::Set(a)) => run_set(deps, a),
        Some(Sub::Clear) => run_clear(deps),
        Some(Sub::Start(a)) => run_start(deps, a),
    }
}

fn run_status(deps: Deps<'_>) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        cwd,
        ..
    } = deps;

    let store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, "worktree", &err);
            return 1;
        }
    };

    let raw = worktree_scope::read_git_side(runner, cwd);
    match worktree_scope::resolve_against_store(&store, &raw) {
        Ok(None) => {
            let _ = writeln!(stdout, "No Workspace Scope.");
            0
        }
        Ok(Some(s)) => {
            let _ = writeln!(stdout, "Scope:  {} - {}", s.display_id, s.title);
            match s.source {
                ScopeSource::Configured => {
                    let _ = writeln!(stdout, "Source: configured");
                }
                ScopeSource::Inferred => {
                    let branch = raw.branch_name.as_deref().unwrap_or("");
                    let _ = writeln!(stdout, "Source: inferred from branch '{branch}'");
                }
            }
            0
        }
        Err(ScopeError::ConfiguredUnresolved(stored)) => {
            let _ = writeln!(
                stderr,
                "tk worktree: Workspace Scope '{stored}' is not a known Display ID or Alias"
            );
            1
        }
        Err(ScopeError::Storage(err)) => {
            resolver::render_storage_error(stderr, "worktree", &err);
            1
        }
    }
}

fn run_set(deps: Deps<'_>, args: SetArgs) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        cwd,
        ..
    } = deps;

    let store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, "worktree set", &err);
            return 1;
        }
    };

    match resolver::resolve(&store, &args.id) {
        Ok(_) => {}
        Err(resolver::ResolveError::NotFound) => {
            let _ = writeln!(
                stderr,
                "tk worktree set: '{}' is not a known Display ID or Alias",
                args.id
            );
            return 1;
        }
        Err(resolver::ResolveError::Storage(err)) => {
            resolver::render_storage_error(stderr, "worktree set", &err);
            return 1;
        }
    }

    if !run_git_or_fail(
        runner,
        cwd,
        stderr,
        &["git", "config", "extensions.worktreeConfig", "true"],
        "tk worktree set: failed to enable worktreeConfig",
    ) {
        return 1;
    }
    if !run_git_or_fail(
        runner,
        cwd,
        stderr,
        &["git", "config", "--worktree", "tk.scope", &args.id],
        "tk worktree set: failed to write tk.scope",
    ) {
        return 1;
    }

    let _ = writeln!(stdout, "Set Workspace Scope to {}", args.id);
    0
}

fn run_clear(deps: Deps<'_>) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        cwd,
        ..
    } = deps;

    let out = match runner.run(&["git", "config", "--worktree", "--unset", "tk.scope"], cwd) {
        Ok(o) => o,
        Err(err) => {
            let _ = writeln!(stderr, "tk worktree clear: failed to invoke git\n{err}");
            return 1;
        }
    };
    // Exit 5 is git's "key already absent" code — idempotent success path.
    if out.exit_code != 0 && out.exit_code != 5 {
        let _ = writeln!(stderr, "tk worktree clear: failed");
        let git_stderr = String::from_utf8_lossy(&out.stderr);
        let trimmed = git_stderr.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(stderr, "{trimmed}");
        }
        return 1;
    }
    let _ = writeln!(stdout, "Cleared Workspace Scope");
    0
}

fn run_start(deps: Deps<'_>, args: StartArgs) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        ..
    } = deps;

    let mut store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, "worktree start", &err);
            return 1;
        }
    };

    let target = match lookup_start_target(&store, &args.id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            let _ = writeln!(
                stderr,
                "tk worktree start: '{}' is not a known Display ID or Alias",
                args.id
            );
            return 1;
        }
        Err(err) => {
            resolver::render_storage_error(stderr, "worktree start", &err);
            return 1;
        }
    };

    if target.status == ItemStatus::Done {
        let _ = writeln!(
            stderr,
            "tk worktree start: refusing to start a done {label}",
            label = target.item_class.label()
        );
        return 1;
    }

    let branch_slug = worktree_scope::sanitize(&target.title, 40);
    let path_slug = worktree_scope::sanitize(&target.title, 30);
    let branch = build_branch(&target.display_id, &branch_slug);
    let worktree_path = match args.path.as_deref() {
        Some(p) => p.to_owned(),
        None => match build_default_path(runner, cwd, stderr, &target.display_id, &path_slug) {
            Some(p) => p,
            None => return 1,
        },
    };

    if !run_git_or_fail(
        runner,
        cwd,
        stderr,
        &["git", "config", "extensions.worktreeConfig", "true"],
        "tk worktree start: failed to enable worktreeConfig",
    ) {
        return 1;
    }
    if !run_git_or_fail(
        runner,
        cwd,
        stderr,
        &["git", "worktree", "add", "-b", &branch, &worktree_path],
        "tk worktree start: failed to add worktree",
    ) {
        return 1;
    }
    if !run_git_or_fail(
        runner,
        cwd,
        stderr,
        &[
            "git",
            "-C",
            &worktree_path,
            "config",
            "--worktree",
            "tk.scope",
            &args.id,
        ],
        "tk worktree start: failed to write tk.scope",
    ) {
        return 1;
    }

    if !args.no_status {
        let outcome = set_status::set_item_status(
            &mut store,
            clock,
            SetStatusRequest {
                id: &target.id,
                status: ItemStatus::Active,
            },
        );
        // Marking active is best-effort: a successful transition and the
        // benign misses (item vanished, already Done) are all no-ops here;
        // only a genuine storage/mutation fault aborts the worktree command.
        match outcome {
            Ok(_) | Err(SetStatusError::NotFound | SetStatusError::LockedDone(_)) => {}
            Err(SetStatusError::Sqlite(err)) => {
                resolver::render_storage_error(stderr, "worktree start", &err);
                return 1;
            }
            Err(SetStatusError::Mutation(err)) => {
                let _ = writeln!(
                    stderr,
                    "tk worktree start: failed to append Mutation: {err}"
                );
                return 1;
            }
        }
    }

    let _ = writeln!(
        stdout,
        "Created worktree for {label}: {} - {}",
        target.display_id,
        target.title,
        label = target.item_class.label()
    );
    if !args.no_status {
        let _ = writeln!(stdout, "Status: active");
    }
    let _ = writeln!(stdout, "Branch: {branch}");
    let _ = writeln!(stdout, "Path:   {worktree_path}");
    0
}

struct StartTarget {
    id: String,
    display_id: String,
    title: String,
    item_class: ItemClass,
    status: ItemStatus,
}

fn lookup_start_target(
    store: &Store,
    display_arg: &str,
) -> Result<Option<StartTarget>, rusqlite::Error> {
    let result = store.conn().query_row(
        "select i.id, i.display_value, i.item_class, i.title, i.status \
           from item_ids ids \
           join items i on i.id = ids.item_id \
          where ids.value = ?1",
        rusqlite::params![display_arg],
        |row| {
            let id: String = row.get(0)?;
            let display_id: String = row.get(1)?;
            let class_text: String = row.get(2)?;
            let title: String = row.get(3)?;
            let status_text: String = row.get(4)?;
            Ok((id, display_id, class_text, title, status_text))
        },
    );
    match result {
        Ok((id, display_id, class_text, title, status_text)) => Ok(Some(StartTarget {
            id,
            display_id,
            title,
            item_class: crate::store::repository::item_class_from_text(&class_text),
            status: crate::store::repository::status_from_text(&status_text),
        })),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err),
    }
}

fn build_branch(display_id: &str, slug: &str) -> String {
    if slug.is_empty() {
        format!("tk/{display_id}")
    } else {
        format!("tk/{display_id}-{slug}")
    }
}

fn build_default_path<R: ProcRunner + ?Sized, W: Write + ?Sized>(
    runner: &R,
    cwd: &Path,
    stderr: &mut W,
    display_id: &str,
    slug: &str,
) -> Option<String> {
    match discovery::discover_paths(runner, cwd) {
        Ok(paths) => {
            let parent = paths.toplevel.parent().unwrap_or_else(|| Path::new("/"));
            let repo = paths
                .toplevel
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let id_sanitized = worktree_scope::sanitize(display_id, usize::MAX);
            let leaf = if slug.is_empty() {
                format!("{repo}.{id_sanitized}")
            } else {
                format!("{repo}.{id_sanitized}-{slug}")
            };
            Some(parent.join(&leaf).to_string_lossy().into_owned())
        }
        Err(err) => {
            discovery::render_failure(stderr, "worktree start", &err);
            None
        }
    }
}

fn run_git_or_fail<R: ProcRunner + ?Sized, W: Write + ?Sized>(
    runner: &R,
    cwd: &Path,
    stderr: &mut W,
    argv: &[&str],
    failure_msg: &str,
) -> bool {
    let result = match runner.run(argv, cwd) {
        Ok(out) => out,
        Err(ProcError::ExecutableNotFound) => {
            let _ = writeln!(stderr, "{failure_msg}\nExecutableNotFound");
            return false;
        }
        Err(ProcError::SpawnFailed) => {
            let _ = writeln!(stderr, "{failure_msg}\nSpawnFailed");
            return false;
        }
    };
    if result.exit_code != 0 {
        let _ = writeln!(stderr, "{failure_msg}");
        let git_stderr = String::from_utf8_lossy(&result.stderr);
        let trimmed = git_stderr.trim();
        if !trimmed.is_empty() {
            let _ = writeln!(stderr, "{trimmed}");
        }
        return false;
    }
    true
}
