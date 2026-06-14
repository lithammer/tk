//! `tk update` — update title, body, priority, or parent of a Ticket or
//! Epic.
//!
//! All field flags are individually optional; at least one must be set.
//! `-m` / `-F` are mutually exclusive (they specify the title+body
//! message); `-P <epic>` and `--no-parent` are mutually exclusive
//! (they specify the parent operation). Epics reject `--priority`,
//! `-P`, and `--no-parent` since they have no Priority and no parent
//! column.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::message::{self, Input as MessageInput};
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::store::repository::update::{self, ParentOp, UpdateRequest};

/// Flags for `tk update`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to update.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Message paragraph; repeatable. Conflicts with -F.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MESSAGE",
        conflicts_with = "file"
    )]
    pub message: Vec<String>,
    /// Read the message from a file, or '-' for stdin.
    #[arg(short = 'F', long = "file", value_name = "PATH")]
    pub file: Option<String>,
    /// Set Priority (P0..P4). Tickets only.
    #[arg(short = 'p', long, value_name = "PRIORITY")]
    pub priority: Option<Priority>,
    /// Set the containing Epic by Display ID or Alias. Tickets only.
    #[arg(short = 'P', long, value_name = "EPIC", conflicts_with = "no_parent")]
    pub parent: Option<String>,
    /// Remove the Ticket from its current Epic. Tickets only.
    #[arg(long = "no-parent")]
    pub no_parent: bool,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let has_message_input = !args.message.is_empty() || args.file.is_some();
    let has_parent_op = args.parent.is_some() || args.no_parent;

    if !has_message_input && args.priority.is_none() && !has_parent_op {
        return Err(CommandError::usage(
            "no changes requested; supply at least one of \
             -m / -F / -p / -P / --no-parent",
        ));
    }

    let parsed_msg = if has_message_input {
        let input = if args.message.is_empty() {
            MessageInput::File(args.file.as_deref().expect("has_message_input flag holds"))
        } else {
            MessageInput::Paragraphs(&args.message)
        };
        Some(message::read_input(input, deps.cwd, deps.stdin).map_err(|err| message_error(&err))?)
    } else {
        None
    };

    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let resolved = match resolver::resolve(&store, &args.id) {
        Ok(r) => r,
        Err(resolver::ResolveError::NotFound) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias",
                id = args.id
            )));
        }
        Err(resolver::ResolveError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };

    if resolved.item_class == ItemClass::Epic {
        if args.priority.is_some() {
            return Err(CommandError::usage("--priority cannot be set on an Epic"));
        }
        if has_parent_op {
            return Err(CommandError::usage(
                "--parent / --no-parent cannot be set on an Epic",
            ));
        }
    }

    let resolved_parent = if let Some(arg) = args.parent.as_deref() {
        match resolver::resolve_epic(&store, arg) {
            Ok(epic) => Some(epic),
            Err(resolver::ResolveEpicError::NotFound) => {
                return Err(CommandError::failure(format!(
                    "parent '{arg}' is not a known Display ID or Alias"
                )));
            }
            Err(resolver::ResolveEpicError::NotAnEpic) => {
                return Err(CommandError::failure(format!(
                    "parent '{arg}' is not an Epic"
                )));
            }
            Err(resolver::ResolveEpicError::Storage(err)) => {
                return Err(resolver::storage_error(&err));
            }
        }
    } else {
        None
    };

    let parent_op = match resolved_parent.as_ref() {
        Some(epic) => ParentOp::Set(&epic.id),
        None if args.no_parent => ParentOp::Clear,
        None => ParentOp::Unchanged,
    };

    let req = UpdateRequest {
        id: &resolved.id,
        item_class: resolved.item_class,
        title: parsed_msg.as_ref().map(|m| m.title.as_str()),
        body: parsed_msg.as_ref().map(|m| m.body.as_str()),
        priority: args.priority,
        parent: parent_op,
    };

    match update::update_item(&mut store, deps.clock, req) {
        Ok(updated) => {
            let label = match updated.item_class {
                ItemClass::Ticket => "Updated Ticket",
                ItemClass::Epic => "Updated Epic",
            };
            let _ = writeln!(
                deps.stdout,
                "{label}: {} - {}",
                updated.display_id, updated.title
            );
            Ok(Exit::Ok)
        }
        Err(update::UpdateError::NotFound) => Err(CommandError::failure(format!(
            "'{id}' is not a known Display ID or Alias",
            id = args.id
        ))),
        Err(update::UpdateError::PriorityOnTriage) => Err(CommandError::failure(format!(
            "'{id}' is in triage; set a Priority by accepting it with \
             'tk accept {id} --priority Pn'",
            id = args.id
        ))),
        Err(update::UpdateError::Sqlite(err)) => Err(resolver::storage_error(&err)),
        Err(update::UpdateError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}

fn message_error(err: &message::ReadError) -> CommandError {
    match err {
        message::ReadError::Parse(message::ParseError::Empty) => {
            CommandError::failure("message is empty")
        }
        message::ReadError::Parse(message::ParseError::NulByte) => {
            CommandError::failure("message contains a NUL byte")
        }
        message::ReadError::File { path, source } => {
            CommandError::failure(format!("failed to read '{path}': {source}"))
        }
        message::ReadError::Stdin(source) => {
            CommandError::failure(format!("failed to read message from stdin: {source}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, TmpStore, insert_fixture_item};
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

    /// Drive `run` and frame any returned error as the dispatch seam does
    /// (ADR-0032: `tk update: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "update");
                exit
            }
        }
    }

    fn args(id: &str) -> Args {
        Args {
            id: id.to_owned(),
            message: Vec::new(),
            file: None,
            priority: None,
            parent: None,
            no_parent: false,
        }
    }

    #[test]
    fn no_change_request_exits_2_with_usage_hint() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let code = run_rendered(&mut h, args("tk-1"));
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk update: no changes requested"));
    }

    #[test]
    fn priority_on_a_triage_ticket_points_at_accept() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Captured",
                priority: None,
                selection_state: Some("triage"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-1");
        a.priority = Some(crate::domain::priority::Priority::P1);
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(
                "tk update: 'tk-1' is in triage; set a Priority by accepting it with \
                 'tk accept tk-1 --priority Pn'"
            ),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn updates_title_and_body_via_message_flag() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Original",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-1");
        a.message = vec!["New title".into(), "New body line".into()];
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Updated Ticket: tk-1 - New title"));

        let conn = Connection::open(store.db_path()).unwrap();
        let (title, body): (String, String) = conn
            .query_row("select title, body from items", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "New title");
        assert_eq!(body, "New body line");
    }

    #[test]
    fn priority_change_on_local_ticket_is_silent_to_mutations() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "T",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-1");
        a.priority = Some(Priority::P0);
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok);
        let conn = Connection::open(store.db_path()).unwrap();
        let priority: String = conn
            .query_row("select priority from items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(priority, "P0");
        let mutations: i64 = conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0);
    }

    #[test]
    fn priority_on_epic_is_a_usage_error() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-1");
        a.priority = Some(Priority::P0);
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk update: --priority cannot be set on an Epic"));
    }

    #[test]
    fn parent_on_epic_is_a_usage_error() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-1",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Epic",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-1");
        a.no_parent = true;
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk update: --parent / --no-parent cannot be set on an Epic"));
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args("tk-9999");
        a.message = vec!["X".into()];
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk update: 'tk-9999' is not a known Display ID or Alias"));
    }
}
