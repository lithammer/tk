//! `tk update` — update title, body, priority, or parent of a Ticket or
//! Epic.
//!
//! ADR-0051: omitted fields keep their Repository Store values; an explicit
//! body replaces the field, including empty input. Requested fields and their
//! Mutations commit together. Epics reject Priority and parent edits.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
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
    /// Replace the title with one nonblank line; trim outer spaces and tabs.
    #[arg(short = 't', long)]
    pub title: Option<String>,
    /// Replace the body with literal text; an empty value clears it.
    #[arg(short = 'b', long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Replace the body from a UTF-8 file, or '-' for stdin; empty input clears it.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<String>,
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

/// Commit field edits and their Mutations together in the Repository Store
/// (ADR-0051).
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let has_parent_op = args.parent.is_some() || args.no_parent;
    if args.title.is_none()
        && args.body.is_none()
        && args.body_file.is_none()
        && args.priority.is_none()
        && !has_parent_op
    {
        return Err(CommandError::usage(
            "no changes requested; supply at least one of \
             --title / --body / --body-file / --priority / --parent / --no-parent",
        ));
    }

    let title = args.title.as_deref().map(validate_title).transpose()?;
    let body = match args.body_file.as_deref() {
        Some("-") => {
            let mut body = String::new();
            deps.stdin.read_to_string(&mut body).map_err(|err| {
                CommandError::failure(format!("failed to read body from stdin: {err}"))
            })?;
            Some(body)
        }
        Some(path) => Some(
            std::fs::read_to_string(deps.cwd.join(path))
                .map_err(|err| CommandError::failure(format!("failed to read '{path}': {err}")))?,
        ),
        None => args.body,
    };
    if body.as_deref().is_some_and(|body| body.contains('\0')) {
        return Err(CommandError::failure("body contains a NUL byte"));
    }

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
        title,
        body: body.as_deref(),
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
        Err(update::UpdateError::BackendBinding(err)) => Err(resolver::backend_binding_error(&err)),
        Err(update::UpdateError::Mutation(err)) => Err(CommandError::failure(format!(
            "failed to append Mutation: {err}"
        ))),
    }
}

