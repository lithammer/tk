//! Project-aware agent briefing (ADR-0052), silent on Store-open errors (ADR-0020).

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::{item_row, next as next_command, plan as plan_command, resolver, scope};
use crate::domain::mutation_state::MutationState;
use crate::domain::status::ItemStatus;
use crate::render::{sanitize, styler::SubStyler};
use crate::store::repository::{ResolvedItemRef, Store, list, next, plan};
use crate::store::sync;

/// Core workflows remain available even when the Store has no work yet.
const PRIME_RAW: &str = include_str!("prime.md");
/// Finishing and Remote guidance requires a configured Remote.
const REMOTE_RAW: &str = include_str!("prime-remote.md");

/// Prime takes no arguments; current Scope comes from `TK_SCOPE`.
#[derive(Debug, ClapArgs)]
pub struct Args {}

/// Facts read together before any output reaches the agent (ADR-0052).
struct Briefing {
    work: CurrentWork,
    plan: Vec<plan::PlanTicket>,
    mutations: Option<Vec<(MutationState, i64)>>,
}

/// An invalid Scope must not masquerade as an empty or unscoped selection.
enum CurrentWork {
    Available {
        scope: Option<ResolvedItemRef>,
        next: Option<next::NextTicket>,
        active: Vec<list::ListRow>,
    },
    InvalidScope(String),
}

/// Open errors stay silent for hooks; later read errors leave stdout empty.
pub fn run(deps: &mut Deps<'_>, _args: Args) -> Result<Exit, CommandError> {
    let Ok(store) = resolver::open_for_command(deps.runner, deps.cwd, deps.clock) else {
        return Ok(Exit::Ok);
    };
    let briefing = Briefing::read(&store)?;
    let mut output = Vec::new();
    if let Err(err) = briefing.render(&mut output, deps.styler.for_stdout()) {
        return cli::write_error(&err);
    }
    if let Err(err) = deps.stdout.write_all(&output) {
        return cli::write_error(&err);
    }
    Ok(Exit::Ok)
}

impl Briefing {
    /// All readers borrow the Store connection held by this read transaction.
    fn read(store: &Store) -> Result<Self, CommandError> {
        let tx = store
            .conn()
            .unchecked_transaction()
            .map_err(|e| resolver::storage_error(&e))?;
        let plan = plan::read_snapshot(&tx).map_err(|e| resolver::storage_error(&e))?;
        let work = match scope::resolve(store, None) {
            Ok(scope) => {
                let epic_id = scope.as_ref().map(|epic| epic.id.as_str());
                let selection = match (plan.is_empty(), epic_id) {
                    (false, epic) => next::NextScope::Plan(epic),
                    (true, Some(epic)) => next::NextScope::Epic(epic),
                    (true, None) => next::NextScope::None,
                };
                let next = next::next_ready_ticket(store, next::NextOptions { scope: selection })
                    .map_err(|next::NextError::Storage(e)| resolver::storage_error(&e))?;
                let active = list::list_rows(
                    store,
                    list::ListOptions {
                        view: list::ListView::Active,
                        scope: epic_id,
                        ..Default::default()
                    },
                )
                .map_err(|e| resolver::storage_error(&e))?
                .into_iter()
                // List Tree reads also include idle parent Epics for matching children.
                .filter(|row| row.status == ItemStatus::Active)
                .collect();
                CurrentWork::Available {
                    scope,
                    next,
                    active,
                }
            }
            Err(scope::ScopeError::Storage(err)) => return Err(resolver::storage_error(&err)),
            Err(err) => CurrentWork::InvalidScope(err.to_string()),
        };
        let mutations = if sync::configured_remote_kind(&tx)
            .map_err(|e| resolver::storage_error(&e))?
            .is_some()
        {
            Some(sync::mutation_state_counts(&tx).map_err(|e| resolver::storage_error(&e))?)
        } else {
            None
        };
        Ok(Self {
            work,
            plan,
            mutations,
        })
    }

