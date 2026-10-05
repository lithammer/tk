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
use crate::store::repository::list::{
    self, ListClassFilter, ListOptions, ListOriginFilter, ListRow, ListView,
};
use crate::store::sync::{
    MutationSummary, UnresolvedMutationCounts, earliest_applicable_mutation,
    unresolved_mutation_counts,
};

/// Flags for `tk list`.
///
/// Clap needs one field per flag. The parser rejects conflicting flags;
/// `--epic` composes with any view or Origin filter.
#[derive(Debug, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Show open, idle, accepted Tickets with no unresolved blockers.
    #[arg(long, conflicts_with_all = ["blocked", "active"])]
    pub ready: bool,
    /// Show open or active Tickets with unresolved blockers, excluding triage.
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
    /// Restrict to items with Local Origin.
    #[arg(long, conflicts_with = "remote")]
    pub local: bool,
    /// Restrict to items with Backend Origin.
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
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock, deps.data_root)
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
    let banner_head = earliest_applicable_mutation(store.conn())
        .map_err(|err| resolver::storage_error(&err))?
        .filter(banner_worthy);

    // Same read-before-write rule: the trailer is written last, so reading it
    // where it is written would put a whole List Tree on stdout and then fail.
    let unresolved =
        unresolved_mutation_counts(store.conn()).map_err(|err| resolver::storage_error(&err))?;

    let out = deps.styler.for_stdout();

    let scope_display_id = scope_epic.as_ref().map(|epic| epic.display_id.as_str());
    if let Err(err) = render_banners(deps.stdout, scope_display_id, banner_head.as_ref(), out) {
        return cli::write_error(&err);
    }

    if let Err(err) = render(deps.stdout, &rows, options, unresolved, out) {
        return cli::write_error(&err);
    }
    Ok(Exit::Ok)
}

/// Write the Scope hint, then the Mutation Log banner, followed by one blank
/// line. Write nothing when neither applies. Each banner must end its line
/// so the final newline separates the block from the tree or empty message.
/// ARCHITECTURE.md records the separator rules.
fn render_banners<W: Write + ?Sized>(
    stdout: &mut W,
    scope_display_id: Option<&str>,
    banner_head: Option<&MutationSummary>,
    styler: SubStyler,
) -> std::io::Result<()> {
    let mut block = Vec::new();
    // Hint so a Scope-filtered tree never reads as the full store (ADR-0022).
    if let Some(display_id) = scope_display_id {
        render_scope_hint(&mut block, display_id, styler)?;
    }
    if let Some(head) = banner_head {
        render_sync_banner(&mut block, head, styler)?;
    }
    if block.is_empty() {
        return Ok(());
    }
    block.push(b'\n');
    stdout.write_all(&block)
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

/// Whether a Mutation Log queue head earns a `Sync:` banner: `Failed` and
/// `Applying` are the two states that need a human.
///
/// `Pending` is the ordinary state between syncs for a local-first tracker
/// with opt-in Backend support, so a banner for it would fire on nearly every
/// invocation.
fn banner_worthy(head: &MutationSummary) -> bool {
    matches!(head.state, MutationState::Failed | MutationState::Applying)
}

/// Name the global Mutation Log queue head, even when its Item is outside
/// Scope. Report its Mutation Sequence, state, and target Display ID, not a
/// cause or Item count. Callers must pass a head that cleared `banner_worthy`.
///
/// Point at `tk sync log <sequence>` for detail; `unresolved_failure` in
/// `commands/promote.rs` owns Promotion recovery guidance (ADR-0017).
/// Promotion failures belong here too, though their row label is distinct
/// from other Mutation markers (ADR-0041).
fn render_sync_banner<W: Write + ?Sized>(
    stdout: &mut W,
    head: &MutationSummary,
    styler: SubStyler,
) -> std::io::Result<()> {
    let sync_log = format!("(tk sync log {})", head.sequence);
    writeln!(
        stdout,
        "{} Mutation {} {} on {} {}",
        styler.wrap(palette::HEADER, "Sync:"),
        head.sequence,
        styler.wrap(palette::mutation_state_style(head.state), head.state.text()),
        styler.wrap(palette::id_style(head.item_class), &head.target_display_id),
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
    unresolved: UnresolvedMutationCounts,
    styler: SubStyler,
) -> std::io::Result<()> {
    if rows.is_empty() {
        writeln!(stdout, "{}", empty_message(options))?;
    } else {
        let mut markers = MutationMarkers::default();
        for row in rows {
            if parent_is_in_rows(rows, row) {
                continue;
            }
            markers = markers.merge(render_row(stdout, row, "", styler)?);
            markers = markers.merge(render_children(stdout, rows, row, styler)?);
        }
        render_chrome(stdout, rows, markers, styler)?;
    }

    render_unresolved_counts(stdout, unresolved, styler)
}

/// The `Mutation Log:` trailer: how many **Unresolved Mutations** the
/// Repository Store holds, per state, or nothing at all when it holds none.
///
/// Writes the blank line that separates it from whatever precedes it, so
/// suppression stays atomic — no count, no stray separator. ARCHITECTURE.md
/// records both positions it takes.
///
/// Counts Promotion Mutations, which the row markers exclude (ADR-0041). It
/// has to: the `mutations` CHECK pairs `applying` with a Promotion, so a
/// count that dropped Promotions could never report that state at all.
///
/// Lives here rather than in [`render_chrome`] because `commands/search.rs`
/// shares that function, and a lookup returns the Items asked for and nothing
/// ambient (GLOSSARY.md).
fn render_unresolved_counts<W: Write + ?Sized>(
    stdout: &mut W,
    unresolved: UnresolvedMutationCounts,
    styler: SubStyler,
) -> std::io::Result<()> {
    if unresolved.total() == 0 {
        return Ok(());
    }

    // Ordered as `MutationState::ALL` and GLOSSARY.md's Unresolved Mutation
    // definition are, not failed-first like the row markers.
    let by_state = [
        (unresolved.pending, MutationState::Pending),
        (unresolved.failed, MutationState::Failed),
        (unresolved.applying, MutationState::Applying),
    ];
    let parts: Vec<String> = by_state
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, state)| {
            format!(
                "{count} {}",
                styler.wrap(palette::mutation_state_style(state), state.text())
            )
        })
        .collect();

    writeln!(stdout, "\nMutation Log: {}", parts.join(", "))
}

