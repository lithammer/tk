//! `tk list` — render the Repository Store List Tree.
//!
//! View selection (`--ready` / `--blocked` / `--active` / `--triage` /
//! `--parked`) and origin filtering (`--local` / `--remote`) are mutually
//! exclusive within their group; clap's `conflicts_with` enforces the policy so
//! the handler doesn't repeat it. The `--epic` class filter is orthogonal
//! to both groups — it composes with any view and Origin (e.g.
//! `--ready --epic` lists Epics that contain ready child Tickets), so it
//! carries no conflicts. Rendering keeps ADR-0014 styling — status
//! glyph, priority text, kind_bug / kind_epic spans, dim row for
//! blocked items — and ends with a separator line and a status legend.

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::item_row::{MutationMarkers, render_chrome, render_row};
use crate::commands::{resolver, scope};
use crate::domain::mutation_state::MutationState;
use crate::render::palette;
use crate::render::styler::SubStyler;
use crate::store::promotion::{self as store_promotion, MutationSummary};
use crate::store::repository::list::{
    self, ListClassFilter, ListOptions, ListOriginFilter, ListRow, ListView,
};

/// Flags for `tk list`.
///
/// Seven `bool`s exceed pedantic's `struct_excessive_bools` cap, but clap's
/// derive API needs one field per `--flag`; collapsing into an enum would
/// fight clap's help generation. The `conflicts_with*` attrs make the
/// invalid combinations unrepresentable at the parser layer; `--epic` is
/// an orthogonal class filter and carries none.
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
    /// Show triage Tickets (captured, not yet accepted).
    #[arg(long, conflicts_with_all = ["ready", "blocked", "active"])]
    pub triage: bool,
    /// Show parked Tickets (accepted, held out of automatic selection).
    #[arg(long, conflicts_with_all = ["ready", "blocked", "active", "triage"])]
    pub parked: bool,
    /// Restrict to locally-authored items.
    #[arg(long, conflicts_with = "remote")]
    pub local: bool,
    /// Restrict to Remote-backed items.
    #[arg(long, conflicts_with = "local")]
    pub remote: bool,
    /// Show only Epics.
    #[arg(long)]
    pub epic: bool,
    /// Scope the listing to this Epic and its child Tickets. Falls back to
    /// the `TK_SCOPE` environment variable.
    #[arg(value_name = "EPIC_ID")]
    pub epic_id: Option<String>,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let scope_epic = scope::resolve(&store, args.epic_id.as_deref())?;

    let options = ListOptions {
        view: select_view(&args),
        origin: select_origin(&args),
        class: select_class(&args),
        scope: scope_epic.as_ref().map(|epic| epic.id.as_str()),
    };

    let rows = list::list_rows(&store, options).map_err(|err| resolver::storage_error(&err))?;

    // Read before writing anything: a storage failure here must not leave a
    // Scope hint on stdout promising a tree that never arrives.
    //
    // This read is store-wide, but it lives in `store/promotion.rs`, which
    // ARCHITECTURE.md scopes to `tk promote`. Reused rather than duplicated;
    // tk-166 moves it to `store/sync.rs`, where the equivalent
    // `applying_mutation_sequence` already sits.
    let queue_head = store_promotion::earliest_applicable_mutation(store.conn())
        .map_err(|err| resolver::storage_error(&err))?;

    let out = deps.styler.for_stdout();

    // Hint so a Scope-filtered tree never reads as the full store (ADR-0022).
    if let Some(epic) = scope_epic.as_ref() {
        if let Err(err) = render_scope_hint(deps.stdout, &epic.display_id, out) {
            return cli::write_error(&err);
        }
    }

    if let Err(err) = render_sync_banner(deps.stdout, queue_head.as_ref(), out) {
        return cli::write_error(&err);
    }

    if let Err(err) = render(deps.stdout, &rows, options, out) {
        return cli::write_error(&err);
    }
    Ok(Exit::Ok)
}

/// One-line banner above a Scope-filtered List Tree: a bold `Scope:` label,
/// the Epic Display ID in the Epic colour (matching the tree's `[epic]`
/// badge), and a dim reminder that child Tickets are included.
fn render_scope_hint<W: Write + ?Sized>(
    stdout: &mut W,
    display_id: &str,
    styler: SubStyler,
) -> std::io::Result<()> {
    writeln!(
        stdout,
        "{} {} {}",
        styler.wrap(palette::HEADER, "Scope:"),
        styler.wrap(palette::KIND_EPIC, display_id),
        styler.wrap(palette::SEPARATOR, "(Epic + child Tickets)"),
    )
}

