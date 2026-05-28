//! `tk list` — render the Repository Store List Tree.
//!
//! View selection (`--ready` / `--blocked` / `--active`) and origin
//! filtering (`--local` / `--remote`) are mutually exclusive within
//! their group; clap's `conflicts_with` enforces the policy so the
//! handler doesn't repeat it. Rendering keeps ADR-0014 styling — status
//! glyph, priority text, kind_bug / kind_epic spans, dim row for
//! blocked items — and ends with a separator line and a status legend.

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
use crate::store::repository::list::{self, ListOptions, ListOriginFilter, ListRow, ListView};

const COMMAND: &str = "list";

/// Flags for `tk list`.
///
/// Five `bool`s exceed pedantic's `struct_excessive_bools` cap, but clap's
/// derive API needs one field per `--flag`; collapsing into an enum would
/// fight clap's help generation. The `conflicts_with*` attrs make the
/// invalid combinations unrepresentable at the parser layer.
#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Show ready Tickets (open, no unresolved blockers).
    #[arg(long, conflicts_with_all = ["blocked", "active"])]
    pub ready: bool,
    /// Show blocked Tickets (open/active with unresolved blockers).
    #[arg(long, conflicts_with_all = ["ready", "active"])]
    pub blocked: bool,
    /// Show active Tickets and Epics.
    #[arg(long, conflicts_with_all = ["ready", "blocked"])]
    pub active: bool,
    /// Restrict to locally-authored items.
    #[arg(long, conflicts_with = "remote")]
    pub local: bool,
    /// Restrict to Remote-backed items.
    #[arg(long, conflicts_with = "local")]
    pub remote: bool,
}

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

    let options = ListOptions {
        view: select_view(&args),
        origin: select_origin(&args),
    };

    let store = match resolver::open_for_command(runner, cwd) {
        Ok(s) => s,
        Err(err) => {
            resolver::render_open_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    let rows = match list::list_rows(&store, options) {
        Ok(rows) => rows,
        Err(err) => {
            resolver::render_storage_error(stderr, COMMAND, &err);
            return 1;
        }
    };

    if render(stdout, &rows, options, styler.for_stdout()).is_err() {
        return 1;
    }
    0
}

fn select_view(args: &Args) -> ListView {
    if args.ready {
        ListView::Ready
    } else if args.blocked {
        ListView::Blocked
    } else if args.active {
        ListView::Active
    } else {
        ListView::Default
    }
}

fn select_origin(args: &Args) -> ListOriginFilter {
    if args.local {
        ListOriginFilter::Local
    } else if args.remote {
        ListOriginFilter::Remote
    } else {
        ListOriginFilter::Any
    }
}

fn render<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    options: ListOptions,
    styler: SubStyler,
) -> std::io::Result<()> {
    if rows.is_empty() {
        writeln!(stdout, "{}", empty_message(options))?;
        return Ok(());
    }

    let counts = StatusCounts::tally(rows);

    // Walk roots first; embed children inline so the renderer can lay
    // out a tree without a second pass over the row vector.
    for row in rows {
        if parent_is_in_rows(rows, row) {
            continue;
        }
        render_row(stdout, row, "", styler)?;
        render_children(stdout, rows, row, styler)?;
    }

    writeln!(
        stdout,
        "{}",
        styler.wrap(
            palette::SEPARATOR,
            "--------------------------------------------------------------------------------"
        )
    )?;

    render_total(stdout, rows.len(), counts)?;
    stdout.write_all(b"\n")?;

    write!(stdout, "Status: ")?;
    write!(
        stdout,
        "{} open  ",
        styler.wrap(palette::STATUS_OPEN, ItemStatus::Open.glyph())
    )?;
    write!(
        stdout,
        "{} active  ",
        styler.wrap(palette::STATUS_ACTIVE, ItemStatus::Active.glyph())
    )?;
    writeln!(
        stdout,
        "{} done",
        styler.wrap(palette::STATUS_DONE, ItemStatus::Done.glyph())
    )?;
    writeln!(
        stdout,
        "Blocked: {} blocked",
        styler.wrap(palette::BLOCKED, "\u{2298}")
    )?;
    Ok(())
}

fn render_children<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    parent: &ListRow,
    styler: SubStyler,
) -> std::io::Result<()> {
    let child_count = count_rendered_children(rows, &parent.id);
    let mut child_index = 0usize;
    for child in rows {
        let Some(container_id) = child.container_id.as_deref() else {
            continue;
        };
        if container_id != parent.id {
            continue;
        }
        child_index += 1;
        let prefix = if child_index == child_count {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };
        render_row(stdout, child, prefix, styler)?;
    }
    Ok(())
}

fn parent_is_in_rows(rows: &[ListRow], row: &ListRow) -> bool {
    let Some(container_id) = row.container_id.as_deref() else {
        return false;
    };
    rows.iter().any(|r| r.id == container_id)
}

fn count_rendered_children(rows: &[ListRow], parent_id: &str) -> usize {
    rows.iter()
        .filter(|r| r.container_id.as_deref() == Some(parent_id))
        .count()
}