fn render_children<W: Write + ?Sized>(
    stdout: &mut W,
    rows: &[ListRow],
    parent: &ListRow,
    styler: SubStyler,
) -> std::io::Result<MutationMarkers> {
    let mut children = rows
        .iter()
        .filter(|child| child.container_id.as_deref() == Some(parent.id.as_str()))
        .peekable();
    let mut markers = MutationMarkers::default();
    while let Some(child) = children.next() {
        let prefix = if children.peek().is_none() {
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

fn empty_message(options: ListOptions<'_>) -> &'static str {
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
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::domain::item_class::ItemClass;
    use crate::domain::mutation_type::MutationType;
    use crate::render::Styler;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, TmpStore, commit_promotion, insert_dependency,
        insert_fixture_item, seed_mutation,
    };

    /// Drive `run` and frame any returned error as the dispatch seam does
    /// (ADR-0032: `tk list: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        run_rendered_with(h, Styler::plain(), args)
    }

    fn run_rendered_with(h: &mut Harness<'_>, styler: Styler, args: Args) -> Exit {
        let mut deps = h.deps_with(styler);
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "list", deps.styler.for_stderr());
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
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(stdout, "No open or active items.\n");
    }

    #[test]
    fn plain_list_marks_parked_and_triage_tickets_with_badges() {
        for (selection, priority, badge) in [
            ("parked", Some("P2"), "[parked]"),
            ("triage", None, "[triage]"),
        ] {
            let store = TmpStore::new("repo");
            let conn = seed_store(&store);
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id: "p1",
                    display: "tk-1",
                    title: "Held work",
                    selection_state: Some(selection),
                    priority,
                    created_seq: 1,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            drop(conn);

            let cwd_path = cwd();
            let mut h = Harness::new(&cwd_path, &store);
            expect_git(&h, &store);
            let code = run_rendered(&mut h, default_args());
            assert_eq!(code, Exit::Ok);
            let stdout = String::from_utf8(h.stdout).unwrap();
            assert!(stdout.contains(badge), "stdout={stdout:?}");
            assert!(stdout.contains("Held work"), "stdout={stdout:?}");
        }
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
        let mut h = Harness::new(&cwd_path, &store);
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
        let mut h = Harness::new(&cwd_path, &store);
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
        for (ready, local, expected) in [
            (false, false, "No epics.\n"),
            (true, false, "No ready items.\n"),
            (false, true, "No local epics.\n"),
        ] {
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
            let mut h = Harness::new(&cwd_path, &store);
            expect_git(&h, &store);
            let code = run_rendered(
                &mut h,
                Args {
                    epic: true,
                    ready,
                    local,
                    ..default_args()
                },
            );
            assert_eq!(code, Exit::Ok);
            assert_eq!(String::from_utf8(h.stdout).unwrap(), expected);
        }
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(stderr.contains("tk list: Repository Store not initialized; run 'tk init'"));
    }

    #[test]
    fn scope_to_a_done_epic_keeps_its_context_and_prints_a_hint() {
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
        let mut h = Harness::new(&cwd_path, &store);
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
        insta::assert_snapshot!(stdout, @"
        Scope: tk-1 (Epic + child Tickets)

        ✓ tk-1 [epic] Epic
        └── ○ tk-2 ● P2 Child
        --------------------------------------------------------------------------------
        Total: 2 items (1 open, 1 done)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked
        ");
        // tk-3 is a root Ticket outside the Epic: a failure here means Scope
        // stopped filtering and the hint is now lying about what is listed.
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
        let mut h = Harness::new(&cwd_path, &store);
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
    fn nested_child_row_reaches_the_legend_through_render_children() {
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
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(
            &conn,
            1,
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "child"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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

        Mutation Log: 1 pending
        ");
    }

    #[test]
    fn mutation_markers_pin_exact_byte_placement_and_spare_clean_rows() {
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
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "row-pending"),
        );
        seed_mutation(
            &conn,
            2,
            MutationState::Failed,
            FixtureMutation::new(MutationType::UpdateTicket, "row-failed"),
        );
        seed_mutation(
            &conn,
            3,
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "row-both"),
        );
        seed_mutation(
            &conn,
            4,
            MutationState::Failed,
            FixtureMutation::new(MutationType::SetItemStatus, "row-both"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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

        Mutation Log: 2 pending, 2 failed
        ");
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
        seed_mutation(
            &conn,
            1,
            MutationState::Failed,
            FixtureMutation {
                item_class: ItemClass::Epic,
                ..FixtureMutation::new(MutationType::UpdateEpic, "e1")
            },
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
        seed_mutation(
            &conn,
            1,
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "blocked"),
        );
        seed_mutation(
            &conn,
            2,
            MutationState::Failed,
            FixtureMutation::new(MutationType::SetItemStatus, "blocked"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
        assert!(
            line.ends_with("\x1b[22m"),
            "blocked dim must close after the title: {line:?}"
        );
    }

    #[test]
    fn pending_promotion_label_stays_distinct_from_queued_edit_markers() {
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
        seed_mutation(
            &conn,
            3,
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "promoted"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
            line_of("tk-1").contains(" ~ [pending promotion] Edit queued"),
            "a queued edit behind a Pending Promotion is genuinely unsent \
             and must still mark: {stdout:?}"
        );
        let promotion_only_line = line_of("tk-2");
        assert!(promotion_only_line.contains("[pending promotion] Promotion pending"));
        assert!(
            !promotion_only_line.contains('~') && !promotion_only_line.contains('\u{2691}'),
            "a Pending Promotion is the Item's own creation, not a queued \
             edit; marking it would conflate the two: {promotion_only_line:?}"
        );
    }

    #[test]
    fn orphaned_child_of_an_excluded_epic_still_reaches_the_legend() {
        // Origin filtering must not lose the child's markers when its Epic
        // is excluded and the child renders at top level.
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
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
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
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        seed_mutation(
            &conn,
            1,
            MutationState::Pending,
            FixtureMutation::new(MutationType::UpdateTicket, "child"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);

        let code = run_rendered(
            &mut h,
            Args {
                local: true,
                ..default_args()
            },
        );

        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("Done epic"), "stdout={stdout:?}");
        assert!(stdout.contains("Total: 1 item (1 open)"), "{stdout}");
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

    /// The banner fires only on a `failed` or `applying` head, so those are
    /// the only two states reachable here. `applying` is paired with a
    /// Promotion because the store's CHECK constraint admits no other
    /// Mutation Type into that state.
    #[test]
    fn sync_banner_styles_the_state_token() {
        for (state, mutation_type, sgr) in [
            (MutationState::Failed, MutationType::UpdateTicket, "91"),
            (MutationState::Applying, MutationType::PromoteTicket, "33"),
        ] {
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
            seed_mutation(&conn, 1, state, FixtureMutation::new(mutation_type, "t1"));
            drop(conn);

            let cwd_path = cwd();
            let mut h = Harness::new(&cwd_path, &store);
            expect_git(&h, &store);

            let code = run_rendered_with(&mut h, Styler::always(), default_args());

            assert_eq!(code, Exit::Ok);
            let stdout = String::from_utf8(h.stdout).unwrap();
            let stdout = stdout.lines().next().expect("sync banner");
            let want = format!("\u{1b}[{sgr}m{state}\u{1b}[39m");
            assert!(
                stdout.contains(&want),
                "banner state {state} should render as {want:?}: {stdout:?}"
            );
        }
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
        seed_mutation(
            &conn,
            1,
            MutationState::Applying,
            FixtureMutation::new(MutationType::PromoteTicket, "t1"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
    fn failed_promotion_has_a_binding_label_and_queue_banner() {
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
        seed_mutation(
            &conn,
            1,
            MutationState::Failed,
            FixtureMutation::new(MutationType::PromoteTicket, "t1"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        insta::assert_snapshot!(stdout, @"
        Sync: Mutation 1 failed on tk-1 (tk sync log 1)

        ○ tk-1 ● P2 [pending promotion] Row
        --------------------------------------------------------------------------------
        Total: 1 item (1 open)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked

        Mutation Log: 1 failed
        ");
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
        seed_mutation(
            &conn,
            1,
            MutationState::Failed,
            FixtureMutation::new(MutationType::UpdateTicket, "d1"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, default_args());
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(
            stdout,
            "Sync: Mutation 1 failed on tk-1 (tk sync log 1)\n\
             \n\
             No open or active items.\n\
             \n\
             Mutation Log: 1 failed\n"
        );
    }

    #[test]
    fn unresolved_count_reports_a_mutation_no_row_can_show() {
        for all_states in [false, true] {
            // The trailer must count Mutations hidden by the view, even when
            // a pending head prints no banner.
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
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id: "o1",
                    display: "tk-2",
                    title: "Open row",
                    created_seq: 2,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
            seed_mutation(
                &conn,
                1,
                MutationState::Pending,
                FixtureMutation::new(MutationType::UpdateTicket, "d1"),
            );
            if all_states {
                seed_mutation(
                    &conn,
                    2,
                    MutationState::Pending,
                    FixtureMutation::new(MutationType::UpdateTicket, "d1"),
                );
                seed_mutation(
                    &conn,
                    3,
                    MutationState::Failed,
                    FixtureMutation::new(MutationType::UpdateTicket, "d1"),
                );
                seed_mutation(
                    &conn,
                    4,
                    MutationState::Applying,
                    FixtureMutation::new(MutationType::PromoteTicket, "d1"),
                );
            }
            drop(conn);

            let cwd_path = cwd();
            let mut h = Harness::new(&cwd_path, &store);
            expect_git(&h, &store);
            let code = run_rendered(&mut h, default_args());
            assert_eq!(code, Exit::Ok);
            let stdout = String::from_utf8(h.stdout).unwrap();
            let counts = if all_states {
                "2 pending, 1 failed, 1 applying"
            } else {
                "1 pending"
            };
            assert_eq!(
                stdout,
                format!(
                    "○ tk-2 ● P2 Open row\n--------------------------------------------------------------------------------\nTotal: 1 item (1 open)\n\nStatus: ○ open  ◐ active  ✓ done\nBlocked: ⊘ blocked\n\nMutation Log: {counts}\n"
                )
            );
        }
    }

    #[test]
    fn queue_head_banner_renders_below_the_scope_hint_and_may_name_an_out_of_scope_item() {
        // The banner describes the whole Mutation Log, even under Scope.
        // Both banners must precede the tree with one blank line after them.
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
        seed_mutation(
            &conn,
            1,
            MutationState::Failed,
            FixtureMutation::new(MutationType::UpdateTicket, "loose"),
        );
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
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
        insta::assert_snapshot!(stdout, @"
        Scope: tk-1 (Epic + child Tickets)
        Sync: Mutation 1 failed on tk-3 (tk sync log 1)

        ○ tk-1 [epic] Epic
        └── ○ tk-2 ● P2 Child
        --------------------------------------------------------------------------------
        Total: 2 items (2 open)

        Status: ○ open  ◐ active  ✓ done
        Blocked: ⊘ blocked

        Mutation Log: 1 failed
        ");
    }

    #[test]
    fn scoped_empty_list_still_opens_on_the_empty_message_after_the_fence() {
        // One blank line must separate the Scope hint from an empty message.
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
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("99"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path, &store);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                epic_id: Some("tk-1".to_owned()),
                epic: true,
                local: true,
                ..default_args()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(
            stdout,
            "Scope: tk-1 (Epic + child Tickets)\n\nNo local epics.\n"
        );
    }
}
