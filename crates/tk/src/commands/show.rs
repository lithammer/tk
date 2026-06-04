//! `tk show` — render one Ticket or Epic with current state.
//!
//! Layout:
//!
//! ```text
//! <status-glyph> <display-id> · <title>
//!   <P_> · <Kind> · Created: <created>[ · Updated: <updated>]    (Tickets)
//!   Epic · Created: <created>[ · Updated: <updated>]             (Epics)
//!
//! DESCRIPTION
//! <body...>
//!
//! PARENT / TICKETS / BLOCKED BY / BLOCKING / EXTERNAL BLOCKERS
//!   <glyph> <status-glyph> <display-id>: [(Epic) ]<title>[ ● <priority>]
//! ```
//!
//! Empty sections are omitted. Output ends with a single trailing newline.
//! The status word and Origin row are intentionally dropped — both
//! duplicate information already carried by the glyph and Display ID
//! shape (ADR-0014 anti-drift; the v1 single-Remote invariant lets the
//! Backend kind ride on the Display ID prefix).

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::item_header::{self, Header};
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::show::{self, ExternalBlockerSummary, ItemDetail, ItemSummary};

const COMMAND: &str = "show";

/// Flags for `tk show`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to render.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Run `tk show <id>` against the supplied `Deps`. Returns the process exit code.
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

    let store = match resolver::open_for_command(runner, cwd, clock) {
        Ok(store) => store,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let detail = match show::show_item(&store, &args.id) {
        Ok(Some(detail)) => detail,
        Ok(None) => {
            let _ = writeln!(
                stderr,
                "tk {COMMAND}: '{id}' is not a known Display ID or Alias",
                id = args.id
            );
            return Exit::Failure;
        }
        Err(err) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return Exit::Failure;
        }
    };

    let sub = styler.for_stdout();
    if render(stdout, &detail, sub).is_err() {
        // stdout writes that fail are surfaced through the normal io::Error
        // path; `tk` does not retry a broken pipe.
        return Exit::Failure;
    }
    Exit::Ok
}

fn render<W: Write + ?Sized>(
    stdout: &mut W,
    detail: &ItemDetail,
    styler: SubStyler,
) -> std::io::Result<()> {
    // Label line + facet bar, shared verbatim with `tk grep` (ADR-0026). The
    // Updated facet is dropped when the Item has never been modified
    // (`updated_at == created_at`, the at-insert default); the labelled
    // `Created:` / `Updated:` form forecloses reading a bare `→` as a
    // start→end due window tk has no concept of.
    item_header::render_header(
        stdout,
        &Header {
            status: detail.status,
            display_id: &detail.display_id,
            item_class: detail.item_class,
            title: &detail.title,
            priority: detail.priority,
            ticket_kind: detail.ticket_kind,
            created_at: &detail.created_at,
            updated_at: &detail.updated_at,
        },
        None,
        styler,
    )?;

    let mut has_section = false;

    if !detail.body.is_empty() {
        stdout.write_all(b"\n")?;
        write_section_header(stdout, styler, "DESCRIPTION")?;
        sanitize::write_sanitized_body(stdout, detail.body.as_bytes())?;
        if !detail.body.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
        has_section = true;
    }

    // Closing Reason (ADR-0023): a Local Field rendered right after the body,
    // present only on `done` items. Body-like prose, so it mirrors DESCRIPTION
    // with an unconditional leading blank line rather than the relationship
    // sections' `if has_section` separator — local Tickets are often
    // title-only, so the bodyless done item is the common case.
    if let Some(reason) = detail.closing_reason.as_deref() {
        stdout.write_all(b"\n")?;
        write_section_header(stdout, styler, "CLOSING REASON")?;
        sanitize::write_sanitized_body(stdout, reason.as_bytes())?;
        if !reason.ends_with('\n') {
            stdout.write_all(b"\n")?;
        }
        has_section = true;
    }

    if let Some(parent) = detail.parent.as_ref() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "PARENT")?;
        render_sub_row(stdout, "\u{2191}", parent, styler)?;
        has_section = true;
    }

    if !detail.children.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "TICKETS")?;
        for child in &detail.children {
            render_sub_row(stdout, "\u{2193}", child, styler)?;
        }
        has_section = true;
    }

    if !detail.blocked_by.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "BLOCKED BY")?;
        for item in &detail.blocked_by {
            render_sub_row(stdout, "\u{2192}", item, styler)?;
        }
        has_section = true;
    }

    if !detail.blocking.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "BLOCKING")?;
        for item in &detail.blocking {
            render_sub_row(stdout, "\u{2192}", item, styler)?;
        }
        has_section = true;
    }

    if !detail.external_blockers.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "EXTERNAL BLOCKERS")?;
        for eb in &detail.external_blockers {
            render_external_blocker(stdout, eb)?;
        }
    }

    Ok(())
}

