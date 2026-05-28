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

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::Deps;
use crate::commands::message::{self, Input as MessageInput};
use crate::commands::resolver;
use crate::domain::priority::Priority;
use crate::domain::ticket_kind::TicketKind;
use crate::store::repository::create::{
    self, CreateError, CreateLocalEpicInput, CreateLocalTicketInput,
};

const COMMAND: &str = "add";

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
    #[arg(short = 'p', long, value_name = "PRIORITY", value_parser = parse_priority)]
    pub priority: Option<Priority>,
}

fn parse_priority(s: &str) -> Result<Priority, String> {
    match s {
        "P0" => Ok(Priority::P0),
        "P1" => Ok(Priority::P1),
        "P2" => Ok(Priority::P2),
        "P3" => Ok(Priority::P3),
        "P4" => Ok(Priority::P4),
        other => Err(format!(
            "invalid priority `{other}` (expected one of P0, P1, P2, P3, P4)"
        )),
    }
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    let Deps {
        stdout,
        stderr,
        stdin,
        runner,
        clock,
        rng,
        cwd,
        ..
    } = deps;

    let input = match select_input(&args) {
        Ok(input) => input,
        Err(line) => {
            let _ = writeln!(stderr, "{line}");
            return 2;
        }
    };

    let parsed = match message::read_input(input, cwd, stdin) {
        Ok(parsed) => parsed,
        Err(err) => {
            render_message_error(stderr, &err);
            return 1;
        }
    };

    let mut store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    let parent_ref = if let Some(arg) = args.parent.as_deref() {
        match resolver::resolve_epic_with_display(&store, arg) {
            Ok(r) => Some(r),
            Err(resolver::ResolveEpicError::NotFound) => {
                let _ = writeln!(
                    stderr,
                    "tk add: parent '{arg}' is not a known Display ID or Alias"
                );
                return 1;
            }
            Err(resolver::ResolveEpicError::NotAnEpic) => {
                let _ = writeln!(stderr, "tk add: parent '{arg}' is not an Epic");
                return 1;
            }
            Err(resolver::ResolveEpicError::Storage(err)) => {
                resolver::render_storage_error(stderr, COMMAND, &err);
                return 1;
            }
        }
    } else {
        None
    };

    if args.epic {
        let result = create::create_local_epic(
            &mut store,
            clock,
            rng,
            CreateLocalEpicInput {
                title: &parsed.title,
                body: &parsed.body,
            },
        );
        let epic = match result {
            Ok(e) => e,
            Err(err) => {
                render_create_error(stderr, &err);
                return 1;
            }
        };
        let _ = writeln!(stdout, "Created Epic: {} - {}", epic.display_id, epic.title);
        let _ = writeln!(stdout, "Status: {}", epic.status);
        return 0;
    }

    let kind = if args.bug {
        TicketKind::Bug
    } else {
        TicketKind::Task
    };
    let priority = args.priority.unwrap_or_default();
    let parent_id = parent_ref.as_ref().map(|r| r.id.as_str());

    let ticket = match create::create_local_ticket(
        &mut store,
        clock,
        rng,
        CreateLocalTicketInput {
            kind,
            priority,
            parent_id,
            title: &parsed.title,
            body: &parsed.body,
        },
    ) {
        Ok(t) => t,
        Err(err) => {
            render_create_error(stderr, &err);
            return 1;
        }
    };

    let _ = writeln!(
        stdout,
        "Created Ticket: {} - {}",
        ticket.display_id, ticket.title
    );
    let _ = writeln!(stdout, "Kind: {}", ticket.kind);
    let _ = writeln!(stdout, "Priority: {}", ticket.priority);
    let _ = writeln!(stdout, "Status: {}", ticket.status);
    if let Some(parent) = parent_ref.as_ref() {
        let _ = writeln!(stdout, "Parent: {}", parent.display_id);
    }
    0
}

fn select_input(args: &Args) -> Result<MessageInput<'_>, &'static str> {
    if !args.message.is_empty() {
        return Ok(MessageInput::Paragraphs(&args.message));
    }
    if let Some(path) = args.file.as_deref() {
        return Ok(MessageInput::File(path));
    }
    Err("tk add: a message is required (use -m or -F)")
}

fn render_message_error<W: Write + ?Sized>(stderr: &mut W, err: &message::ReadError) {
    match err {
        message::ReadError::Parse(message::ParseError::Empty) => {
            let _ = writeln!(stderr, "tk add: message is empty");
        }
        message::ReadError::Parse(message::ParseError::NulByte) => {
            let _ = writeln!(stderr, "tk add: message contains a NUL byte");
        }
        message::ReadError::File { path, source } => {
            let _ = writeln!(stderr, "tk add: failed to read '{path}': {source}");
        }
        message::ReadError::Stdin(source) => {
            let _ = writeln!(
                stderr,
                "tk add: failed to read message from stdin: {source}"
            );
        }
    }
}

fn render_create_error<W: Write + ?Sized>(stderr: &mut W, err: &CreateError) {
    match err {
        CreateError::Sqlite(err) => resolver::render_storage_error(stderr, COMMAND, err),
        CreateError::Sequence(err) => {
            let _ = writeln!(stderr, "tk add: Repository Store corruption: {err}");
        }
        CreateError::DisplayPrefixMissing => {
            let _ = writeln!(
                stderr,
                "tk add: Repository Store is missing the display_prefix seed (run 'tk init')"
            );
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

    fn args_with(message: Vec<String>) -> Args {
        Args {
            message,
            file: None,
            bug: false,
            epic: false,
            parent: None,
            priority: None,
        }
    }

    #[test]
    fn creates_a_local_ticket_with_minimal_message() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), args_with(vec!["Ship it".into()]));
        assert_eq!(code, 0);
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
        let code = run(h.deps(), a);
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Kind: bug"));
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
        let code = run(h.deps(), a);
        assert_eq!(code, 0);
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
            assert_eq!(run(h.deps(), a), 0);
        }

        // Then a child ticket referencing it. Distinct seed so the internal
        // 128-bit id doesn't collide with the epic's.
        let mut h = Harness::with_seed(&cwd_path, 11);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Child ticket".into()]);
        a.parent = Some("tk-1".into());
        let code = run(h.deps(), a);
        let stderr = String::from_utf8(h.stderr.clone()).unwrap();
        let stdout = String::from_utf8(h.stdout.clone()).unwrap();
        assert_eq!(code, 0, "stderr={stderr:?} stdout={stdout:?}");
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
        let code = run(h.deps(), a);
        assert_eq!(code, 1);
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
            assert_eq!(run(h.deps(), args_with(vec!["Standalone".into()])), 0);
        }

        // Trying to parent a new ticket under that ticket should fail.
        let mut h = Harness::with_seed(&cwd_path, 11);
        expect_git(&h, &store);
        let mut a = args_with(vec!["Child".into()]);
        a.parent = Some("tk-1".into());
        let code = run(h.deps(), a);
        assert_eq!(code, 1);
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
        let code = run(h.deps(), args_with(vec!["   ".into()]));
        assert_eq!(code, 1);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk add: message is empty"));
    }

    #[test]
    fn missing_message_returns_exit_2_with_usage_hint() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let code = run(h.deps(), args_with(vec![]));
        assert_eq!(code, 2);
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
        let code = run(h.deps(), a);
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Created Ticket: tk-1 - From stdin"));
    }
}