    fn render(&self, out: &mut dyn Write, styler: SubStyler) -> std::io::Result<()> {
        writeln!(out, "# tk Workflow Context\n\n## Current Work\n")?;
        self.work.render(out, !self.plan.is_empty(), styler)?;
        if self.plan.is_empty() {
            writeln!(out, "\nPlan: empty")?;
        } else {
            writeln!(out, "\nPlan (whole Store):")?;
            plan_command::render(out, &self.plan, styler)?;
        }
        if let Some(counts) = &self.mutations {
            if counts.is_empty() {
                writeln!(out, "\nMutation Log: clean")?;
            } else {
                write!(out, "\nMutation Log: ")?;
                for (index, state) in MutationState::ALL
                    .iter()
                    .filter(|state| **state != MutationState::Applied)
                    .enumerate()
                {
                    if index != 0 {
                        write!(out, " · ")?;
                    }
                    let count = counts
                        .iter()
                        .find(|(s, _)| s == state)
                        .map_or(0, |(_, count)| *count);
                    write!(out, "{count} {}", state.text())?;
                }
                writeln!(out)?;
            }
        }
        writeln!(out, "\n{}", PRIME_RAW.trim_end())?;
        if self.mutations.is_some() {
            writeln!(out, "\n{}", REMOTE_RAW.trim_end())?;
        }
        writeln!(
            out,
            "\nThis briefing is contextual, not complete. Use `tk --help`, `tk <command> --help`, or `man tk` for the full command reference."
        )
    }
}

impl CurrentWork {
    fn render(&self, out: &mut dyn Write, in_plan: bool, styler: SubStyler) -> std::io::Result<()> {
        let (scope, next, active) = match self {
            Self::Available {
                scope,
                next,
                active,
            } => (scope, next, active),
            Self::InvalidScope(warning) => {
                write!(out, "Warning: ")?;
                sanitize::write_sanitized_line(out, warning.as_bytes())?;
                writeln!(out)?;
                return Ok(());
            }
        };
        if let Some(epic) = scope {
            writeln!(out, "Scope: {} (Epic + child Tickets)\n", epic.display_id)?;
        }
        write!(out, "Next")?;
        if in_plan {
            write!(out, " in Plan")?;
        }
        if let Some(epic) = scope {
            write!(out, " within Scope {}", epic.display_id)?;
        }
        write!(out, ": ")?;
        if let Some(ticket) = next {
            next_command::render_selection(out, ticket, styler)?;
            writeln!(out, "Start: `tk start {}`", ticket.display_id)?;
        } else {
            writeln!(out, "no ready Tickets")?;
        }
        if active.is_empty() {
            writeln!(out, "\nActive: none")?;
        } else {
            writeln!(out, "\nActive (Store context):")?;
            for row in active {
                item_row::render_row(out, row, "  ", styler)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::TmpStore;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;

    #[test]
    fn prime_against_a_behind_version_store_migrates_and_prints_the_briefing() {
        // ADR-0020 preserves migration on open, including for global hooks.
        let store = TmpStore::new("repo");
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_through(&mut conn, 2, "2026-05-09T00:00:00.000Z").unwrap();
        drop(conn);

        let cwd_path = std::env::current_dir().unwrap();
        let runner = FakeRunner::new();
        runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = StdRng::seed_from_u64(0);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut deps = Deps {
            stdout: &mut stdout,
            stderr: &mut stderr,
            stdin: &mut stdin,
            runner: &runner,
            clock: &clock,
            rng: &mut rng,
            cwd: &cwd_path,
            styler: Styler::plain(),
        };

        assert_eq!(run(&mut deps, Args {}).unwrap(), Exit::Ok);
        assert!(
            String::from_utf8(stdout)
                .unwrap()
                .starts_with("# tk Workflow Context")
        );

        let conn = Connection::open(store.db_path()).unwrap();
        assert_eq!(
            migrations::current_version(&conn).unwrap(),
            i64::from(migrations::MAX_KNOWN_VERSION)
        );
    }
}