/// One-line banner naming the Mutation Log's queue head: its Mutation
/// Sequence, state, and target Display ID, pointing at `tk sync log
/// <sequence>` for detail.
///
/// Fires only for a `Failed` or `Applying` head — the two states that need a
/// human. A `Pending` head is the ordinary state between syncs for a
/// local-first tracker with opt-in Backend support, so printing it always
/// would put a line on nearly every invocation.
///
/// Never claims a cause: `sync_cursors` has no last-error column, and an
/// Apply that fails on the environment leaves its row `pending` with no
/// outcome written, so the store cannot tell "sync could not reach the
/// Backend" from "sync has not run yet" — the common case. The banner says
/// where the queue is stuck, not why.
///
/// Never carries an item count: a store-wide rollup would count Items that
/// Scope, `--local` / `--remote`, and `--epic` deliberately exclude,
/// contradicting ADR-0022's confinement of this command. Naming the queue
/// head is a statement about the Mutation Log, not the rows in view — so
/// under an active Scope the banner may correctly name an Item outside that
/// Scope, directly beneath the `Scope:` hint. That is not a bug.
///
/// Never restates recovery guidance: `unresolved_failure` in
/// `commands/promote.rs` owns the verbatim ADR-0017 wording for `tk promote
/// reconcile` / `retry` / `cancel`. This banner only points at `tk sync log`.
///
/// Disagrees with the row markers on one case, deliberately: this read
/// applies no Mutation Type filter, while the markers exclude Promotions. So
/// a rejected Promotion — the first thing that goes wrong on a fresh
/// Backend — names an Item whose row then carries no marker, and a reader
/// following the banner to that row finds nothing on it. Surfacing a Pending
/// Promotion as such is tk-160; until it lands, `tk sync log <sequence>` is
/// the only place that case is legible.
///
/// In the other direction the two surfaces do agree: a failed row marker
/// always has a matching banner, because sync stops at the first rejection
/// so a failed Mutation is always the head. That coupling comes from the
/// write path, not from this function.
fn render_sync_banner<W: Write + ?Sized>(
    stdout: &mut W,
    head: Option<&MutationSummary>,
    styler: SubStyler,
) -> std::io::Result<()> {
    let Some(head) = head else {
        return Ok(());
    };
    if !matches!(head.state, MutationState::Failed | MutationState::Applying) {
        return Ok(());
    }
    let sync_log = format!("(tk sync log {})", head.sequence);
    writeln!(
        stdout,
        "{} Mutation {} {} on {} {}",
        styler.wrap(palette::HEADER, "Sync:"),
        head.sequence,
        head.state,
        // MutationSummary carries no Item class, so ID_TICKET and ID_EPIC
        // cannot be chosen between here; both resolve to cyan today, so the
        // anchor renders identically either way. Revisit if the two colours
        // ever diverge.
        styler.wrap(palette::ID_TICKET, &head.target_display_id),
        styler.wrap(palette::SEPARATOR, &sync_log),
    )
}

fn select_view(args: &Args) -> ListView {
    if args.ready {
        ListView::Ready
    } else if args.blocked {
        ListView::Blocked
    } else if args.active {
        ListView::Active
    } else if args.triage {
        ListView::Triage
    } else if args.parked {
        ListView::Parked
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

fn select_class(args: &Args) -> ListClassFilter {
    if args.epic {
        ListClassFilter::Epic
    } else {
        ListClassFilter::Any
    }
}

fn render<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    options: ListOptions<'_>,
    styler: SubStyler,
) -> std::io::Result<()> {
    if rows.is_empty() {
        writeln!(stdout, "{}", empty_message(options))?;
        return Ok(());
    }

    // Walk roots first; embed children inline so the renderer can lay
    // out a tree without a second pass over the row vector.
    let mut markers = MutationMarkers::default();
    for row in rows {
        if parent_is_in_rows(rows, row) {
            continue;
        }
        markers = markers.merge(render_row(stdout, row, "", styler)?);
        markers = markers.merge(render_children(stdout, rows, row, styler)?);
    }

    render_chrome(stdout, rows, markers, styler)
}

fn render_children<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    parent: &ListRow,
    styler: SubStyler,
) -> std::io::Result<MutationMarkers> {
    let child_count = count_rendered_children(rows, &parent.id);
    let mut child_index = 0usize;
    let mut markers = MutationMarkers::default();
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
        markers = markers.merge(render_row(stdout, child, prefix, styler)?);
    }
    Ok(markers)
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

