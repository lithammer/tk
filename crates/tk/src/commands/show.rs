//! `tk show` — render one Ticket or Epic with current state.
//!
//! Layout:
//!
//! ```text
//! <status-glyph> <display-id> · <title>
//!   <P_> · <Kind> · Created: <created>[ · Updated: <updated>]    (Tickets)
//!   Epic · Created: <created>[ · Updated: <updated>]             (Epics)
//!   Selection: <triage|accepted|parked>                          (Tickets)
//!
//! DESCRIPTION
//! <body...>
//!
//! CLOSING REASON
//! <reason...>                                                    (done items)
//!
//! PARENT / TICKETS / BLOCKED BY / BLOCKING / EXTERNAL BLOCKERS
//!   <glyph> <status-glyph> <display-id>: [(Epic) ]<title>[ ● <priority>]
//!
//! FORMER BACKEND IDENTITIES
//!   • <backend-kind> <backend-key>
//!
//! UNRESOLVED MUTATIONS / WITHDRAWN MUTATIONS
//!   • <sequence> <state> <mutation-type>
//!   Inspect with 'tk sync log <sequence>'.
//! ```
//!
//! Empty sections are omitted. Output ends with a single trailing newline.
//! The status word and Origin row are intentionally dropped — both
//! duplicate information already carried by the glyph and Display ID
//! shape (ADR-0014 anti-drift; the v1 single-Remote invariant lets the
//! Backend kind ride on the Display ID prefix).

use std::io::Write;

use clap::Args as ClapArgs;

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::item_header::{self, Header};
use crate::commands::resolver;
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_state::MutationState;
use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;
use crate::store::repository::show::{
    self, ExternalBlockerSummary, ItemDetail, ItemMutation, ItemSummary,
};

