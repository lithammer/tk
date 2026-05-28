//! `tk show` — render one Ticket or Epic with current state.
//!
//! Layout:
//!
//! ```text
//! <status-glyph> <display-id> · <title>
//!   <P_> · <kind> · <created> → <updated>    (Tickets)
//!   EPIC · <created> → <updated>             (Epics)
//!
//! DESCRIPTION
//! <body...>
//!
//! PARENT / TICKETS / BLOCKED BY / BLOCKING / EXTERNAL BLOCKERS
//!   <glyph> <status-glyph> <display-id>: [(EPIC) ]<title>[ ● <priority>]
//! ```
//!
//! Empty sections are omitted. Output ends with a single trailing newline.
//! The status word and Origin row are intentionally dropped — both
//! duplicate information already carried by the glyph and Display ID
//! shape (ADR-0014 anti-drift; the v1 single-Remote invariant lets the
//! Backend kind ride on the Display ID prefix).

use std::io::Write;

use anstyle::Style;
use clap::Args as ClapArgs;

use crate::cli::Deps;
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::priority::Priority;
use crate::domain::status::ItemStatus;
use crate::domain::ticket_kind::TicketKind;
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
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
    let Deps {
        stdout,
        stderr,
        runner,
        cwd,
        styler,
        ..
    } = deps;

    let store = match resolver::open_for_command(runner, cwd) {
        Ok(store) => store,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return 1;
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
            return 1;
        }
        Err(err) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    let sub = styler.for_stdout();
    if render(stdout, &detail, sub).is_err() {
        // stdout writes that fail are surfaced through the normal io::Error
        // path; `tk` does not retry a broken pipe.
        return 1;
    }
    0
}

fn render<W: Write + ?Sized>(
    stdout: &mut W,
    detail: &ItemDetail,
    styler: SubStyler,
) -> std::io::Result<()> {
    // Label line: <status-glyph> <display-id> · <title>
    write!(
        stdout,
        "{} ",
        styler.wrap(status_style(detail.status), detail.status.glyph())
    )?;
    write!(
        stdout,
        "{} · ",
        styler.wrap(id_style(detail.item_class), &detail.display_id)
    )?;
    write!(stdout, "{}", styler.open(palette::HEADER))?;
    sanitize::write_sanitized_line(stdout, detail.title.as_bytes())?;
    write!(stdout, "{}", styler.close(palette::HEADER))?;
    stdout.write_all(b"\n")?;

    // Facet bar.
    stdout.write_all(b"  ")?;
    match detail.item_class {
        ItemClass::Epic => {
            write!(stdout, "{}", styler.wrap(palette::KIND_EPIC, "EPIC"))?;
        }
        ItemClass::Ticket => {
            let priority = detail
                .priority
                .expect("Tickets always carry a Priority (schema CHECK)");
            write!(
                stdout,
                "{}",
                styler.wrap(priority_style(priority), priority.text())
            )?;
            stdout.write_all(b" \xc2\xb7 ")?; // " · "
            let kind = detail
                .ticket_kind
                .expect("Tickets always carry a TicketKind (schema CHECK)");
            match kind {
                TicketKind::Bug => {
                    write!(stdout, "{}", styler.wrap(palette::KIND_BUG, kind.text()))?;
                }
                TicketKind::Task => stdout.write_all(kind.text().as_bytes())?,
            }
        }
    }
    let created_date = first_chars(&detail.created_at, 10);
    let updated_date = first_chars(&detail.updated_at, 10);
    writeln!(stdout, " \u{b7} {created_date} \u{2192} {updated_date}")?;

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
        styler.wrap(status_style(item.status), item.status.glyph())
    )?;
    write!(
        stdout,
        "{}: ",
        styler.wrap(id_style(item.item_class), &item.display_id)
    )?;
    if item.item_class == ItemClass::Epic {
        write!(stdout, "{} ", styler.wrap(palette::KIND_EPIC, "(EPIC)"))?;
    }
    sanitize::write_sanitized_line(stdout, item.title.as_bytes())?;
    if let Some(p) = item.priority {
        let p_st = priority_style(p);
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

fn status_style(status: ItemStatus) -> Style {
    match status {
        ItemStatus::Open => palette::STATUS_OPEN,
        ItemStatus::Active => palette::STATUS_ACTIVE,
        ItemStatus::Done => palette::STATUS_DONE,
    }
}

fn priority_style(p: Priority) -> Style {
    match p {
        Priority::P0 => palette::PRIORITY_P0,
        Priority::P1 => palette::PRIORITY_P1,
        Priority::P2 => palette::PRIORITY_P2,
        Priority::P3 => palette::PRIORITY_P3,
        Priority::P4 => palette::PRIORITY_P4,
    }
}

fn id_style(class: ItemClass) -> Style {
    match class {
        ItemClass::Epic => palette::ID_EPIC,
        ItemClass::Ticket => palette::ID_TICKET,
    }
}

fn first_chars(s: &str, n: usize) -> &str {
    // Truncate by char count (not bytes); the created_at / updated_at
    // columns are ASCII ISO-8601 in practice, so this is equivalent to
    // `&s[..min(n, s.len())]`, but the char_indices form survives a
    // future stamp that happens to carry multi-byte content.
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
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
        assert_eq!(code, 1);
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
        assert_eq!(code, 1);
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
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Status glyph + Display ID + title on the label line.
        assert!(
            stdout.contains("\u{25cb} tk-1 \u{b7} Plain ticket\n"),
            "stdout={stdout:?}"
        );
        // Facet bar: P2 · task · 2026-05-09 → 2026-05-09
        assert!(stdout.contains("  P2 \u{b7} task \u{b7} 2026-05-09 \u{2192} 2026-05-09"));
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
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("EPIC"), "stdout={stdout:?}");
        assert!(stdout.contains("TICKETS"));
        assert!(stdout.contains("tk-2: Child ticket"));
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
