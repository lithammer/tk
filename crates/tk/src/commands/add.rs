//! `tk add` — create a local Ticket or Epic from git-commit-style input.
//!
//! Reads the message from either repeated `-m` flags, a `-F path` file, or
//! stdin (`-F -`). The first paragraph becomes the title; later paragraphs
//! become the body. Class is `Ticket` by default; `--epic` switches to
//! Epic creation; `--bug` flips `ticket_kind` from `task` to `bug`.
//!
//! Flag combinations that can't be satisfied (a Ticket-only flag set with
//! `--epic`, both `--bug` and `--epic`, both `-m` and `-F`) are gated by
//! clap's `conflicts_with` so the handler doesn't repeat the policy.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::message::{self, Input as MessageInput};
use crate::commands::resolver;
use crate::domain::priority::Priority;
use crate::domain::ticket_kind::TicketKind;
use crate::store::repository::create::{
    self, CreateError, CreateLocalEpicInput, CreateLocalTicketInput, NewTicketSelection,
};

/// Flags for `tk add`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Message paragraph; repeatable (-m title -m body ...). Conflicts with -F.
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
    /// Create a bug Ticket (conflicts with --epic).
    #[arg(long, conflicts_with = "epic")]
    pub bug: bool,
    /// Create an Epic (conflicts with --bug, --priority, --parent).
    #[arg(long, conflicts_with_all = ["bug", "priority", "parent"])]
    pub epic: bool,
    /// Place the new Ticket under an Epic by Display ID or Alias.
    #[arg(short = 'P', long, value_name = "EPIC")]
    pub parent: Option<String>,
    /// Set Priority (P0..P4). Tickets only.
    #[arg(short = 'p', long, value_name = "PRIORITY")]
    pub priority: Option<Priority>,
    /// Capture the Ticket in triage with no Priority, pending a decision
    /// (conflicts with --priority and --epic).
    #[arg(long, conflicts_with_all = ["priority", "epic"])]
    pub triage: bool,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let input = select_input(&args).map_err(CommandError::usage)?;

    let parsed =
        message::read_input(input, deps.cwd, deps.stdin).map_err(|err| message_error(&err))?;

    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let parent_ref = if let Some(arg) = args.parent.as_deref() {
        match resolver::resolve_epic_with_display(&store, arg) {
            Ok(r) => Some(r),
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

    if args.epic {
        let epic = create::create_local_epic(
            &mut store,
            deps.clock,
            deps.rng,
            CreateLocalEpicInput {
                title: &parsed.title,
                body: &parsed.body,
            },
        )
        .map_err(|err| create_error(&err))?;
        let _ = writeln!(
            deps.stdout,
            "Created Epic: {} - {}",
            epic.display_id, epic.title
        );
        let _ = writeln!(deps.stdout, "Status: {}", epic.status);
        return Ok(Exit::Ok);
    }

    let kind = if args.bug {
        TicketKind::Bug
    } else {
        TicketKind::Task
    };
    // `--triage` captures unranked work (no Priority); otherwise the Ticket is
    // accepted at the requested or default Priority (ADR-0027). clap's
    // conflicts_with rules out `--triage --priority`, so the two never collide.
    let selection = if args.triage {
        NewTicketSelection::Triage
    } else {
        NewTicketSelection::Accepted(args.priority.unwrap_or_default())
    };
    let parent_id = parent_ref.as_ref().map(|r| r.id.as_str());

    let ticket = create::create_local_ticket(
        &mut store,
        deps.clock,
        deps.rng,
        CreateLocalTicketInput {
            kind,
            selection,
            parent_id,
            title: &parsed.title,
            body: &parsed.body,
        },
    )
    .map_err(|err| create_error(&err))?;

    let _ = writeln!(
        deps.stdout,
        "Created Ticket: {} - {}",
        ticket.display_id, ticket.title
    );
    let _ = writeln!(deps.stdout, "Kind: {}", ticket.kind);
    // A triage Ticket has no Priority, so it surfaces its Selection State
    // instead; accepted Tickets stay the quiet default and show only Priority.
    match ticket.priority {
        Some(priority) => {
            let _ = writeln!(deps.stdout, "Priority: {priority}");
        }
        None => {
            let _ = writeln!(deps.stdout, "Selection: {}", ticket.selection_state);
        }
    }
    let _ = writeln!(deps.stdout, "Status: {}", ticket.status);
    if let Some(parent) = parent_ref.as_ref() {
        let _ = writeln!(deps.stdout, "Parent: {}", parent.display_id);
    }
    Ok(Exit::Ok)
}

fn select_input(args: &Args) -> Result<MessageInput<'_>, &'static str> {
    if !args.message.is_empty() {
        return Ok(MessageInput::Paragraphs(&args.message));
    }
    if let Some(path) = args.file.as_deref() {
        return Ok(MessageInput::File(path));
    }
    Err("a message is required (use -m or -F)")
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

fn create_error(err: &CreateError) -> CommandError {
    match err {
        CreateError::Sqlite(err) => resolver::storage_error(err),
        CreateError::Sequence(err) => {
            CommandError::failure(format!("Repository Store corruption: {err}"))
        }
        CreateError::DisplayPrefixMissing => CommandError::failure(
            "Repository Store is missing the display_prefix seed (run 'tk init')",
        ),
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
    use std::path::Path;

    fn cwd() -> std::path::PathBuf {
        std::env::current_dir().unwrap()
    }

    fn seed_store(store: &TmpStore) {
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn.execute(
            "insert into store_config(key, value) values ('display_prefix', 'tk')",
            [],
        )
        .unwrap();
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
            Self::with_seed(cwd, 7)
        }
        fn with_seed(cwd: &'a Path, seed: u64) -> Self {
            Self {
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdin: std::io::Cursor::new(Vec::new()),
                runner: FakeRunner::new(),
                clock: FakeClock::new(1_778_284_800_000),
                rng: StdRng::seed_from_u64(seed),
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
    /// (ADR-0032: `tk add: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "add");
                exit
            }
        }
    }

    fn args_with(message: Vec<String>) -> Args {
        Args {
            message,
            file: None,
            bug: false,
            epic: false,
            parent: None,
            priority: None,
            triage: false,
        }
    }

    #[test]
    fn creates_a_local_ticket_with_minimal_message() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, args_with(vec!["Ship it".into()]));
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Created Ticket: tk-1 - Ship it"));
        assert!(stdout.contains("Kind: task"));
        assert!(stdout.contains("Priority: P2"));
        assert!(stdout.contains("Status: open"));
    }

    #[test]
    fn bug_flag_creates_a_bug_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Crash on click".into()]);
        a.bug = true;
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Kind: bug"));
    }

    #[test]
    fn triage_flag_creates_an_unranked_triage_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Investigate flaky test".into()]);
        a.triage = true;
        a.bug = true; // a triage bug is valid (ADR-0027)
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Kind: bug"));
        // A triage Ticket surfaces its Selection State, not a Priority line.
        assert!(stdout.contains("Selection: triage"), "stdout={stdout:?}");
        assert!(!stdout.contains("Priority:"), "stdout={stdout:?}");
    }

    #[test]
    fn epic_flag_creates_an_epic_without_priority_or_kind() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Big work".into()]);
        a.epic = true;
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Created Epic: tk-1 - Big work"));
        assert!(!stdout.contains("Priority:"));
        assert!(!stdout.contains("Kind:"));
    }

    #[test]
    fn parent_flag_attaches_ticket_to_epic_and_renders_parent_line() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();

        // First create an epic.
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            let mut a = args_with(vec!["Epic".into()]);
            a.epic = true;
            assert_eq!(run_rendered(&mut h, a), Exit::Ok);
        }

        // Then a child ticket referencing it. Distinct seed so the internal
        // 128-bit id doesn't collide with the epic's.
        let mut h = Harness::with_seed(&cwd_path, 11);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Child ticket".into()]);
        a.parent = Some("tk-1".into());
        let code = run_rendered(&mut h, a);
        let stderr = String::from_utf8(h.stderr.clone()).unwrap();
        let stdout = String::from_utf8(h.stdout.clone()).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={stderr:?} stdout={stdout:?}");
        assert!(stdout.contains("Created Ticket: tk-2"));
        assert!(stdout.contains("Parent: tk-1"));
    }

    #[test]
    fn unknown_parent_reports_typed_error() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Title".into()]);
        a.parent = Some("nope".into());
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk add: parent 'nope' is not a known Display ID or Alias"));
    }

    #[test]
    fn parent_must_be_an_epic_not_a_ticket() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();

        // Create a ticket first.
        {
            let mut h = Harness::new(&cwd_path);
            expect_git(&h, &store);
            assert_eq!(
                run_rendered(&mut h, args_with(vec!["Standalone".into()])),
                Exit::Ok
            );
        }

        // Trying to parent a new ticket under that ticket should fail.
        let mut h = Harness::with_seed(&cwd_path, 11);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Child".into()]);
        a.parent = Some("tk-1".into());
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk add: parent 'tk-1' is not an Epic"));
    }

    #[test]
    fn empty_message_reports_usage_error() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, args_with(vec!["   ".into()]));
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk add: message is empty"));
    }

    #[test]
    fn missing_message_returns_exit_2_with_usage_hint() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let code = run_rendered(&mut h, args_with(vec![]));
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk add: a message is required"));
    }

    #[test]
    fn stdin_message_via_dash_path() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        h.stdin = std::io::Cursor::new(b"From stdin\n\nBody p".to_vec());
        expect_git(&h, &store);
        let mut a = args_with(vec![]);
        a.file = Some("-".into());
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Created Ticket: tk-1 - From stdin"));
    }
}