fn empty_message(options: ListOptions) -> &'static str {
    match options.view {
        ListView::Default => match options.origin {
            ListOriginFilter::Local => "No local items.",
            ListOriginFilter::Remote => "No remote items.",
            ListOriginFilter::Any => "No open or active items.",
        },
        ListView::Ready => "No ready items.",
        ListView::Blocked => "No blocked items.",
        ListView::Active => "No active items.",
    }
}

fn render_row<W: Write + ?Sized>(
    stdout: &mut W,
    row: &ListRow,
    tree_prefix: &str,
    styler: SubStyler,
) -> std::io::Result<()> {
    stdout.write_all(tree_prefix.as_bytes())?;

    if row.has_unresolved_blocker {
        write!(stdout, "{}", styler.open(palette::BLOCKED_ROW))?;
    }

    write!(
        stdout,
        "{} ",
        styler.wrap(status_style(row.status), row.status.glyph())
    )?;
    write!(
        stdout,
        "{}",
        styler.wrap(id_style(row.item_class), &row.display_id)
    )?;

    if row.has_unresolved_blocker {
        write!(stdout, " {}", styler.wrap(palette::BLOCKED, "\u{2298}"))?;
    }

    match row.item_class {
        ItemClass::Ticket => {
            let priority = row
                .priority
                .expect("schema CHECK guarantees Tickets carry a Priority");
            let p_style = priority_style(priority);
            write!(stdout, " {} ", styler.wrap(p_style, "\u{25cf}"))?;
            write!(stdout, "{}", styler.wrap(p_style, priority.text()))?;
            if row.ticket_kind == Some(TicketKind::Bug) {
                write!(stdout, " {}", styler.wrap(palette::KIND_BUG, "[bug]"))?;
            }
            stdout.write_all(b" ")?;
            sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;
        }
        ItemClass::Epic => {
            write!(stdout, " {} ", styler.wrap(palette::KIND_EPIC, "[epic]"))?;
            sanitize::write_sanitized_line(stdout, row.title.as_bytes())?;
        }
    }

    if row.has_unresolved_blocker {
        write!(stdout, "{}", styler.close(palette::BLOCKED_ROW))?;
    }
    stdout.write_all(b"\n")
}

fn render_total<W: Write + ?Sized>(
    stdout: &mut W,
    total: usize,
    counts: StatusCounts,
) -> std::io::Result<()> {
    let noun = if total == 1 { "item" } else { "items" };
    write!(stdout, "Total: {total} {noun} (")?;
    let mut wrote = false;
    write_count(stdout, &mut wrote, counts.open, "open")?;
    write_count(stdout, &mut wrote, counts.active, "active")?;
    write_count(stdout, &mut wrote, counts.done, "done")?;
    writeln!(stdout, ")")
}

fn write_count<W: Write + ?Sized>(
    stdout: &mut W,
    wrote: &mut bool,
    count: usize,
    label: &str,
) -> std::io::Result<()> {
    if count == 0 {
        return Ok(());
    }
    if *wrote {
        write!(stdout, ", ")?;
    }
    write!(stdout, "{count} {label}")?;
    *wrote = true;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct StatusCounts {
    open: usize,
    active: usize,
    done: usize,
}

impl StatusCounts {
    fn tally(rows: &[ListRow]) -> Self {
        let mut counts = Self::default();
        for row in rows {
            match row.status {
                ItemStatus::Open => counts.open += 1,
                ItemStatus::Active => counts.active += 1,
                ItemStatus::Done => counts.done += 1,
            }
        }
        counts
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, TmpStore, insert_dependency, insert_fixture_item};
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

    fn default_args() -> Args {
        Args {
            ready: false,
            blocked: false,
            active: false,
            local: false,
            remote: false,
        }
    }

    #[test]
    fn empty_store_prints_empty_default_line() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), default_args());
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "No open or active items.\n");
    }

    #[test]
    fn renders_single_ticket_with_totals_and_legend() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ship it",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), default_args());
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("\u{25cb} tk-1 \u{25cf} P2 Ship it\n"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("Total: 1 item (1 open)"));
        assert!(stdout.contains("Status:"));
    }

    #[test]
    fn ready_view_excludes_blocked_tickets() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "ready",
                display: "tk-1",
                title: "Ready",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "tk-2",
                title: "Blocked",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "tk-3",
                title: "Blocker",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "blocked").unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(
            h.deps(),
            Args {
                ready: true,
                ..default_args()
            },
        );
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("tk-1"));
        assert!(stdout.contains("tk-3"));
        assert!(!stdout.contains("tk-2"), "stdout={stdout:?}");
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run(h.deps(), default_args());
        assert_eq!(code, 1);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk list: Repository Store not initialized; run 'tk init'"));
    }

    #[test]
    fn epic_with_a_child_ticket_renders_tree_glyphs() {
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
                title: "Child",
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
        let code = run(h.deps(), default_args());
        assert_eq!(code, 0);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Epic line and the single └── child below it.
        assert!(stdout.contains("[epic] Epic"));
        assert!(stdout.contains("\u{2514}\u{2500}\u{2500} \u{25cb} tk-2"));
    }
}
