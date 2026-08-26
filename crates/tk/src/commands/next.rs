//! `tk next` — select the next ready Ticket.
//!
//! Ranks ready Tickets by Effective Priority (lowest first), then own
//! Priority, then created_seq, within the active Scope (per ADR-0015).
//! Prints `<display-id>: <title>` to stdout by default; `-q`/`--quiet`
//! prints the bare Display ID instead, for `id="$(tk next -q)"` scripting.
//! When the pick's Effective Priority is lower than its own Priority, also
//! writes a rationale line to stderr (`<display>: Effective Priority <ep>
//! (via <contributor>)`) in both modes.
//!
//! Scope is the optional `<epic-id>` argument or `TK_SCOPE` (ADR-0022),
//! resolved Epic-only here before selection runs.

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::{resolver, scope};
use crate::domain::item_class::ItemClass;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::next::{
    self, NextError, NextOptions, NextScope, NextTicket, Rationale,
};

/// Flags for `tk next`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Restrict selection to this Epic's child Tickets. Falls back to the
    /// `TK_SCOPE` environment variable; absent both, all ready Tickets are
    /// considered.
    #[arg(value_name = "EPIC_ID")]
    pub epic: Option<String>,
    /// Print only the bare Display ID, omitting the title.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

/// Run `tk next [epic]`. On failure returns the [`CommandError`] for the
/// dispatch seam to frame as `tk next:` (ADR-0032). The Effective Priority
/// rationale is informational stderr written directly rather than framed as a
/// diagnostic, and goes out once a Ticket is selected — including when the
/// stdout write then fails.
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let scope_epic = scope::resolve(&store, args.epic.as_deref())?;

    let next_scope = match &scope_epic {
        None => NextScope::None,
        Some(epic) => NextScope::Epic(epic.id.as_str()),
    };

    match next::next_ready_ticket(&store, NextOptions { scope: next_scope }) {
        Ok(Some(ticket)) => {
            let written = if args.quiet {
                // `-q` is unstyled by contract (ADR-0015, tk-171 amendment):
                // it exists to be captured, so no Styler touches this path and
                // its bytes never vary with colour policy.
                writeln!(deps.stdout, "{}", ticket.display_id)
            } else {
                render_selection(deps.stdout, &ticket, deps.styler.for_stdout())
            };
            // The rationale is written before the selection's write error is
            // raised, for both failure kinds. On a broken pipe — which
            // `write_error` reports as success — returning first would cost
            // the explanation of a run that succeeded. On any other write
            // error the cost is inverted but far smaller: one rationale line
            // above a diagnostic on the same stream, naming a pick stdout
            // never delivered. Losing an explanation on success is worse than
            // a redundant line on a failure that already announces itself.
            if let Some(rationale) = ticket.rationale.as_ref() {
                render_rationale(deps.stderr, &ticket.display_id, rationale);
            }
            if let Err(err) = written {
                return cli::write_error(&err);
            }
            Ok(Exit::Ok)
        }
        Ok(None) => match &scope_epic {
            Some(epic) => Err(CommandError::failure(format!(
                "no ready Tickets in Epic {}",
                epic.display_id
            ))),
            None => Err(CommandError::failure("no ready Tickets")),
        },
        Err(NextError::Storage(err)) => Err(resolver::storage_error(&err)),
    }
}

/// Render the default `tk next` line: a styled Display ID, an unstyled
/// `: ` separator, then the sanitized title. `tk next` only ever selects
/// Tickets, so the Display ID always takes the Ticket [`palette::id_style`].
/// The title is user/Remote-controlled text at an output boundary, so it
/// runs through [`sanitize::write_sanitized_line`] rather than a raw
/// `write!`; it renders plain (not `palette::HEADER`) — this is a row, not
/// a detail header.
///
/// `show::render_sub_row` writes the same `<styled-id>: <sanitized-title>`
/// core and is the closest analogue, but it is not shared: it also carries a
/// caller-supplied glyph, a status glyph, a conditional `(Epic)` badge, and a
/// trailing `● P_`, none of which a selection line wants. `item_row` and
/// `item_header` were extracted for two callers emitting identical bytes;
/// this would parameterise four optionals for one caller that wants none.
fn render_selection<W: Write + ?Sized>(
    stdout: &mut W,
    ticket: &NextTicket,
    styler: SubStyler,
) -> std::io::Result<()> {
    write!(
        stdout,
        "{}: ",
        styler.wrap(palette::id_style(ItemClass::Ticket), &ticket.display_id)
    )?;
    sanitize::write_sanitized_line(stdout, ticket.title.as_bytes())?;
    stdout.write_all(b"\n")
}