/// Validate a title and return it with outer ASCII spaces and tabs trimmed
/// (ADR-0051).
fn validate_title(title: &str) -> Result<&str, CommandError> {
    if title.contains('\0') {
        return Err(CommandError::failure("title contains a NUL byte"));
    }
    if title.contains([
        '\n', '\u{b}', '\u{c}', '\r', '\u{85}', '\u{2028}', '\u{2029}',
    ]) {
        return Err(CommandError::failure("title must be a single line"));
    }
    let title = title.trim_matches([' ', '\t']);
    if title.chars().all(char::is_whitespace) {
        return Err(CommandError::failure(
            "title must contain a non-whitespace character",
        ));
    }
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::store::testing::{FixtureItem, TmpStore, insert_fixture_item};
    use rusqlite::Connection;

    #[test]
    fn cli_rejects_removed_options_and_conflicting_body_sources() {
        for options in [
            vec!["-m", "Title"],
            vec!["--message", "Title"],
            vec!["-F", "-"],
            vec!["--file", "-"],
            vec!["--body", "Text", "--body-file", "-"],
            vec!["--body-file", "-", "-b", "Text"],
        ] {
            let cwd_path = cwd();
            let mut h = Harness::new(&cwd_path);
            h.stdin = std::io::Cursor::new(b"Must not be consumed".to_vec());
            let argv: Vec<_> = ["update", "tk-1"]
                .into_iter()
                .chain(options)
                .map(String::from)
                .collect();
            assert_eq!(crate::cli::run_argv(h.deps(), &argv).unwrap(), Exit::Usage);
            assert!(h.out().is_empty());
            assert_eq!(h.stdin.position(), 0);
        }
    }

    #[test]
    fn bad_body_input_leaves_all_fields_and_mutations_unchanged() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Original",
                body: "Keep body",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);
        for (source, bytes, error) in [
            ("inline", b"a\0b".as_slice(), "body contains a NUL byte"),
            ("file", b"a\0b".as_slice(), "body contains a NUL byte"),
            ("stdin", b"a\0b".as_slice(), "body contains a NUL byte"),
            ("file", b"a\xff".as_slice(), "failed to read 'body.txt':"),
            (
                "stdin",
                b"a\xff".as_slice(),
                "failed to read body from stdin:",
            ),
            ("missing", b"".as_slice(), "failed to read 'missing.txt':"),
        ] {
            let mut h = Harness::new(&fixture.toplevel);
            let input = match source {
                "inline" => ["--body", std::str::from_utf8(bytes).unwrap()],
                "file" => {
                    std::fs::write(fixture.toplevel.join("body.txt"), bytes).unwrap();
                    ["--body-file", "body.txt"]
                }
                "stdin" => {
                    h.stdin = std::io::Cursor::new(bytes.to_vec());
                    ["--body-file", "-"]
                }
                "missing" => ["--body-file", "missing.txt"],
                _ => unreachable!(),
            };
            let argv = [
                "update", "gh-1", "-t", "Changed", "-p", "P0", input[0], input[1],
            ]
            .map(String::from);
            assert_eq!(
                crate::cli::run_argv(h.deps(), &argv).unwrap(),
                Exit::Failure
            );
            assert!(
                h.err().starts_with(&format!("tk update: {error}")),
                "{}",
                h.err()
            );
            expect_git(&h, &fixture);
            let store = resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
            let item = crate::store::repository::show::show_item(&store, "gh-1")
                .unwrap()
                .unwrap();
            assert_eq!(item.title, "Original");
            assert_eq!(item.body, "Keep body");
            assert_eq!(item.priority, Some(Priority::P2));
            assert!(crate::store::sync::mutation_log_is_empty(&store.conn).unwrap());
        }
    }

    #[test]
    fn title_validation_rejects_blank_and_line_breaks_without_partial_edits() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Original",
                body: "Keep body",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);
        for title in [
            "",
            " \t",
            "\u{a0}\u{2003}",
            "A\0B",
            "A\nB",
            "A\rB",
            "A\r\nB",
            "A\u{b}B",
            "A\u{c}B",
            "A\u{85}B",
            "A\u{2028}B",
            "A\u{2029}B",
        ] {
            let mut h = Harness::new(&fixture.toplevel);
            let argv = [
                "update", "tk-1", "--title", title, "-b", "Changed", "-p", "P0",
            ]
            .map(String::from);
            assert_eq!(
                crate::cli::run_argv(h.deps(), &argv).unwrap(),
                Exit::Failure,
                "{title:?}"
            );
            assert!(h.err().starts_with("tk update: title "), "{}", h.err());
            expect_git(&h, &fixture);
            let store = resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
            let item = crate::store::repository::show::show_item(&store, "tk-1")
                .unwrap()
                .unwrap();
            assert_eq!(item.title, "Original");
            assert_eq!(item.body, "Keep body");
            assert_eq!(item.priority, Some(Priority::P2));
        }
        let mut h = Harness::new(&fixture.toplevel);
        expect_git(&h, &fixture);
        let argv = ["update", "tk-1", "--title", " \tA\t\u{a0}B\u{a0} \t"].map(String::from);
        assert_eq!(crate::cli::run_argv(h.deps(), &argv).unwrap(), Exit::Ok);
        expect_git(&h, &fixture);
        let store = resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
        let item = crate::store::repository::show::show_item(&store, "tk-1")
            .unwrap()
            .unwrap();
        assert_eq!(item.title, "A\t\u{a0}B\u{a0}");
    }

    #[test]
    fn body_sources_replace_literal_text_and_clear_without_changing_title() {
        for item_class in ["ticket", "epic"] {
            for source in ["inline", "file", "stdin"] {
                let fixture = TmpStore::new("repo");
                let conn = seed_store(&fixture);
                insert_fixture_item(
                    &conn,
                    FixtureItem {
                        id: "i1",
                        display: "tk-1",
                        item_class,
                        ticket_kind: (item_class == "ticket").then_some("task"),
                        priority: (item_class == "ticket").then_some("P2"),
                        title: "Keep title",
                        body: "Old body",
                        created_seq: 1,
                        ..FixtureItem::default()
                    },
                )
                .unwrap();
                drop(conn);
                for body in ["@notes.md\r\n\n  Keep spaces\t\r\n", " \t\n", ""] {
                    let mut h = Harness::new(&fixture.toplevel);
                    let input = match source {
                        "inline" => ["-b", body],
                        "file" => {
                            std::fs::write(fixture.toplevel.join("body.txt"), body).unwrap();
                            ["--body-file", "body.txt"]
                        }
                        "stdin" => {
                            h.stdin = std::io::Cursor::new(body.as_bytes().to_vec());
                            ["--body-file", "-"]
                        }
                        _ => unreachable!(),
                    };
                    let argv = ["update", "tk-1", input[0], input[1]].map(String::from);
                    expect_git(&h, &fixture);
                    assert_eq!(crate::cli::run_argv(h.deps(), &argv).unwrap(), Exit::Ok);
                    expect_git(&h, &fixture);
                    let store =
                        resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
                    let item = crate::store::repository::show::show_item(&store, "tk-1")
                        .unwrap()
                        .unwrap();
                    assert_eq!(item.title, "Keep title");
                    assert_eq!(item.body, body, "{item_class} via {source}");
                    assert!(crate::store::sync::mutation_log_is_empty(&store.conn).unwrap());
                }
            }
        }
    }

    #[test]
    fn title_only_cli_edit_preserves_body_in_store_and_mutation() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "gh-1",
                title: "Original",
                body: "Steps and context\r\n",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("1"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);
        let mut h = Harness::new(&fixture.toplevel);
        expect_git(&h, &fixture);
        let argv = ["update", "gh-1", "-t", "Corrected title"].map(String::from);
        assert_eq!(crate::cli::run_argv(h.deps(), &argv).unwrap(), Exit::Ok);
        expect_git(&h, &fixture);
        let store = resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
        let item = crate::store::repository::show::show_item(&store, "gh-1")
            .unwrap()
            .unwrap();
        assert_eq!(item.title, "Corrected title");
        assert_eq!(item.body, "Steps and context\r\n");
        let mutation = crate::store::sync::show_mutation_log(&store.conn, 1).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&mutation.payload_json).unwrap(),
            serde_json::json!({"title": "Corrected title", "body": "Steps and context\r\n"})
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
            title: None,
            body: None,
            body_file: None,
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
    fn field_edits_compose_with_priority_and_parent() {
        let fixture = TmpStore::new("repo");
        let conn = seed_store(&fixture);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Original",
                body: "Old body",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "e1",
                display: "tk-2",
                title: "Epic",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);
        let mut h = Harness::new(&fixture.toplevel);
        h.stdin = std::io::Cursor::new(b"New body\n".to_vec());
        expect_git(&h, &fixture);
        let argv = [
            "update",
            "tk-1",
            "--title",
            "New title",
            "--body-file",
            "-",
            "--priority",
            "P0",
            "--parent",
            "tk-2",
        ]
        .map(String::from);
        assert_eq!(crate::cli::run_argv(h.deps(), &argv).unwrap(), Exit::Ok);
        assert_eq!(h.out(), "Updated Ticket: tk-1 - New title\n");
        expect_git(&h, &fixture);
        let store = resolver::open_for_command(&h.runner, &fixture.toplevel, &h.clock).unwrap();
        let item = crate::store::repository::show::show_item(&store, "tk-1")
            .unwrap()
            .unwrap();
        assert_eq!(item.title, "New title");
        assert_eq!(item.body, "New body\n");
        assert_eq!(item.priority, Some(Priority::P0));
        assert_eq!(item.parent.unwrap().display_id, "tk-2");
        assert!(crate::store::sync::mutation_log_is_empty(&store.conn).unwrap());
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
        a.title = Some("X".into());
        let code = run_rendered(&mut h, a);
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk update: 'tk-9999' is not a known Display ID or Alias"));
    }
}