fn empty_message(options: ListOptions<'_>) -> &'static str {
    // Only the Default view distinguishes Epic-vs-Any and Origin in its empty
    // message; the Ready / Blocked / Active views keep their per-view phrasing
    // because Epics may still exist there but simply contain no matching child.
    match options.view {
        ListView::Default => match (options.class, options.origin) {
            (ListClassFilter::Epic, ListOriginFilter::Local) => "No local epics.",
            (ListClassFilter::Epic, ListOriginFilter::Remote) => "No remote epics.",
            (ListClassFilter::Epic, ListOriginFilter::Any) => "No epics.",
            (ListClassFilter::Any, ListOriginFilter::Local) => "No local items.",
            (ListClassFilter::Any, ListOriginFilter::Remote) => "No remote items.",
            (ListClassFilter::Any, ListOriginFilter::Any) => "No open or active items.",
        },
        ListView::Ready => "No ready items.",
        ListView::Blocked => "No blocked items.",
        ListView::Active => "No active items.",
        ListView::Triage => "No triage items.",
        ListView::Parked => "No parked items.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, TmpStore, commit_promotion, insert_dependency,
        insert_fixture_item, insert_fixture_mutation,
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
        fn deps_with(&mut self, styler: Styler) -> Deps<'_> {
            Deps {
                stdout: &mut self.stdout,
                stderr: &mut self.stderr,
                stdin: &mut self.stdin,
                runner: &self.runner,
                clock: &self.clock,
                rng: &mut self.rng,
                cwd: self.cwd,
                styler,
            }
        }
    }

    /// Seed one Mutation against `item_id`, deriving the `failure_json` the
    /// `failed` state's CHECK requires. Mirrors the helper in
    /// `store/repository/list.rs`'s tests.
    fn seed_mutation(
        conn: &Connection,
        sequence: i64,
        item_id: &str,
        item_class: &str,
        mutation_type: &str,
        state: &str,
    ) {
        insert_fixture_mutation(
            conn,
            FixtureMutation {
                sequence,
                mutation_type,
                item_id,
                item_class,
                state,
                failure_json: (state == "failed").then_some(r#"{"detail":"prior"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
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
    /// (ADR-0032: `tk list: <body>`), so a test asserts the framed bytes.
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
                err.render(deps.stderr, "list");
                exit
            }
        }
    }

    fn default_args() -> Args {
        Args {
            ready: false,
            blocked: false,
            active: false,
            triage: false,
            parked: false,
            local: false,
            remote: false,
            epic: false,
            epic_id: None,
        }
    }

    #[test]
    fn empty_store_prints_empty_default_line() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "No open or active items.\n");
    }

    #[test]
    fn plain_list_marks_parked_tickets_with_a_badge() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "p1",
                display: "tk-1",
                title: "Held work",
                selection_state: Some("parked"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("[parked]"), "stdout={stdout:?}");
        assert!(stdout.contains("Held work"), "stdout={stdout:?}");
    }

    #[test]
    fn plain_list_marks_triage_tickets_with_a_badge() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Captured idea",
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
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("[triage]"), "stdout={stdout:?}");
        assert!(stdout.contains("Captured idea"), "stdout={stdout:?}");
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
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
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
        let code = run_rendered(
            &mut h,
            Args {
                ready: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("tk-1"));
        assert!(stdout.contains("tk-3"));
        assert!(!stdout.contains("tk-2"), "stdout={stdout:?}");
    }

    #[test]
    fn epic_flag_lists_only_epics() {
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
                id: "ticket",
                display: "tk-2",
                title: "Ticket",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("[epic] Epic"), "stdout={stdout:?}");
        assert!(!stdout.contains("tk-2"), "stdout={stdout:?}");
    }

    #[test]
    fn epic_flag_with_no_epics_prints_no_epics() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "ticket",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(String::from_utf8(h.stdout).unwrap(), "No epics.\n");
    }

    #[test]
    fn epic_flag_in_ready_view_keeps_per_view_message() {
        // The "No epics." empty message is Default-view-only. A ready Ticket
        // exists but is not an Epic, so `--ready --epic` matches nothing; the
        // Ready view must keep "No ready items." rather than claim "No epics.".
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "ready-ticket",
                display: "tk-1",
                title: "Ready ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                ready: true,
                epic: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(String::from_utf8(h.stdout).unwrap(), "No ready items.\n");
    }

    #[test]
    fn epic_flag_with_local_filter_names_local_epics_when_empty() {
        // The Default-view empty message reflects the Origin filter under
        // `--epic`, mirroring the non-epic path's "No local items.".
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic: true,
                local: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(String::from_utf8(h.stdout).unwrap(), "No local epics.\n");
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk list: Repository Store not initialized; run 'tk init'"));
    }

    #[test]
    fn scope_filters_to_epic_and_prints_a_hint() {
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "loose",
                display: "tk-3",
                title: "Loose",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic_id: Some("tk-1".to_owned()),
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Scope: tk-1 (Epic + child Tickets)"),
            "stdout={stdout:?}"
        );
        assert!(stdout.contains("[epic] Epic"));
        assert!(stdout.contains("tk-2"));
        assert!(!stdout.contains("tk-3"), "stdout={stdout:?}");
    }

    #[test]
    fn scope_to_a_ticket_is_rejected_as_not_an_epic() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic_id: Some("tk-1".to_owned()),
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk list: scope 'tk-1' is not an Epic"),
            "stderr={stderr:?}"
        );
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
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Epic line and the single └── child below it.
        assert!(stdout.contains("[epic] Epic"));
        assert!(stdout.contains("\u{2514}\u{2500}\u{2500} \u{25cb} tk-2"));
    }

    #[test]
    fn nested_child_row_reaches_the_legend_through_render_children() {
        // `render`'s top-level loop and `render_children` each merge their own
        // `MutationMarkers` into the running total (list.rs's fold has two
        // call sites, unlike search.rs's one). This Epic is open, so its
        // child nests under it instead of falling through to top level the
        // way the orphaned-child test's `done` Epic does — the only path
        // that exercises the `render_children` half of the fold.
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
                title: "Open epic",
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
                title: "Open child",
                container_id: Some("e1"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "child", "ticket", "update_ticket", "pending");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        insta::assert_snapshot!(stdout, @"
        ○ tk-1 [epic] Open epic
        └── ○ tk-2 ● P2 ~ Open child
        --------------------------------------------------------------------------------
        Total: 2 items (2 open)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked
        Mutations: ~ pending
        ");
    }

    #[test]
    fn mutation_markers_pin_exact_byte_placement_and_spare_clean_rows() {
        // A substring check proves a marker glyph appears somewhere in the
        // line, not that it sits in the right place in a renderer shared
        // with `tk search`; this pins the full row set's bytes instead.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "row-clean",
                display: "tk-1",
                title: "Clean row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "row-pending",
                display: "tk-2",
                title: "Pending row",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "row-failed",
                display: "tk-3",
                title: "Failed row",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "row-both",
                display: "tk-4",
                title: "Both markers",
                created_seq: 4,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // Pending at sequence 1 keeps the queue head pending, so `run` prints
        // no `Sync:` banner and this snapshot stays a pure row/chrome contract
        // (a failed queue head would prepend a banner line here instead).
        seed_mutation(
            &conn,
            1,
            "row-pending",
            "ticket",
            "update_ticket",
            "pending",
        );
        seed_mutation(&conn, 2, "row-failed", "ticket", "update_ticket", "failed");
        seed_mutation(&conn, 3, "row-both", "ticket", "update_ticket", "pending");
        seed_mutation(&conn, 4, "row-both", "ticket", "set_item_status", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // tk-1 carries no Mutation; a marker on its row would mean the
        // rollup joined on the wrong key. The snapshot below pins its
        // bytes as `○ tk-1 ● P2 Clean row` with no `~`/`⚑`.
        insta::assert_snapshot!(stdout, @"
        ○ tk-1 ● P2 Clean row
        ○ tk-2 ● P2 ~ Pending row
        ○ tk-3 ● P2 ⚑ Failed row
        ○ tk-4 ● P2 ⚑ ~ Both markers
        --------------------------------------------------------------------------------
        Total: 4 items (4 open)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked
        Mutations: ⚑ failed  ~ pending
        ");
    }

    #[test]
    fn row_set_with_no_marked_rows_emits_no_mutations_legend() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Clean row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            !stdout.contains("Mutations:"),
            "no row carries a Mutation; the legend must not appear: {stdout:?}"
        );
    }

    #[test]
    fn mutation_legend_names_only_the_glyphs_present_in_the_row_set() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Pending row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "t1", "ticket", "update_ticket", "pending");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Mutations: ~ pending\n"),
            "legend should show only the pending entry: {stdout:?}"
        );
        assert!(
            !stdout.contains("failed"),
            "no row is failed; the legend must not mention it: {stdout:?}"
        );
    }

    #[test]
    fn epic_with_a_failed_mutation_renders_the_marker_after_the_epic_badge() {
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
                title: "Epic with unsent work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "e1", "epic", "update_epic", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("[epic] \u{2691} Epic with unsent work"),
            "marker should sit between [epic] and the title: {stdout:?}"
        );
    }

    #[test]
    fn mutation_markers_nest_inside_the_blocked_row_dim_without_resetting_it() {
        // ADR-0014 nesting: BLOCKED_ROW's dim closes with SGR 22, the same
        // family a bold or dimmed inner span would close with. MUTATION_FAILED
        // and MUTATION_PENDING are both foreground colours (close 39)
        // precisely so either can sit inside the dim span without releasing
        // it before the row ends. Both markers are seeded here so both
        // claims are exercised, not just the pending one.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "tk-1",
                title: "Blocked work",
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
                title: "Blocker",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "blocked").unwrap();
        seed_mutation(&conn, 1, "blocked", "ticket", "update_ticket", "pending");
        seed_mutation(&conn, 2, "blocked", "ticket", "set_item_status", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered_with(&mut h, Styler::always(), default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let line = stdout
            .lines()
            .find(|l| l.contains("tk-1"))
            .unwrap_or_else(|| panic!("no row for tk-1 in {stdout:?}"));
        assert!(
            line.starts_with("\u{1b}[2m"),
            "row should open the BLOCKED_ROW dim span: {line:?}"
        );
        assert!(
            line.contains("\u{1b}[91m\u{2691}\u{1b}[39m"),
            "failed marker should open bright-red and close with 39; a dim-family \
             close would release BLOCKED_ROW mid-row: {line:?}"
        );
        assert!(
            line.contains("\u{1b}[90m~\u{1b}[39m"),
            "pending marker should open bright-black and close with 39: {line:?}"
        );
        assert_eq!(
            line.matches("\u{1b}[22m").count(),
            1,
            "a second SGR 22 means an inner span closes in the dim family and \
             releases BLOCKED_ROW before the row ends: {line:?}"
        );
    }

    #[test]
    fn queued_edit_behind_a_pending_promotion_marks_but_the_promotion_alone_does_not() {
        // `has_pending_mutation` excludes the Promotion's own Mutation type;
        // this drives a real Promotion through `commit_promotion` rather
        // than fixturing a `promote_ticket` row by hand, so the exclusion
        // is proven against the outbox `tk promote` actually writes.
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "promoted",
                display: "tk-1",
                title: "Edit queued behind the Promotion",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "promotion_only",
                display: "tk-2",
                title: "Promotion pending, nothing queued behind it",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // Each call consumes the real `mutation_seq` counter (1, then 2);
        // the fixture edit below must sequence after both.
        commit_promotion(&mut conn, "promoted");
        commit_promotion(&mut conn, "promotion_only");
        seed_mutation(&conn, 3, "promoted", "ticket", "update_ticket", "pending");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
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
            line_of("tk-1").contains(" ~ Edit queued"),
            "a queued edit behind a Pending Promotion is genuinely unsent \
             and must still mark: {stdout:?}"
        );
        let promotion_only_line = line_of("tk-2");
        assert!(
            !promotion_only_line.contains('~') && !promotion_only_line.contains('\u{2691}'),
            "a Pending Promotion is the Item's own creation, not a queued \
             edit; marking it would conflate the two: {promotion_only_line:?}"
        );
    }

    #[test]
    fn orphaned_child_of_an_excluded_epic_still_reaches_the_legend() {
        // The default view excludes a `done` Epic outright (no matching-child
        // fallback the way `--ready`/`--blocked`/etc. have one), so its open
        // child reaches `render` with its parent absent from `rows` and falls
        // through to top level — the case `render_mutation_legend`'s fold
        // assumes never drops a row's flags. A failure here means a row was
        // rendered whose flags the legend never saw — the fold's premise
        // broken.
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
                status: "done",
                title: "Done epic",
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
                title: "Open child of a done Epic",
                container_id: Some("epic"),
                container_class: Some("epic"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "child", "ticket", "update_ticket", "pending");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("Done epic"), "stdout={stdout:?}");
        let line = stdout
            .lines()
            .find(|l| l.contains("tk-2"))
            .unwrap_or_else(|| panic!("no row for tk-2 in {stdout:?}"));
        assert!(line.contains(" ~ Open child"), "stdout={stdout:?}");
        assert!(
            stdout.contains("Mutations: ~ pending\n"),
            "the child's flags must reach the legend even though its parent \
             is absent from rows: {stdout:?}"
        );
    }

    #[test]
    fn failed_queue_head_prints_the_sync_banner() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "t1", "ticket", "update_ticket", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Sync: Mutation 1 failed on tk-1 (tk sync log 1)\n"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn applying_queue_head_prints_the_sync_banner() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        // `applying` is confined to promote_ticket/promote_epic with a
        // matching item_class (migration 010's CHECK constraint).
        seed_mutation(&conn, 1, "t1", "ticket", "promote_ticket", "applying");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(
            stdout.contains("Sync: Mutation 1 applying on tk-1 (tk sync log 1)\n"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn failed_promotion_prints_the_banner_but_marks_no_row() {
        // `render_sync_banner`'s doc comment names this as the case a reader
        // hits first: the banner applies no Mutation Type filter, but the row
        // markers exclude Promotions (`has_pending_mutation` /
        // `has_failed_mutation` filter out promote_ticket/promote_epic). So a
        // failed Promotion names its Item in the `Sync:` banner while the
        // matching row stays unmarked and no `Mutations:` legend appears — a
        // failure here means the two surfaces stopped disagreeing on the
        // documented case, so check which one moved.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "t1", "ticket", "promote_ticket", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        insta::assert_snapshot!(stdout, @"
        Sync: Mutation 1 failed on tk-1 (tk sync log 1)
        ○ tk-1 ● P2 Row
        --------------------------------------------------------------------------------
        Total: 1 item (1 open)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked
        ");
    }

    #[test]
    fn pending_queue_head_prints_no_banner() {
        // Pending is the ordinary state between syncs; a banner here would
        // fire on nearly every invocation and stop meaning anything.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "t1", "ticket", "update_ticket", "pending");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("Sync:"), "stdout={stdout:?}");
    }

    #[test]
    fn empty_mutation_log_prints_no_banner() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Row",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("Sync:"), "stdout={stdout:?}");
    }

    #[test]
    fn failed_queue_head_banner_still_prints_above_an_empty_row_set() {
        // The queue head can be a `done` Ticket the Default view never
        // renders (tk-158): the banner still has to appear,
        // above the empty-view line, or a stuck queue on a done Ticket would
        // be invisible.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "d1",
                display: "tk-1",
                title: "Done row",
                status: "done",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "d1", "ticket", "update_ticket", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(
            stdout,
            "Sync: Mutation 1 failed on tk-1 (tk sync log 1)\nNo open or active items.\n"
        );
    }

    #[test]
    fn queue_head_banner_renders_below_the_scope_hint_and_may_name_an_out_of_scope_item() {
        // The banner describes the Mutation Log, not the rows in view, so it
        // may correctly name an Item the active Scope excludes (tk-158).
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
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "loose",
                display: "tk-3",
                title: "Loose",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(&conn, 1, "loose", "ticket", "update_ticket", "failed");
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic_id: Some("tk-1".to_owned()),
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        let scope_at = stdout
            .find("Scope: tk-1 (Epic + child Tickets)")
            .unwrap_or_else(|| panic!("no Scope hint in {stdout:?}"));
        let banner_at = stdout
            .find("Sync: Mutation 1 failed on tk-3 (tk sync log 1)")
            .unwrap_or_else(|| panic!("no Sync banner in {stdout:?}"));
        let tree_at = stdout
            .find("[epic] Epic")
            .unwrap_or_else(|| panic!("no tree content in {stdout:?}"));
        assert!(
            scope_at < banner_at && banner_at < tree_at,
            "expected Scope hint, then Sync banner, then the tree: {stdout:?}"
        );
    }
}