/// Flags for `tk show`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Display ID or Alias of the Ticket or Epic to render.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Run `tk show <id>`. On failure returns the [`CommandError`] for the dispatch
/// seam to frame as `tk show:` (ADR-0032); on success returns the process
/// [`Exit`].
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    let detail = match show::show_item(&store, &args.id) {
        Ok(Some(detail)) => detail,
        // The not-found phrasing is `tk show`'s own, so it lives here rather
        // than in a shared resolver helper (the body carries no prefix).
        Ok(None) => {
            return Err(CommandError::failure(format!(
                "'{id}' is not a known Display ID or Alias",
                id = args.id
            )));
        }
        Err(err) => return Err(resolver::storage_error(&err)),
    };

    let sub = deps.styler.for_stdout();
    if let Err(err) = render(deps.stdout, &detail, sub) {
        // A closed pager (`tk show | head`) is success; other write errors are
        // a diagnosed failure (shared policy).
        return cli::write_error(&err);
    }
    Ok(Exit::Ok)
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

    // Selection State (ADR-0027) renders as its own line under the facet bar,
    // not in the shared `item_header` facet bar — that header is the verbatim
    // show/grep single-source, and `tk grep` is content search where Selection
    // State is noise. `None` (Epics) omits the line.
    if let Some(selection) = detail.selection_state {
        writeln!(stdout, "  Selection: {}", selection.text())?;
    }

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
            render_sub_row(stdout, "\u{2190}", item, styler)?;
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
        has_section = true;
    }

    if !detail.former_backend_identities.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "FORMER BACKEND IDENTITIES")?;
        for identity in &detail.former_backend_identities {
            writeln!(
                stdout,
                "  \u{2022} {} {}",
                identity.backend_kind, identity.backend_key
            )?;
        }
        has_section = true;
    }

    // Exhaustive over MutationState rather than a terminality predicate:
    // applied is terminal too, so such a predicate would only be correct
    // because rendering happens to drop applied elsewhere. Writing out all
    // seven variants means a state added later fails to compile here instead
    // of silently landing in whichever bucket the predicate picked.
    let mut unresolved = Vec::new();
    let mut withdrawn = Vec::new();
    for mutation in &detail.mutations {
        match mutation.state {
            MutationState::Pending | MutationState::Failed | MutationState::Applying => {
                unresolved.push(mutation);
            }
            MutationState::Skipped | MutationState::Cancelled | MutationState::Abandoned => {
                withdrawn.push(mutation);
            }
            MutationState::Applied => {}
        }
    }

    if !unresolved.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "UNRESOLVED MUTATIONS")?;
        for mutation in &unresolved {
            render_item_mutation(stdout, mutation, styler)?;
        }
        has_section = true;
    }

    if !withdrawn.is_empty() {
        if has_section {
            stdout.write_all(b"\n")?;
        }
        write_section_header(stdout, styler, "WITHDRAWN MUTATIONS")?;
        for mutation in &withdrawn {
            render_item_mutation(stdout, mutation, styler)?;
        }
    }

    // One hint below whichever of the two Mutation sections rendered last,
    // not one per section — a reader who sees both does not need to be told
    // twice how to inspect a row.
    if !unresolved.is_empty() || !withdrawn.is_empty() {
        writeln!(
            stdout,
            "  {}",
            styler.wrap(palette::SEPARATOR, "Inspect with 'tk sync log <sequence>'.")
        )?;
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

/// Sub-row bullet for the sections that list plain values rather than
/// related Items, written as bytes so the escape stays out of the two call
/// sites: `"  • "`.
const BULLET: &[u8] = b"  \xe2\x80\xa2 ";

fn render_external_blocker<W: Write + ?Sized>(
    stdout: &mut W,
    eb: &ExternalBlockerSummary,
) -> std::io::Result<()> {
    stdout.write_all(BULLET)?;
    sanitize::write_sanitized_line(stdout, eb.reason.as_bytes())?;
    stdout.write_all(b"\n")
}

/// Render one Mutation sub-row inside an UNRESOLVED or WITHDRAWN section.
///
/// The state token carries `palette::mutation_state_style`, the same entry
/// the row glyphs in `tk list`, the `tk list` banner, and `tk sync log` use
/// for that state — so one Mutation cannot read as two different things
/// depending on which command the reader ran. The row renders after the
/// section header's bold span has closed, so its foreground colour nests
/// inside nothing (ADR-0014).
fn render_item_mutation<W: Write + ?Sized>(
    stdout: &mut W,
    mutation: &ItemMutation,
    styler: SubStyler,
) -> std::io::Result<()> {
    stdout.write_all(BULLET)?;
    writeln!(
        stdout,
        "{} {} {}",
        mutation.sequence,
        styler.wrap(
            palette::mutation_state_style(mutation.state),
            mutation.state.text()
        ),
        mutation.mutation_type
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::repository::show::FormerBackendIdentity;
    use crate::store::testing::{
        FixtureItem, FixtureMutation, TmpStore, insert_fixture_item, insert_fixture_mutation,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    /// `ItemDetail` with every relationship empty and `mutations` supplied by
    /// the caller, for tests that vary only the Mutation section. Adding a
    /// field to `ItemDetail` should only require touching one full literal —
    /// `render_pins_every_section_header_glyph_and_order`'s, which is
    /// exhaustive on purpose — not this one too.
    fn minimal_item_detail(mutations: Vec<ItemMutation>) -> ItemDetail {
        ItemDetail {
            display_id: "tk-1".into(),
            item_class: ItemClass::Ticket,
            ticket_kind: Some(crate::domain::ticket_kind::TicketKind::Task),
            priority: Some(crate::domain::priority::Priority::P2),
            selection_state: Some(crate::domain::selection_state::SelectionState::Accepted),
            title: "Ticket".into(),
            body: String::new(),
            closing_reason: None,
            status: crate::domain::status::ItemStatus::Open,
            created_at: "2026-05-09T00:00:00.000Z".into(),
            updated_at: "2026-05-09T00:00:00.000Z".into(),
            parent: None,
            children: Vec::new(),
            blocked_by: Vec::new(),
            blocking: Vec::new(),
            external_blockers: Vec::new(),
            former_backend_identities: Vec::new(),
            mutations,
        }
    }

    /// A stdout that fails every write with `BrokenPipe`, modelling a closed
    /// pager (`tk show ID | head`).
    struct BrokenPipe;
    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "broken pipe",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Drive `run` and frame any returned error exactly as the dispatch seam
    /// does (ADR-0032: `tk show: <body>`), so a test asserts the framed bytes
    /// the user sees. A success passes its `Exit` straight through, writing no
    /// stderr.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "show");
                exit
            }
        }
    }

    #[test]
    fn broken_pipe_to_stdout_is_success() {
        // `tk show ID | head` (reader quits early) is success, not Failure: the
        // shared write-error policy maps a broken pipe to Exit::Ok.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let runner = FakeRunner::new();
        runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = StdRng::seed_from_u64(0);
        let mut stdout = BrokenPipe;
        let mut stderr: Vec<u8> = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut deps = Deps {
            stdout: &mut stdout,
            stderr: &mut stderr,
            stdin: &mut stdin,
            runner: &runner,
            clock: &clock,
            rng: &mut rng,
            cwd: &cwd_path,
            styler: Styler::plain(),
        };
        let code = run(&mut deps, Args { id: "tk-1".into() })
            .expect("a broken pipe is success, not an error");
        assert_eq!(code, Exit::Ok, "a broken pipe is success, not failure");
        assert!(
            stderr.is_empty(),
            "broken pipe writes no diagnostic: {stderr:?}"
        );
    }

    #[test]
    fn missing_store_renders_init_diagnostic() {
        let store = TmpStore::new("repo");
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
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
        let code = run_rendered(
            &mut h,
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
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
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
        // Selection State (ADR-0027) renders on its own line directly under the
        // facet bar; a normal ticket is accepted.
        assert!(
            stdout.contains("\u{b7} Created: 2026-05-09\n  Selection: accepted\n"),
            "stdout={stdout:?}"
        );
    }

    #[test]
    fn renders_triage_ticket_without_a_priority_token() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Investigate flake",
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
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // The facet bar opens at the Kind — no Priority token — and the
        // Selection line carries the triage cue (ADR-0027).
        assert!(
            stdout.contains("  Task \u{b7} Created: 2026-05-09\n  Selection: triage\n"),
            "stdout={stdout:?}"
        );
        assert!(
            !stdout.contains('\u{25cf}'),
            "no priority bullet: {stdout:?}"
        );
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
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
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
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Facet bar carries the capitalized `Epic` token (asserted on the
        // leading `  Epic \u{b7}` so the Epic title can't satisfy it by chance).
        assert!(
            stdout.contains("  Epic \u{b7} Created: 2026-05-09"),
            "stdout={stdout:?}"
        );
        // Epics stay outside Selection State (ADR-0027): no Selection line.
        assert!(
            !stdout.contains("Selection:"),
            "Epics omit Selection State: stdout={stdout:?}"
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
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
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
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
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
        let _ = run_rendered(&mut h, Args { id: "tk-1".into() });
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(!stdout.contains("CLOSING REASON"), "stdout={stdout:?}");
    }

    /// A deliberately maximal `ItemDetail` — one Item carrying every section at
    /// once — rendered through `render()` so a single inline snapshot pins each
    /// section header, its sub-row glyph, and the section ordering. The
    /// directional glyphs are the contract: PARENT `↑`, TICKETS `↓`, BLOCKED BY
    /// `→`, BLOCKING `←` (the two blocker relationships read as opposite
    /// directions, mirroring the `↑`/`↓` parent/child pair). EXTERNAL BLOCKERS is
    /// Backend-Adapter-sourced and unreachable from the CLI, so this direct
    /// `render()` call is the only path that covers it. Realism is not the point
    /// — a real Ticket would not carry both a parent and children — coverage of
    /// every layout branch is.
    #[test]
    fn render_pins_every_section_header_glyph_and_order() {
        let summary = |display_id: &str, title: &str, class, status, priority| ItemSummary {
            display_id: display_id.into(),
            title: title.into(),
            item_class: class,
            status,
            priority,
        };
        let detail = ItemDetail {
            display_id: "tk-1".into(),
            item_class: ItemClass::Ticket,
            ticket_kind: Some(crate::domain::ticket_kind::TicketKind::Task),
            priority: Some(crate::domain::priority::Priority::P1),
            selection_state: Some(crate::domain::selection_state::SelectionState::Accepted),
            title: "Wire the thing".into(),
            body: "First line.\nSecond line.".into(),
            closing_reason: Some("Shipped in PR #42".into()),
            status: crate::domain::status::ItemStatus::Done,
            created_at: "2026-05-09T00:00:00.000Z".into(),
            updated_at: "2026-05-29T12:00:00.000Z".into(),
            parent: Some(summary(
                "tk-9",
                "Parent epic",
                ItemClass::Epic,
                crate::domain::status::ItemStatus::Open,
                None,
            )),
            children: vec![summary(
                "tk-2",
                "Child ticket",
                ItemClass::Ticket,
                crate::domain::status::ItemStatus::Open,
                Some(crate::domain::priority::Priority::P2),
            )],
            blocked_by: vec![summary(
                "tk-3",
                "Prerequisite",
                ItemClass::Ticket,
                crate::domain::status::ItemStatus::Done,
                Some(crate::domain::priority::Priority::P0),
            )],
            blocking: vec![summary(
                "tk-4",
                "Downstream work",
                ItemClass::Ticket,
                crate::domain::status::ItemStatus::Active,
                Some(crate::domain::priority::Priority::P3),
            )],
            external_blockers: vec![ExternalBlockerSummary {
                reason: "WAITING-ON-123: upstream fix".into(),
            }],
            former_backend_identities: vec![FormerBackendIdentity {
                backend_kind: crate::domain::backend_kind::BackendKind::Github,
                backend_key: "https://github.com/o/r/issues/42".into(),
            }],
            mutations: vec![
                ItemMutation {
                    sequence: 7,
                    state: MutationState::Failed,
                    mutation_type: crate::domain::mutation_type::MutationType::UpdateTicket,
                },
                ItemMutation {
                    sequence: 9,
                    state: MutationState::Pending,
                    mutation_type: crate::domain::mutation_type::MutationType::SetItemStatus,
                },
                ItemMutation {
                    sequence: 4,
                    state: MutationState::Skipped,
                    mutation_type: crate::domain::mutation_type::MutationType::AddDependency,
                },
            ],
        };

        let mut out = Vec::new();
        render(&mut out, &detail, Styler::plain().for_stdout()).unwrap();
        insta::assert_snapshot!(String::from_utf8(out).unwrap(), @"
        ✓ tk-1 · Wire the thing
          P1 · Task · Created: 2026-05-09 · Updated: 2026-05-29
          Selection: accepted

        DESCRIPTION
        First line.
        Second line.

        CLOSING REASON
        Shipped in PR #42

        PARENT
          ↑ ○ tk-9: (Epic) Parent epic

        TICKETS
          ↓ ○ tk-2: Child ticket ● P2

        BLOCKED BY
          → ✓ tk-3: Prerequisite ● P0

        BLOCKING
          ← ◐ tk-4: Downstream work ● P3

        EXTERNAL BLOCKERS
          • WAITING-ON-123: upstream fix

        FORMER BACKEND IDENTITIES
          • github https://github.com/o/r/issues/42

        UNRESOLVED MUTATIONS
          • 7 failed update_ticket
          • 9 pending set_item_status

        WITHDRAWN MUTATIONS
          • 4 skipped add_dependency
          Inspect with 'tk sync log <sequence>'.
        ");
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
        let _ = run_rendered(&mut h, Args { id: "tk-1".into() });
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert!(stdout.contains("DESCRIPTION"));
        assert!(stdout.contains("Multi-line\nbody\n"));
    }

    /// Covers every state a Mutation section can render. `applied` appears
    /// in neither, which
    /// `render_omits_mutation_sections_when_every_mutation_is_applied` pins.
    #[test]
    fn mutation_rows_style_every_rendered_state_when_colour_is_forced() {
        // One list, used to seed the rows and then to assert them, so a state
        // cannot be rendered without also being checked.
        let expected = [
            (MutationState::Pending, "90"),
            (MutationState::Failed, "91"),
            (MutationState::Applying, "33"),
            (MutationState::Skipped, "90"),
            (MutationState::Cancelled, "90"),
            (MutationState::Abandoned, "35"),
        ];
        let detail = minimal_item_detail(
            expected
                .iter()
                .enumerate()
                .map(|(index, (state, _))| ItemMutation {
                    sequence: i64::try_from(index).expect("fixture index fits i64") + 1,
                    state: *state,
                    mutation_type: crate::domain::mutation_type::MutationType::UpdateTicket,
                })
                .collect(),
        );

        let mut out = Vec::new();
        render(&mut out, &detail, Styler::always().for_stdout()).unwrap();
        let stdout = String::from_utf8(out).unwrap();

        for (state, sgr) in expected {
            let want = format!("\u{1b}[{sgr}m{state}\u{1b}[39m");
            assert!(
                stdout.contains(&want),
                "{state} should render as {want:?}: {stdout:?}"
            );
        }
    }

    #[test]
    fn render_omits_mutation_sections_when_every_mutation_is_applied() {
        // The store returns `applied` rows (store-level
        // `applied_mutations_come_back_from_the_read` proves it), so this
        // empty output can only be the renderer's `Applied` match arm at
        // work, not a filtered-away query result.
        let detail = minimal_item_detail(vec![ItemMutation {
            sequence: 1,
            state: MutationState::Applied,
            mutation_type: crate::domain::mutation_type::MutationType::UpdateTicket,
        }]);

        let mut out = Vec::new();
        render(&mut out, &detail, Styler::plain().for_stdout()).unwrap();
        let stdout = String::from_utf8(out).unwrap();
        assert!(!stdout.contains("MUTATIONS"), "stdout={stdout:?}");
        assert!(!stdout.contains("Inspect with"), "stdout={stdout:?}");
    }

    #[test]
    fn run_renders_unresolved_and_withdrawn_mutations_read_from_the_store() {
        // The other Mutation-section tests build `ItemDetail` by hand; this
        // one drives `show::run` end to end so the new SQL read is covered,
        // not just the renderer's partition of an already-typed `Vec`.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Ticket with mutations",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 7,
                mutation_type: "update_ticket",
                item_id: "t1",
                item_class: "ticket",
                state: "failed",
                failure_json: Some(r#"{"detail":"boom"}"#),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "add_dependency",
                item_id: "t1",
                item_class: "ticket",
                state: "skipped",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, Args { id: "tk-1".into() });
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        insta::assert_snapshot!(stdout, @"
        ○ tk-1 · Ticket with mutations
          P2 · Task · Created: 2026-05-09
          Selection: accepted
        UNRESOLVED MUTATIONS
          • 7 failed update_ticket

        WITHDRAWN MUTATIONS
          • 4 skipped add_dependency
          Inspect with 'tk sync log <sequence>'.
        ");
    }
}
