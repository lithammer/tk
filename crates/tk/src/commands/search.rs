//! `tk search` — find Tickets and Epics by a title substring (ADR-0025).
//!
//! A flat, whole-store lookup: every item whose title contains the query as a
//! case-insensitive literal substring, across every Item Status. Unlike
//! `tk list` it applies no view, Origin, class, or Scope filter and never
//! nests a List Tree — matches render flat, reusing the shared row + chrome
//! renderer so search and list output can never drift.

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{self, Deps, Exit};
use crate::commands::item_row::{render_chrome, render_row};
use crate::commands::resolver;
use crate::render::styler::SubStyler;
use crate::store::repository::list::ListRow;
use crate::store::repository::search;

const COMMAND: &str = "search";

/// Flags for `tk search`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Title substring to search for (case-insensitive, matched literally).
    #[arg(value_name = "QUERY")]
    pub query: String,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> Exit {
    let Deps {
        stdout,
        stderr,
        runner,
        clock,
        cwd,
        styler,
        ..
    } = deps;

    // An all-whitespace query is rejected before any store work: a literal
    // substring of only spaces matches nothing meaningful, and `instr` against
    // the empty string would otherwise match every row.
    if args.query.trim().is_empty() {
        let _ = writeln!(stderr, "tk search: query must not be empty");
        return Exit::Failure;
    }

    let store = match resolver::open_for_command(runner, cwd, clock) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let rows = match search::search_rows(&store, &args.query) {
        Ok(rows) => rows,
        Err(err) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let out = styler.for_stdout();
    if let Err(err) = render(stdout, &rows, &args.query, out) {
        return cli::exit_for_write_error(&err, stderr, COMMAND);
    }
    Exit::Ok
}

/// Render matches flat (no List Tree nesting) followed by the shared summary
/// chrome, or a no-match line when nothing matched. The query is echoed
/// verbatim in the no-match message.
fn render<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    query: &str,
    styler: SubStyler,
) -> std::io::Result<()> {
    if rows.is_empty() {
        return writeln!(stdout, "No items match \"{query}\".");
    }

    for row in rows {
        render_row(stdout, row, "", styler)?;
    }
    render_chrome(stdout, rows, styler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, TmpStore, insert_dependency, insert_external_blocker, insert_fixture_item,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;
    use std::path::Path;

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    fn seed_store(store: &TmpStore) -> Connection {
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into store_config(key, value) values ('display_prefix', 'tk')",
            [],
        )
        .unwrap();
        conn
    }

    struct Harness<'a> {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdin: std::io::Cursor<Vec<u8>>,
        runner: FakeRunner,
        clock: FakeClock,
        rng: StdRng,
        cwd: &'a Path,
    }

    impl<'a> Harness<'a> {
        fn new(cwd: &'a Path) -> Self {
            Self {
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdin: std::io::Cursor::new(Vec::new()),
                runner: FakeRunner::new(),
                clock: FakeClock::new(1_778_284_800_000),
                rng: StdRng::seed_from_u64(0),
                cwd,
            }
        }
        fn deps(&mut self) -> Deps<'_> {
            Deps {
                stdout: &mut self.stdout,
                stderr: &mut self.stderr,
                stdin: &mut self.stdin,
                runner: &self.runner,
                clock: &self.clock,
                rng: &mut self.rng,
                cwd: self.cwd,
                styler: Styler::plain(),
            }
        }
    }

    fn expect_git(h: &Harness<'_>, store: &TmpStore) {
        h.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
    }

    #[test]
    fn renders_a_title_match_with_chrome() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Fix the flaky test",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t2",
                display: "tk-2",
                title: "Unrelated chore",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                query: "flaky".to_owned(),
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("tk-1"), "stdout={stdout:?}");
        assert!(!stdout.contains("tk-2"), "stdout={stdout:?}");
        assert!(
            stdout.contains("Total: 1 item (1 open)"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("Status:"), "stdout={stdout:?}");
    }

    #[test]
    fn no_match_prints_message_at_exit_ok() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Auth rework",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                query: "nonexistent".to_owned(),
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(
            String::from_utf8(h.stdout).unwrap(),
            "No items match \"nonexistent\".\n"
        );
    }

    #[test]
    fn done_match_renders_without_the_blocked_indicator() {
        // The render gate (ADR-0025): an open blocked Ticket shows `⊘`, but a
        // `done` Ticket carrying an unresolved blocker must not — closing
        // resolved nothing, yet a finished item shown as blocked reads as
        // nonsense. This would regress if the gate were removed from
        // `item_row::render_row`.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "open",
                display: "tk-1",
                title: "Auth open",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "tk-2",
                title: "Auth blocker",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "open").unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "done",
                display: "tk-3",
                title: "Auth done",
                status: "done",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_external_blocker(&conn, "eb", "done", None).unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                query: "auth".to_owned(),
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let line_of = |id: &str| {
            stdout
                .lines()
                .find(|l| l.contains(id))
                .unwrap_or_else(|| panic!("no row for {id} in {stdout:?}"))
                .to_owned()
        };
        assert!(
            line_of("tk-1").contains('\u{2298}'),
            "open blocked row should show ⊘: {stdout:?}"
        );
        assert!(
            !line_of("tk-3").contains('\u{2298}'),
            "done row must not show ⊘: {stdout:?}"
        );
    }

    #[test]
    fn empty_query_is_rejected_before_opening_the_store() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        // No git expectation: an all-whitespace query is rejected before discovery.
        let code = run(
            h.deps(),
            Args {
                query: "   ".to_owned(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk search: query must not be empty"),
            "stderr={stderr:?}"
        );
        assert!(h.stdout.is_empty(), "stdout should be empty");
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                query: "auth".to_owned(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk search: Repository Store not initialized; run 'tk init'"),
            "stderr={stderr:?}"
        );
    }
}