fn render_rationale<W: Write + ?Sized>(stderr: &mut W, display_id: &str, r: &Rationale) {
    let _ = writeln!(
        stderr,
        "{display_id}: Effective Priority {ep} (via {blocked})",
        ep = r.effective_priority,
        blocked = r.blocked_display_id,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::commands::testing::{Harness, cwd, seed_store};
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::testing::{FixtureItem, TmpStore, insert_dependency, insert_fixture_item};
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;
    use std::path::Path;

    /// Seed an accepted Ticket whose title is its `id`, so a rendered
    /// `<display-id>: <title>` line reads `<display>: <id>`. Use
    /// [`seed_ticket_titled`] when the title itself is under assertion.
    fn seed_ticket(conn: &Connection, id: &str, display: &str, priority: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: Some(priority),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// [`seed_ticket`] with an explicit title distinct from `id`/`display`,
    /// for tests asserting the rendered title rather than only the Display ID.
    fn seed_ticket_titled(
        conn: &Connection,
        id: &str,
        display: &str,
        title: &str,
        priority: &str,
        created_seq: i64,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title,
                priority: Some(priority),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// Stage the single git call store-open makes (`git rev-parse` for
    /// repository discovery). Scope no longer reads git state (ADR-0022).
    fn expect_open(h: &Harness<'_>, store: &TmpStore) {
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
    }

    /// Drive `run` and frame any returned error as the dispatch seam does
    /// (ADR-0032: `tk next: <body>`). A success passes its `Exit` through; the
    /// rationale line `run` writes to stderr on success is preserved.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        run_rendered_with(h, Styler::plain(), args)
    }

    /// [`run_rendered`] with an explicit `Styler` so the colour-output test
    /// can exercise `Styler::always()`.
    fn run_rendered_with(h: &mut Harness<'_>, styler: Styler, args: Args) -> Exit {
        let mut deps = h.deps_with(styler);
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "next");
                exit
            }
        }
    }

    fn seed_epic(conn: &Connection, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: id,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    /// [`seed_ticket`] for a child of `epic`; the title is likewise the `id`.
    fn seed_child(
        conn: &Connection,
        id: &str,
        display: &str,
        priority: &str,
        epic: &str,
        created_seq: i64,
    ) {
        insert_fixture_item(
            conn,
            FixtureItem {
                id,
                display,
                title: id,
                priority: Some(priority),
                container_id: Some(epic),
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    fn args(epic: Option<&str>) -> Args {
        Args {
            epic: epic.map(str::to_owned),
            quiet: false,
        }
    }

    #[test]
    fn empty_store_reports_no_ready_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(None));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk next: no ready Tickets"));
        assert!(!stderr.contains("Epic"));
    }

    #[test]
    fn prints_highest_priority_ready_ticket_to_stdout() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "low", "tk-1", "P3", 1);
        seed_ticket(&conn, "high", "tk-2", "P0", 2);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout.trim(), "tk-2: high");
    }

    #[test]
    fn rationale_lands_on_stderr_when_effective_priority_promotes() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "blocker", "tk-1", "P3", 1);
        seed_ticket(&conn, "blocked-high", "tk-2", "P0", 2);
        insert_dependency(&conn, "blocker", "blocked-high").unwrap();
        seed_ticket(&conn, "ready", "tk-3", "P1", 3);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert_eq!(stdout.trim(), "tk-1: blocker");
        assert!(
            stderr.contains("tk-1: Effective Priority P0 (via tk-2)"),
            "stderr={stderr:?}"
        );
    }

    /// The rationale still lands on stderr under `-q`, unchanged from the
    /// default mode: `-q` only replaces the stdout selection line.
    #[test]
    fn rationale_lands_on_stderr_under_quiet_too() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "blocker", "tk-1", "P3", 1);
        seed_ticket(&conn, "blocked-high", "tk-2", "P0", 2);
        insert_dependency(&conn, "blocker", "blocked-high").unwrap();
        seed_ticket(&conn, "ready", "tk-3", "P1", 3);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                quiet: true,
                ..args(None)
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert_eq!(stdout.trim(), "tk-1");
        assert!(
            stderr.contains("tk-1: Effective Priority P0 (via tk-2)"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn default_prints_display_id_and_title() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "ready", "tk-2", "Design comments schema", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "tk-2: Design comments schema\n");
    }

    #[test]
    fn quiet_prints_the_bare_display_id_and_nothing_else() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "ready", "tk-2", "Design comments schema", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                quiet: true,
                ..args(None)
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "tk-2\n");
    }

    /// The guard that matters: under a colour-forcing `Styler`, `-q` must stay
    /// the bare Display ID line ADR-0015's tk-171 amendment fixes as unstyled
    /// — no SGR escape — while the default line does style its ID. A
    /// `Styler::plain()` assertion alone would be trivially true and would
    /// not catch `-q` routing through the Styler (see `render_selection`).
    #[test]
    fn quiet_bypasses_the_styler_even_when_colour_is_forced() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "ready", "tk-2", "Design comments schema", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered_with(
            &mut h,
            Styler::always(),
            Args {
                quiet: true,
                ..args(None)
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "tk-2\n");
        assert!(
            !stdout.contains('\u{1b}'),
            "-q must never carry an SGR escape: stdout={stdout:?}"
        );
    }

    #[test]
    fn default_opens_the_cyan_id_span_when_colour_is_forced() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "ready", "tk-2", "Design comments schema", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered_with(&mut h, Styler::always(), args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(
            stdout, "\u{1b}[36mtk-2\u{1b}[39m: Design comments schema\n",
            "the cyan Ticket-ID span must close (39) before the separator so the \
             title renders plain (ADR-0015 tk-171 amendment; ADR-0014 \
             disjoint-family close)"
        );
    }

    /// A title carrying a control byte is sanitized at the output boundary
    /// (`sanitize::write_sanitized_line`), matching `tk list` / `tk show`.
    #[test]
    fn default_sanitizes_a_title_control_byte() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "ready", "tk-2", "bell\x07ring", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(None));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "tk-2: bell\\x07ring\n");
    }

    #[test]
    fn empty_case_is_unchanged_under_quiet() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                quiet: true,
                ..args(None)
            },
        );
        assert_eq!(code, Exit::Failure);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.is_empty(), "stdout={stdout:?}");
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk next: no ready Tickets"));
    }

    /// A stdout writer that always fails with a chosen [`std::io::ErrorKind`],
    /// standing in for a pipe closed by `| head` (`BrokenPipe`) or a full disk
    /// on a redirect (`StorageFull`).
    struct FailingWriter(std::io::ErrorKind);
    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.0, "write failed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Drive `run` against a stdout that fails with `kind`, returning the
    /// framed exit and whatever reached stderr.
    fn run_with_failing_stdout(
        store: &TmpStore,
        cwd_path: &Path,
        kind: std::io::ErrorKind,
        quiet: bool,
    ) -> (Exit, String) {
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
        let mut stdout = FailingWriter(kind);
        let mut stderr: Vec<u8> = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut deps = Deps {
            stdout: &mut stdout,
            stderr: &mut stderr,
            stdin: &mut stdin,
            runner: &runner,
            clock: &clock,
            rng: &mut rng,
            cwd: cwd_path,
            styler: Styler::plain(),
        };
        let exit = match run(&mut deps, Args { epic: None, quiet }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "next");
                exit
            }
        };
        (exit, String::from_utf8(stderr).unwrap())
    }

    /// A full disk on `tk next > file` must not report success having written
    /// nothing — the shared `cli::write_error` policy turns it into
    /// `Exit::Failure` with a diagnostic, as the sibling read commands do.
    #[test]
    fn non_broken_pipe_write_error_fails_with_a_stderr_diagnostic() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        // Seeded so a rationale is owed: stderr then carries both the
        // rationale and the diagnostic, in that order, which is the contract
        // `run` documents for a non-broken-pipe write failure.
        seed_ticket_titled(&conn, "blocker", "tk-1", "Unblock the P0", "P3", 1);
        seed_ticket_titled(&conn, "blocked-high", "tk-2", "The P0", "P0", 2);
        insert_dependency(&conn, "blocker", "blocked-high").unwrap();
        drop(conn);

        let cwd_path = cwd();
        for quiet in [false, true] {
            let (exit, stderr) =
                run_with_failing_stdout(&store, &cwd_path, std::io::ErrorKind::StorageFull, quiet);
            assert_eq!(exit, Exit::Failure, "quiet={quiet}");
            let rationale = "tk-1: Effective Priority P0 (via tk-2)";
            let diagnostic = "tk next: failed to write output";
            assert!(
                stderr.contains(diagnostic),
                "quiet={quiet}, stderr={stderr:?}"
            );
            assert!(
                stderr.find(rationale) < stderr.find(diagnostic),
                "the rationale precedes the diagnostic: quiet={quiet}, stderr={stderr:?}"
            );
        }
    }

    /// `tk next | head` closes stdout early. That is success (ADR: a consumer
    /// that stopped reading is not a `tk next` failure), and the Effective
    /// Priority rationale must still reach stderr — it is written before the
    /// selection's write error is raised, so a broken pipe cannot cost the
    /// explanation of a non-obvious pick.
    #[test]
    fn broken_pipe_is_success_and_still_writes_the_rationale() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket_titled(&conn, "blocker", "tk-1", "Unblock the P0", "P3", 1);
        seed_ticket_titled(&conn, "blocked-high", "tk-2", "The P0", "P0", 2);
        insert_dependency(&conn, "blocker", "blocked-high").unwrap();
        drop(conn);

        let cwd_path = cwd();
        for quiet in [false, true] {
            let (exit, stderr) =
                run_with_failing_stdout(&store, &cwd_path, std::io::ErrorKind::BrokenPipe, quiet);
            assert_eq!(exit, Exit::Ok, "quiet={quiet}");
            assert!(
                stderr.contains("tk-1: Effective Priority P0 (via tk-2)"),
                "quiet={quiet}, stderr={stderr:?}"
            );
        }
    }

    #[test]
    fn epic_scope_selects_a_child_over_a_higher_priority_outsider() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "child", "tk-2", "P2", "epic", 2);
        seed_ticket(&conn, "outside", "tk-3", "P0", 3);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(Some("tk-1")));
        assert_eq!(code, Exit::Ok);
        // tk-3 outranks tk-2 globally, but the Epic Scope confines selection.
        assert_eq!(String::from_utf8(h.stdout).unwrap().trim(), "tk-2: child");
    }

    #[test]
    fn epic_scope_with_no_ready_child_names_the_epic_in_the_empty_message() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_epic(&conn, "epic", "tk-1", 1);
        seed_child(&conn, "blocked-child", "tk-2", "P0", "epic", 2);
        seed_ticket(&conn, "blocker", "tk-3", "P0", 3);
        insert_dependency(&conn, "blocker", "blocked-child").unwrap();
        // A ready Ticket outside the Epic must not rescue the empty result.
        seed_ticket(&conn, "outside", "tk-4", "P0", 4);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(Some("tk-1")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: no ready Tickets in Epic tk-1"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn unknown_scope_value_renders_typed_error() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(Some("vanished")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: scope 'vanished' is not a known Display ID or Alias"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn ticket_scope_is_rejected_as_not_an_epic() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        seed_ticket(&conn, "t1", "tk-1", "P2", 1);
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_open(&h, &store);
        let code = run_rendered(&mut h, args(Some("tk-1")));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk next: scope 'tk-1' is not an Epic"),
            "stderr={stderr:?}"
        );
    }
}
