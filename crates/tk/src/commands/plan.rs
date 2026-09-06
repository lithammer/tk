//! Dedicated Plan editing and progress view (ADR-0050).
//!
//! The view ignores Epic Scope and always counts the whole Plan. Its sections
//! are independent of List Tree filters; all dynamic text is sanitized here.

use std::io::Write;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::selection_state::SelectionState;
use crate::domain::status::ItemStatus;
use crate::render::{palette, sanitize, styler::SubStyler};
use crate::store::repository::plan::{
    self, MembershipEdit, PlanBlocker, PlanError, PlanSection, PlanTicket,
};

/// No subcommand displays the complete Plan, including done members.
#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Plan commands change membership only, never Ticket state.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Include Tickets in the Plan; validate the whole batch before editing.
    Add {
        /// Ticket Display IDs or Aliases.
        #[arg(required = true, num_args = 1.., value_name = "ID")]
        ids: Vec<String>,
    },
    /// Remove Tickets from the Plan without changing their state.
    Remove {
        /// Ticket Display IDs or Aliases.
        #[arg(required = true, num_args = 1.., value_name = "ID")]
        ids: Vec<String>,
    },
    /// Remove all membership, including unfinished Tickets; keep the Tickets.
    Clear,
}

/// Execute a local Plan operation without opening a Backend Adapter.
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let result = match args.command {
        Some(Command::Add { ids }) => edit(deps.stdout, &mut store, &ids, MembershipEdit::Add)?,
        Some(Command::Remove { ids }) => {
            edit(deps.stdout, &mut store, &ids, MembershipEdit::Remove)?
        }
        Some(Command::Clear) => {
            let count = plan::clear(&mut store).map_err(plan_error)?;
            writeln!(deps.stdout, "Cleared Plan ({count} removed)")
        }
        None => {
            let tickets = plan::read(&store).map_err(plan_error)?;
            render(deps.stdout, &tickets, deps.styler.for_stdout())
        }
    };
    match result {
        Ok(()) => Ok(Exit::Ok),
        Err(err) => cli::write_error(&err),
    }
}

fn edit(
    out: &mut dyn Write,
    store: &mut crate::store::repository::Store,
    ids: &[String],
    edit: MembershipEdit,
) -> Result<std::io::Result<()>, CommandError> {
    let results = plan::edit_membership(store, ids, edit).map_err(plan_error)?;
    Ok((|| {
        for result in results {
            let label = match (edit, result.changed) {
                (MembershipEdit::Add, true) => "Added to Plan",
                (MembershipEdit::Add, false) => "Already in Plan",
                (MembershipEdit::Remove, true) => "Removed from Plan",
                (MembershipEdit::Remove, false) => "Not in Plan",
            };
            writeln!(out, "{label}: {}", result.display_id)?;
        }
        Ok(())
    })())
}

/// Preserve the Repository Store busy diagnostic for every Plan operation.
fn plan_error(err: PlanError) -> CommandError {
    match err {
        PlanError::Storage(err) => resolver::storage_error(&err),
        err => CommandError::failure(err),
    }
}

fn render(out: &mut dyn Write, tickets: &[PlanTicket], styler: SubStyler) -> std::io::Result<()> {
    if tickets.is_empty() {
        writeln!(out, "No Tickets in Plan.\n")?;
    }
    for (section, heading) in [
        (PlanSection::Ready, "Ready"),
        (PlanSection::InProgress, "In progress"),
        (PlanSection::Waiting, "Waiting"),
        (PlanSection::Done, "Done"),
    ] {
        let members: Vec<_> = tickets.iter().filter(|t| t.section() == section).collect();
        if members.is_empty() {
            continue;
        }
        writeln!(out, "{heading}")?;
        for ticket in members {
            write!(
                out,
                "  {} {}",
                styler.wrap(palette::status_style(ticket.status), ticket.status.glyph()),
                ticket.display_id
            )?;
            if let Some(priority) = ticket.priority {
                write!(out, " {priority}")?;
            }
            write!(out, " ")?;
            sanitize::write_sanitized_line(out, ticket.title.as_bytes())?;
            if ticket.status != ItemStatus::Done && ticket.selection != SelectionState::Accepted {
                write!(out, " [{}]", ticket.selection)?;
            }
            if ticket.status != ItemStatus::Done {
                for blocker in &ticket.blockers {
                    match blocker {
                        PlanBlocker::Dependency {
                            display_id,
                            outside_plan,
                        } => {
                            write!(out, " [blocked by {display_id}")?;
                            if *outside_plan {
                                write!(out, " (outside Plan)")?;
                            }
                            write!(out, "]")?;
                        }
                        PlanBlocker::External { reason } => {
                            write!(out, " [external blocker: ")?;
                            sanitize::write_sanitized_line(out, reason.as_bytes())?;
                            write!(out, "]")?;
                        }
                    }
                }
            }
            writeln!(out)?;
        }
        writeln!(out)?;
    }
    let done = tickets
        .iter()
        .filter(|t| t.status == ItemStatus::Done)
        .count();
    writeln!(
        out,
        "{} remaining · {done}/{} done",
        tickets.len() - done,
        tickets.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::store::testing::{
        FixtureItem, TmpStore, insert_external_blocker, insert_fixture_item,
    };

    #[test]
    fn plan_storage_contention_keeps_the_retry_diagnostic() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let err = plan_error(PlanError::Storage(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            )));
            let CommandError::Failure { body, tail } = err else {
                panic!("Store contention must be an operation failure");
            };
            assert_eq!(body, "Repository Store is busy; retry the command");
            assert!(tail.is_none());
        }
    }

    #[test]
    fn plan_sanitizes_titles_and_external_reasons_without_backend_reads() {
        let tmp = TmpStore::new("tk");
        let conn = seed_store(&tmp);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "ticket",
                display: "tk-1",
                title: "Title\x1b[31m\nnext",
                created_seq: 1,
                selection_state: Some("parked"),
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_external_blocker(&conn, "external", "ticket", None).unwrap();
        conn.execute(
            "update external_blockers set reason = ?1",
            ["Await\x07\nreview"],
        )
        .unwrap();
        let mut store = crate::store::repository::Store::for_test(conn);
        plan::edit_membership(&mut store, &["tk-1".into()], MembershipEdit::Add).unwrap();
        let path = cwd();
        let mut h = Harness::new(&path);
        expect_git(&h, &tmp);
        run(&mut h.deps(), Args { command: None }).unwrap();
        insta::assert_snapshot!(h.out(), @r"
        Waiting
          ○ tk-1 P2 Title\x1b[31m next [parked] [external blocker: Await\x07 review]

        1 remaining · 0/1 done
        ");
        assert!(h.err().is_empty());
    }
}