fn write_section_header<W: Write + ?Sized>(
    stdout: &mut W,
    styler: SubStyler,
    label: &str,
) -> std::io::Result<()> {
    writeln!(stdout, "{}", styler.wrap(palette::HEADER, label))
}

fn render_sub_row<W: Write + ?Sized>(
    stdout: &mut W,
    glyph: &str,
    item: &ItemSummary,
    styler: SubStyler,
) -> std::io::Result<()> {
    write!(stdout, "  {glyph} ")?;
    write!(
        stdout,
        "{} ",
        styler.wrap(palette::status_style(item.status), item.status.glyph())
    )?;
    write!(
        stdout,
        "{}: ",
        styler.wrap(palette::id_style(item.item_class), &item.display_id)
    )?;
    if item.item_class == ItemClass::Epic {
        write!(stdout, "{} ", styler.wrap(palette::KIND_EPIC, "(Epic)"))?;
    }
    sanitize::write_sanitized_line(stdout, item.title.as_bytes())?;
    if let Some(p) = item.priority {
        let p_st = palette::priority_style(p);
        write!(
            stdout,
            " {} {}",
            styler.wrap(p_st, "\u{25cf}"),
            styler.wrap(p_st, p.text())
        )?;
    }
    stdout.write_all(b"\n")
}

fn render_external_blocker<W: Write + ?Sized>(
    stdout: &mut W,
    eb: &ExternalBlockerSummary,
) -> std::io::Result<()> {
    stdout.write_all(b"  \xe2\x80\xa2 ")?; // "  • "
    sanitize::write_sanitized_line(stdout, eb.reason.as_bytes())?;
    stdout.write_all(b"\n")
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

    fn expect_git(harness: &Harness<'_>, store: &TmpStore) {
        harness.runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk show: Repository Store not initialized; run 'tk init'"),
            "stderr={stderr:?}"
        );
    }

    #[test]
    fn unknown_id_renders_not_found_with_arg_verbatim() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                id: "tk-9999".into(),
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk show: 'tk-9999' is not a known Display ID or Alias"));
    }

    #[test]
    fn renders_minimal_ticket() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Plain ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Status glyph + Display ID + title on the label line.
        assert!(
            stdout.contains("\u{25cb} tk-1 \u{b7} Plain ticket\n"),
            "stdout={stdout:?}"
        );
        // Facet bar: P2 · Task · Created: 2026-05-09 — the fixture leaves
        // updated_at == created_at, so the Updated facet is omitted.
        assert!(stdout.contains("  P2 \u{b7} Task \u{b7} Created: 2026-05-09\n"));
        assert!(!stdout.contains("Updated:"), "stdout={stdout:?}");
    }

    #[test]
    fn renders_updated_facet_only_when_item_was_modified() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Edited ticket",
                created_at: "2026-05-16T00:00:00.000Z",
                updated_at: "2026-05-29T12:34:56.000Z",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout
                .contains("  P2 \u{b7} Task \u{b7} Created: 2026-05-16 \u{b7} Updated: 2026-05-29"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn renders_epic_with_children() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "child",
                display: "tk-2",
                title: "Child ticket",
                container_id: Some("epic"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Facet bar carries the capitalized `Epic` token (asserted on the
        // leading `  Epic \u{b7}` so the Epic title can't satisfy it by chance).
        assert!(
            stdout.contains("  Epic \u{b7} Created: 2026-05-09"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("TICKETS"));
        assert!(stdout.contains("tk-2: Child ticket"));
    }

    #[test]
    fn renders_closing_reason_section_after_description_for_a_done_ticket() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                body: "Some body",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set closing_reason = ?1 where id = 't1'",
            rusqlite::params!["Fixed in PR #12"],
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("Fixed in PR #12\n"), "stdout={stdout:?}");
        // The Closing Reason follows the body with one blank line separator.
        assert!(
            stdout.contains("Some body\n\nCLOSING REASON"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn renders_closing_reason_with_a_leading_blank_line_for_a_bodyless_ticket() {
        // Local Tickets are often title-only, so a `done` item with a reason
        // but no body is the common case; the section still needs a blank line
        // after the facet bar, mirroring DESCRIPTION (ADR-0023).
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Quick fix",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        conn.execute(
            "update items set closing_reason = ?1 where id = 't1'",
            rusqlite::params!["Done in standup"],
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("\n\nCLOSING REASON\nDone in standup\n"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn omits_closing_reason_section_when_absent() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Done, no reason",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let _ = run(h.deps(), Args { id: "tk-1".into() });
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("CLOSING REASON"), "stdout={stdout:?}");
    }

    #[test]
    fn renders_description_body_when_present() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                body: "Multi-line\nbody",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let _ = run(h.deps(), Args { id: "tk-1".into() });
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("DESCRIPTION"));
        assert!(stdout.contains("Multi-line\nbody\n"));
    }
}
