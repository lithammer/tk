//! `tk grep` — regex content search over title and body, rendered as
//! `tk show`-style match context (ADR-0026).

use std::borrow::Cow;
use std::io::Write;
use std::ops::ControlFlow;

use clap::Args as ClapArgs;
use regex::{Regex, RegexBuilder};

use crate::cli::{self, CommandError, Deps, Exit};
use crate::commands::item_header::{self, Header};
use crate::commands::resolver;
use crate::render::highlight;
use crate::render::palette;
use crate::render::styler::SubStyler;
use crate::store::repository::grep::{self, GrepItem, ScanError};

/// Flags for `tk grep`.
///
/// Four `bool`s exceed pedantic's `struct_excessive_bools` cap, but each is an
/// independent grep flag clap's derive must own as its own field; only `-c` and
/// `-q` conflict, leaving the rest freely composable. The allow mirrors
/// `tk list`'s Args for the same parser-layer reason.
#[derive(Debug, Default, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Regular expression to search for in title and body text.
    #[arg(value_name = "PATTERN")]
    pub pattern: String,

    /// Match case-insensitively (full Unicode case folding).
    #[arg(short = 'i', long = "ignore-case")]
    pub ignore_case: bool,

    /// Match the pattern as a literal string, not a regular expression.
    #[arg(short = 'F', long = "fixed-strings")]
    pub fixed: bool,

    /// Show N lines of context on each side of a match (default 3).
    #[arg(short = 'C', long = "context", value_name = "N")]
    pub context: Option<usize>,

    /// Show N lines of context after each match (overrides -C).
    #[arg(short = 'A', long = "after-context", value_name = "N")]
    pub after_context: Option<usize>,

    /// Show N lines of context before each match (overrides -C).
    #[arg(short = 'B', long = "before-context", value_name = "N")]
    pub before_context: Option<usize>,

    /// Suppress all output and exit on the first match.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Print the count of matching items instead of the match blocks.
    #[arg(short = 'c', long = "count", conflicts_with = "quiet")]
    pub count: bool,
}

/// Compile the search pattern into a matcher, applying the literal-match and
/// case-folding policies. The pattern is a regular expression by default
/// (ADR-0026); `-F` escapes its metacharacters so it matches verbatim, and
/// `-i` compiles case-insensitively (full Unicode case folding) rather than
/// lowercasing. The two compose: `-F` escapes, then `-i` folds.
fn build_matcher(pattern: &str, ignore_case: bool, fixed: bool) -> Result<Regex, regex::Error> {
    let pattern: Cow<'_, str> = if fixed {
        Cow::Owned(regex::escape(pattern))
    } else {
        Cow::Borrowed(pattern)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .build()
}

/// Run `tk grep <pattern>`. On failure returns the [`CommandError`] for the
/// dispatch seam to frame as `tk grep:` (ADR-0032); a clean run returns
/// [`Exit::Ok`] / [`Exit::NoMatch`] per the grep 0/1 overload (ADR-0026).
pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    // Reject only a truly empty pattern before any store work, mirroring
    // `tk search` (ADR-0026, amended): an empty regex matches every line and
    // would dump the whole store. A whitespace pattern is a valid (if unusual)
    // needle — `grep`/`ripgrep` match it — so it is not rejected. An empty or
    // malformed pattern is a usage error (exit 2), surfaced fail-fast.
    if args.pattern.is_empty() {
        return Err(CommandError::usage("pattern must not be empty"));
    }

    // Compile (and so validate) the pattern before opening the store.
    let re = build_matcher(&args.pattern, args.ignore_case, args.fixed)
        .map_err(|err| CommandError::usage(format!("invalid pattern: {err}")))?;

    let store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;

    // Resolve the per-side context window: `-A`/`-B` override their side of
    // `-C`, which in turn overrides the default-3 (ADR-0026).
    let before = args
        .before_context
        .or(args.context)
        .unwrap_or(DEFAULT_CONTEXT);
    let after = args
        .after_context
        .or(args.context)
        .unwrap_or(DEFAULT_CONTEXT);

    // Stream Items in creation order, rendering each match straight to stdout
    // (one Item in memory). `matched` drives the grep-style 0/1 exit overload;
    // `count` accumulates the matching-item total for `-c`.
    let out = deps.styler.for_stdout();
    let stdout = &mut *deps.stdout;
    let mut matched = false;
    let mut count: usize = 0;
    let scan = grep::scan(&store, |item| {
        // `-q` suppresses output and stops at the first match: the 0/1 exit
        // overload still carries the answer, so no block is rendered (ADR-0026).
        if args.quiet {
            if item_matches(&item, &re) {
                matched = true;
                return Ok(ControlFlow::Break(()));
            }
            return Ok(ControlFlow::Continue(()));
        }
        // `-c` tallies matching items rather than rendering them; an item that
        // matches on several lines still counts once (the unit is the item).
        if args.count {
            if item_matches(&item, &re) {
                matched = true;
                count += 1;
            }
            return Ok(ControlFlow::Continue(()));
        }
        render_match(stdout, &item, &re, &mut matched, before, after, out)?;
        Ok(ControlFlow::Continue(()))
    });
    match scan {
        Ok(()) => {}
        Err(ScanError::Sql(err)) => return Err(resolver::storage_error(&err)),
        // A write only happens after a block has started, so a broken pipe
        // (`tk grep … | head`) means matches WERE found and piped: the shared
        // policy reports Ok for that and a diagnosed Failure for any other write
        // error (so its exit `1` is never misread as a no-match).
        Err(ScanError::Write(err)) => return cli::write_error(&err).map_or(Ok(Exit::Ok), Err),
    }

    // `-c` always prints a number (even `0`, like `grep -c`); the 0/1 exit
    // overload below still reports whether anything matched.
    if args.count {
        if let Err(err) = writeln!(stdout, "{count}") {
            return cli::write_error(&err).map_or(Ok(Exit::Ok), Err);
        }
    }

    Ok(if matched { Exit::Ok } else { Exit::NoMatch })
}

/// Default `grep -C` context window, in lines each side of a match (matching
/// `git diff -U3`, ADR-0026), used when neither `-C` nor `-A`/`-B` is given.
const DEFAULT_CONTEXT: usize = 3;

/// Whether the pattern hits an Item at all — the title or any (non-empty) body
/// line. Mirrors [`render_match`]'s "title or body line" rule but yields only a
/// predicate, so `-q` can answer yes/no without materialising any hunk.
fn item_matches(item: &GrepItem, re: &Regex) -> bool {
    re.is_match(&item.title)
        || (!item.body.is_empty() && item.body.split('\n').any(|line| re.is_match(line)))
}

/// Render one Item's matches as a `tk show`-style block, or nothing when the
/// pattern hits neither title nor body.
///
/// The title is matched and rendered on the label line; body matches drive the
/// `DESCRIPTION` hunks. A title-only hit therefore renders just the label (no
/// body hunk), and the body context windows never absorb the title line.
fn render_match<W: Write + ?Sized>(
    stdout: &mut W,
    item: &GrepItem,
    re: &Regex,
    matched: &mut bool,
    before: usize,
    after: usize,
    styler: SubStyler,
) -> std::io::Result<()> {
    let title_hit = re.is_match(&item.title);

    // An empty body is not one empty line: `"".split('\n')` would yield a
    // single "" the pattern could never usefully match.
    let body_lines: Vec<&str> = if item.body.is_empty() {
        Vec::new()
    } else {
        item.body.split('\n').collect()
    };
    let body_hits: Vec<usize> = body_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| re.is_match(line))
        .map(|(i, _)| i)
        .collect();

    if !title_hit && body_hits.is_empty() {
        return Ok(());
    }
    // A blank line separates consecutive item blocks (not before the first).
    // `*matched` is already true once an earlier block has been written.
    if *matched {
        stdout.write_all(b"\n")?;
    }
    *matched = true;

    // Label line + facet bar, shared verbatim with `tk show` (ADR-0026). No
    // `DESCRIPTION` header: the hunks below are a collapsed view, not the body.
    item_header::render_header(
        stdout,
        &Header {
            status: item.status,
            display_id: &item.display_id,
            item_class: item.item_class,
            title: &item.title,
            priority: item.priority,
            ticket_kind: item.ticket_kind,
            created_at: &item.created_at,
            updated_at: &item.updated_at,
        },
        Some(re),
        styler,
    )?;

    for (idx, hunk) in context_hunks(&body_hits, body_lines.len(), before, after)
        .into_iter()
        .enumerate()
    {
        // A cyan `--` between non-contiguous hunks, like grep -C (the separator
        // colour is policy-gated, so a pipe still sees a bare `--`).
        if idx > 0 {
            writeln!(stdout, "{}", styler.wrap(palette::HUNK_SEPARATOR, "--"))?;
        }
        for line in &body_lines[hunk] {
            stdout.write_all(b"  ")?;
            highlight::write_highlighted_line(stdout, line, re, styler)?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// Expand each body-line hit to a `[hit - before, hit + after]` window (clamped
/// to the body) and merge windows that overlap or touch, yielding inclusive line
/// ranges in document order. Touching windows merge so adjacent matches read as
/// one hunk. `before == after == 0` collapses each hunk to its matching line.
fn context_hunks(
    hits: &[usize],
    len: usize,
    before: usize,
    after: usize,
) -> Vec<std::ops::RangeInclusive<usize>> {
    let mut hunks: Vec<std::ops::RangeInclusive<usize>> = Vec::new();
    for &hit in hits {
        let start = hit.saturating_sub(before);
        // Saturating, mirroring `start`: a huge `-A`/`-C` (clap accepts up to
        // usize::MAX) must clamp to the last line, not overflow `hit + after`.
        let end = hit.saturating_add(after).min(len.saturating_sub(1));
        match hunks.last_mut() {
            // `start <= prev_end + 1` covers both overlap and adjacency.
            Some(last) if start <= *last.end() + 1 => {
                *last = *last.start()..=end.max(*last.end());
            }
            _ => hunks.push(start..=end),
        }
    }
    hunks
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
    /// (ADR-0032: `tk grep: <body>`). A success — including the `Exit::NoMatch`
    /// half of grep's 0/1 overload — passes through, writing no stderr.
    fn run_rendered(h: &mut Harness<'_>, args: Args) -> Exit {
        run_rendered_with(h, Styler::plain(), args)
    }

    /// [`run_rendered`] with an explicit `Styler` so the colour-output tests can
    /// exercise `Styler::always()`.
    fn run_rendered_with(h: &mut Harness<'_>, styler: Styler, args: Args) -> Exit {
        let mut deps = h.deps_with(styler);
        match run(&mut deps, args) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "grep");
                exit
            }
        }
    }

    #[test]
    fn body_match_renders_a_show_style_block() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Add middleware",
                body: "The handler uses the auth token to authorize.",
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
                pattern: "auth".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Label line carries the Display ID; the matching body line is shown.
        assert!(stdout.contains("tk-1"), "stdout={stdout:?}");
        assert!(stdout.contains("uses the auth token"), "stdout={stdout:?}");
    }

    #[test]
    fn no_match_exits_one_with_empty_streams() {
        // grep's 0/1 predicate (ADR-0026): a no-match is NoMatch (exit 1), not a
        // failure, and writes nothing to either stream — empty stderr is how a
        // script distinguishes "no match" from "broken".
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Unrelated chore",
                body: "Nothing to see here.",
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
                pattern: "nonexistent".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::NoMatch);
        assert!(h.stdout.is_empty(), "stdout={:?}", h.stdout);
        assert!(h.stderr.is_empty(), "stderr={:?}", h.stderr);
    }

    #[test]
    fn body_match_shows_three_lines_of_context_each_side() {
        // ADR-0026 fixes the default context at 3 (matching `git diff -U3`).
        // A match on body line index 5 shows indices [2, 8] and excludes the
        // lines just outside that window.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        let body = "alpha\nbravo\ncharlie\ndelta\necho\nMATCHHERE\nfoxtrot\ngolf\nhotel\nindia";
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body,
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
                pattern: "MATCHHERE".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        for shown in [
            "charlie",
            "delta",
            "echo",
            "MATCHHERE",
            "foxtrot",
            "golf",
            "hotel",
        ] {
            assert!(stdout.contains(shown), "expected {shown:?} in {stdout:?}");
        }
        for hidden in ["alpha", "bravo", "india"] {
            assert!(
                !stdout.contains(hidden),
                "did not expect {hidden:?} in {stdout:?}"
            );
        }
    }

    #[test]
    fn block_carries_the_show_facet_bar_without_a_description_header() {
        // ADR-0026: a grep block is the `tk show` label line + facet bar, then
        // the collapsed hunks — but no `DESCRIPTION` header (noise for hunks).
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body: "match here",
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
                pattern: "match".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let stdout = String::from_utf8(h.stdout).unwrap();
        // Facet bar identical to `tk show`'s for a default Ticket.
        assert!(
            stdout.contains("  P2 \u{b7} Task \u{b7} Created: 2026-05-09\n"),
            "stdout={stdout:?}"
        );
        assert!(!stdout.contains("DESCRIPTION"), "stdout={stdout:?}");
    }

    /// A 10-line body with the needle on index 5, shared by the context-window
    /// tests so each can name exactly which lines a window includes or omits.
    const CONTEXT_BODY: &str =
        "alpha\nbravo\ncharlie\ndelta\necho\nMATCHHERE\nfoxtrot\ngolf\nhotel\nindia";

    #[test]
    fn context_flag_narrows_the_window_each_side() {
        // tk-118: `-C 1` overrides the default 3, so only one line each side of
        // the index-5 hit shows.
        let (code, out) = grep_one_args(
            "Subject",
            CONTEXT_BODY,
            Args {
                pattern: "MATCHHERE".to_owned(),
                context: Some(1),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        for shown in ["echo", "MATCHHERE", "foxtrot"] {
            assert!(out.contains(shown), "expected {shown:?} in {out:?}");
        }
        for hidden in ["delta", "golf"] {
            assert!(
                !out.contains(hidden),
                "did not expect {hidden:?} in {out:?}"
            );
        }
    }

    #[test]
    fn context_zero_shows_only_the_matching_line() {
        // tk-118: `-C 0` collapses the hunk to the matching line — no context.
        let (code, out) = grep_one_args(
            "Subject",
            CONTEXT_BODY,
            Args {
                pattern: "MATCHHERE".to_owned(),
                context: Some(0),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("MATCHHERE"), "out={out:?}");
        for hidden in ["echo", "foxtrot"] {
            assert!(
                !out.contains(hidden),
                "did not expect {hidden:?} in {out:?}"
            );
        }
    }

    #[test]
    fn after_and_before_context_override_the_context_flag() {
        // tk-118: `-A`/`-B` win over `-C` on their own side. With `-C 5 -A 1`,
        // before takes 5 (from -C) but after takes 1 (from -A), not 5 — so the
        // window is [0, 6]: `alpha` shows (before=5) while `golf` does not
        // (after=1, not 5). A reversed precedence would fail this.
        let (code, out) = grep_one_args(
            "Subject",
            CONTEXT_BODY,
            Args {
                pattern: "MATCHHERE".to_owned(),
                context: Some(5),
                after_context: Some(1),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        for shown in ["alpha", "echo", "MATCHHERE", "foxtrot"] {
            assert!(out.contains(shown), "expected {shown:?} in {out:?}");
        }
        assert!(
            !out.contains("golf"),
            "-A 1 must override -C 5 on the after side: {out:?}"
        );
    }

    #[test]
    fn a_huge_after_context_clamps_instead_of_overflowing() {
        // tk-118 regression: `-A usize::MAX` on a match past line 0 must clamp to
        // the last line, not overflow `hit + after` (a debug-build panic / release
        // silent-wrong window). The match is on index 5, so the window runs to the
        // body end and the last line shows.
        let (code, out) = grep_one_args(
            "Subject",
            CONTEXT_BODY,
            Args {
                pattern: "MATCHHERE".to_owned(),
                after_context: Some(usize::MAX),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(
            out.contains("india"),
            "window should reach the last line: {out:?}"
        );
    }

    #[test]
    fn after_and_before_context_override_each_side() {
        // tk-118: `-B 1 -A 2` is asymmetric — one line before, two after the
        // index-5 hit — proving the two sides resolve independently.
        let (code, out) = grep_one_args(
            "Subject",
            CONTEXT_BODY,
            Args {
                pattern: "MATCHHERE".to_owned(),
                before_context: Some(1),
                after_context: Some(2),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        for shown in ["echo", "MATCHHERE", "foxtrot", "golf"] {
            assert!(out.contains(shown), "expected {shown:?} in {out:?}");
        }
        for hidden in ["delta", "hotel"] {
            assert!(
                !out.contains(hidden),
                "did not expect {hidden:?} in {out:?}"
            );
        }
    }

    /// Run `tk grep PATTERN` against a single seeded Ticket, returning
    /// `(exit, stdout)`. Keeps the matching-semantics tests focused on behavior.
    fn grep_one(title: &str, body: &str, pattern: &str) -> (Exit, String) {
        grep_one_args(
            title,
            body,
            Args {
                pattern: pattern.to_owned(),
                ..Args::default()
            },
        )
    }

    /// Like [`grep_one`] but takes the full [`Args`], so a test can exercise a
    /// flag (e.g. `-i`) without restating the single-item seeding.
    fn grep_one_args(title: &str, body: &str, args: Args) -> (Exit, String) {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title,
                body,
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, args);
        (code, String::from_utf8(h.stdout).unwrap())
    }

    #[test]
    fn pattern_is_a_real_regex_not_a_literal() {
        // ADR-0026: regex by default. Alternation matches; a metacharacter-free
        // pattern degrades to a plain substring (same recall as literal).
        let (code, out) = grep_one("Subject", "please FIXME this soon", "TODO|FIXME");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("FIXME"), "out={out:?}");

        let (code, out) = grep_one("Subject", "a plain word here", "plain");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("a plain word here"), "out={out:?}");
    }

    #[test]
    fn matching_is_case_sensitive_by_default() {
        // ADR-0026: case-sensitive by default (deliberate divergence from
        // `tk search`); `-i` flips it. `Auth` must not match `auth`.
        let (code, out) = grep_one("Subject", "the auth token", "Auth");
        assert_eq!(code, Exit::NoMatch);
        assert!(out.is_empty(), "out={out:?}");
    }

    #[test]
    fn ignore_case_matches_across_case() {
        // tk-117: `-i` flips the case-sensitive default for one invocation, so
        // the capitalised pattern hits the lowercase body.
        let (code, out) = grep_one_args(
            "Subject",
            "the auth token",
            Args {
                pattern: "Auth".to_owned(),
                ignore_case: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("the auth token"), "out={out:?}");
    }

    #[test]
    fn ignore_case_folds_beyond_ascii() {
        // ADR-0026: `-i` compiles case-insensitively rather than lowercasing,
        // so it inherits the regex engine's full Unicode case folding — strictly
        // stronger than `tk search`'s ASCII-only `lower()`. `é` folds to `É`.
        let (code, out) = grep_one_args(
            "Subject",
            "the CAFÉ menu",
            Args {
                pattern: "café".to_owned(),
                ignore_case: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("the CAFÉ menu"), "out={out:?}");
    }

    #[test]
    fn fixed_strings_matches_regex_metacharacters_verbatim() {
        // tk-120: `-F` escapes the pattern, so metacharacters match literally.
        // `a(b` is an unbalanced group as a regex (a usage error without `-F`),
        // but a valid literal needle with it.
        let (code, out) = grep_one_args(
            "Subject",
            "the call site is a(b) here",
            Args {
                pattern: "a(b".to_owned(),
                fixed: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("a(b)"), "out={out:?}");
    }

    #[test]
    fn fixed_strings_does_not_treat_pattern_as_regex() {
        // `.*` under `-F` is the literal two characters, not "any run": it hits
        // a body that contains `.*` and misses one that does not — the opposite
        // of the regex default, where `.*` matches every non-empty line.
        let (code, out) = grep_one_args(
            "Subject",
            "the glob .* expands",
            Args {
                pattern: ".*".to_owned(),
                fixed: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("glob .* expands"), "out={out:?}");

        let (code, out) = grep_one_args(
            "Subject",
            "no metacharacters present",
            Args {
                pattern: ".*".to_owned(),
                fixed: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::NoMatch);
        assert!(out.is_empty(), "out={out:?}");
    }

    #[test]
    fn fixed_strings_composes_with_ignore_case() {
        // `-F` escapes first, then `-i` folds: the literal `A(B` matches the
        // lowercase occurrence in the body.
        let (code, out) = grep_one_args(
            "Subject",
            "the x a(b y site",
            Args {
                pattern: "A(B".to_owned(),
                ignore_case: true,
                fixed: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("a(b"), "out={out:?}");
    }

    #[test]
    fn quiet_suppresses_output_on_a_match() {
        // tk-119: `-q` writes nothing; the match is carried by the exit code
        // alone (exit 0), so a script reads the predicate without parsing output.
        let (code, out) = grep_one_args(
            "Subject",
            "the auth token",
            Args {
                pattern: "auth".to_owned(),
                quiet: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert!(out.is_empty(), "quiet must suppress stdout: {out:?}");
    }

    #[test]
    fn quiet_no_match_is_exit_one_with_no_output() {
        // tk-119: a quiet no-match keeps the 0/1 overload — exit 1, empty
        // stdout (and empty stderr, distinguishing it from a failure).
        let (code, out) = grep_one_args(
            "Subject",
            "the auth token",
            Args {
                pattern: "nonexistent".to_owned(),
                quiet: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::NoMatch);
        assert!(out.is_empty(), "out={out:?}");
    }

    #[test]
    fn count_prints_the_number_of_matching_items_not_lines() {
        // tk-121: `-c` counts items, not lines. tk-1 matches on two body lines
        // but counts once; tk-2 does not match; tk-3 matches — total 2, not 3.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        for (id, display, title, body, seq) in [
            ("a", "tk-1", "First", "SHARED alpha\nSHARED gamma", 1),
            ("b", "tk-2", "Second", "no hit here", 2),
            ("c", "tk-3", "Third", "SHARED beta", 3),
        ] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title,
                    body,
                    created_seq: seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
        }
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                pattern: "SHARED".to_owned(),
                count: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        assert_eq!(String::from_utf8(h.stdout).unwrap(), "2\n");
    }

    #[test]
    fn count_prints_zero_and_exits_one_on_no_match() {
        // tk-121: like `grep -c`, a no-match still prints `0`, and the 0/1 exit
        // overload reports exit 1.
        let (code, out) = grep_one_args(
            "Subject",
            "nothing relevant",
            Args {
                pattern: "absent".to_owned(),
                count: true,
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::NoMatch);
        assert_eq!(out, "0\n");
    }

    #[test]
    fn title_only_match_renders_label_and_facet_without_a_body_hunk() {
        // ADR-0026: a title hit renders the label + facet (the highlighted title
        // is the cue) but produces no body hunk, since nothing in the body matched.
        let (code, out) = grep_one("Refactor the auth layer", "unrelated body text", "auth");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("tk-1"), "out={out:?}");
        assert!(out.contains("Refactor the auth layer"), "out={out:?}");
        assert!(!out.contains("unrelated body text"), "out={out:?}");
    }

    #[test]
    fn non_contiguous_hunks_are_separated_by_a_dashes_line() {
        // grep behavior (ADR-0026): two matches far enough apart that their ±3
        // windows don't touch render as two hunks split by a bare `--`, and the
        // lines in the gap between the windows are omitted.
        let body = "NEEDLE first\nctx1\nctx2\nctx3\nGAP4\nGAP5\nGAP6\nctx7\nctx8\nctx9\nNEEDLE second\nctx11\nctx12\nctx13";
        let (code, out) = grep_one("Subject", body, "NEEDLE");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("NEEDLE first"), "out={out:?}");
        assert!(out.contains("NEEDLE second"), "out={out:?}");
        assert!(
            out.contains("\n--\n"),
            "expected a -- hunk separator: {out:?}"
        );
        for gap in ["GAP4", "GAP5", "GAP6"] {
            assert!(!out.contains(gap), "did not expect {gap:?} in {out:?}");
        }
    }

    #[test]
    fn hunk_separator_is_blue_under_color() {
        // ADR-0026: the `--` between non-contiguous hunks is blue (secondary to
        // the cyan Display ID and red matches), gated by the Styler so piped
        // output stays a bare `--`.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        let body = "NEEDLE one\na\nb\nc\nd\ne\nf\ng\nNEEDLE two";
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body,
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered_with(
            &mut h,
            Styler::always(),
            Args {
                pattern: "NEEDLE".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        // Blue opens `\x1b[34m` and closes `\x1b[39m` (foreground default).
        assert!(
            out.contains("\u{1b}[34m--\u{1b}[39m"),
            "blue -- separator missing: {out:?}"
        );
    }

    #[test]
    fn multiple_matches_render_in_creation_order_separated_by_a_blank_line() {
        // ADR-0026: matches stream in `created_seq` order (never ranked), one
        // `tk show`-style block per Item, blocks separated by a blank line.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        for (id, display, title, body, seq) in [
            ("a", "tk-1", "First", "SHARED alpha", 1),
            ("b", "tk-2", "Second", "no hit here", 2),
            ("c", "tk-3", "Third", "SHARED beta", 3),
        ] {
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id,
                    display,
                    title,
                    body,
                    created_seq: seq,
                    ..FixtureItem::default()
                },
            )
            .unwrap();
        }
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                pattern: "SHARED".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(!out.contains("tk-2"), "out={out:?}");
        let (i1, i3) = (out.find("tk-1"), out.find("tk-3"));
        assert!(
            i1.is_some() && i3.is_some() && i1 < i3,
            "creation order: out={out:?}"
        );
        // A blank line precedes the second block.
        assert!(
            out.contains("\n\n\u{25cb} tk-3 \u{b7} Third"),
            "expected a blank line before the tk-3 block: {out:?}"
        );
    }

    #[test]
    fn invalid_pattern_is_a_usage_error_before_opening_the_store() {
        // ADR-0026: a malformed regex is a usage error (exit 2), surfaced
        // fail-fast — the pattern is compiled before the store is opened, so no
        // git discovery runs (no expect_git below: an unexpected run would panic).
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let code = run_rendered(
            &mut h,
            Args {
                pattern: "a(".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk grep: invalid pattern"),
            "stderr={stderr:?}"
        );
        assert!(h.stdout.is_empty(), "stdout={:?}", h.stdout);
    }

    #[test]
    fn empty_pattern_is_a_usage_error() {
        // ADR-0026 (amended): only a truly-empty pattern is rejected — it would
        // match every line and dump the whole store. Usage error, exit 2, before
        // the store is opened (no expect_git: an unexpected run would panic).
        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        let code = run_rendered(
            &mut h,
            Args {
                pattern: String::new(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Usage);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk grep: pattern must not be empty"),
            "stderr={stderr:?}"
        );
        assert!(h.stdout.is_empty(), "stdout={:?}", h.stdout);
    }

    #[test]
    fn whitespace_pattern_is_a_valid_needle() {
        // ADR-0026 (amended): a whitespace pattern is a normal needle (matched
        // like grep/ripgrep), not rejected as empty. The two-space pattern hits
        // the double space in the body.
        let (code, out) = grep_one("Subject", "the  gap here", "  ");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("the  gap here"), "out={out:?}");
    }

    #[test]
    fn block_never_renders_relationship_or_blocker_sections() {
        // ADR-0026: a grep block is label + facet + hunks only. Relationship
        // sections (PARENT/TICKETS/BLOCKED BY/BLOCKING) are references, not
        // matched text, so grep omits them even when the Item has dependencies.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "tk-1",
                title: "Subject",
                body: "the UNIQUEWORD appears here",
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
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered(
            &mut h,
            Args {
                pattern: "UNIQUEWORD".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        for header in [
            "PARENT",
            "TICKETS",
            "BLOCKED BY",
            "BLOCKING",
            "EXTERNAL BLOCKERS",
        ] {
            assert!(
                !out.contains(header),
                "did not expect {header:?} in {out:?}"
            );
        }
    }

    #[test]
    fn matches_a_done_item_across_all_statuses() {
        // ADR-0026: grep covers every Item Status, including done.
        let (code, out) = {
            let store = TmpStore::new("repo");
            let conn = seed_store(&store);
            insert_fixture_item(
                &conn,
                FixtureItem {
                    id: "t1",
                    display: "tk-1",
                    title: "Closed work",
                    body: "shipped the DONEMARKER fix",
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
            let code = run_rendered(
                &mut h,
                Args {
                    pattern: "DONEMARKER".to_owned(),
                    ..Args::default()
                },
            );
            (code, String::from_utf8(h.stdout).unwrap())
        };
        assert_eq!(code, Exit::Ok);
        // Done glyph ✓ on the label line proves the done Item rendered.
        assert!(out.contains("\u{2713} tk-1"), "out={out:?}");
        assert!(out.contains("DONEMARKER"), "out={out:?}");
    }

    #[test]
    fn matches_are_highlighted_in_both_body_and_title_under_color() {
        // ADR-0026: matched text is highlighted via the Styler (bright yellow,
        // \x1b[93m..\x1b[39m), in body hunks and in the title. The span nests
        // inside the bold title without breaking the outer bold.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "auth subject",
                body: "the auth token here",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let styler = Styler::always();
        let code = run_rendered_with(
            &mut h,
            styler,
            Args {
                pattern: "auth".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        // Body match wrapped in bright yellow.
        assert!(
            out.contains("the \u{1b}[93mauth\u{1b}[39m token here"),
            "body highlight missing: {out:?}"
        );
        // Title match wrapped, nested inside the bold (\x1b[1m) HEADER.
        assert!(
            out.contains("\u{1b}[93mauth\u{1b}[39m subject"),
            "title highlight missing: {out:?}"
        );
        assert!(
            out.contains("\u{1b}[1m"),
            "title should still be bold: {out:?}"
        );
    }

    #[test]
    fn body_control_bytes_are_sanitized_in_a_hunk() {
        // The matched body text is user/Remote-controlled; a stray ESC must not
        // reach the terminal as SGR. Under the plain styler grep emits no SGR of
        // its own, so any raw ESC would be an injection.
        let (code, out) = grep_one("Subject", "alert \u{1b}[31mRED MATCHME", "MATCHME");
        assert_eq!(code, Exit::Ok);
        assert!(out.contains("\\x1b[31mRED"), "ESC not escaped: {out:?}");
        assert!(
            !out.as_bytes().contains(&0x1b),
            "raw ESC reached output: {out:?}"
        );
    }

    #[test]
    fn match_at_line_start_leaves_the_indent_outside_the_highlight() {
        // The underline must never cover a hunk line's leading indent: the
        // indent is written plain *before* the highlight opens, so even a match
        // at column 0 of the body renders as `··\x1b[4m…`. This is what keeps
        // underline from looking ragged on indented multi-line output.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body: "MARKER at the very start",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        drop(conn);

        let cwd_path = cwd();
        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        let code = run_rendered_with(
            &mut h,
            Styler::always(),
            Args {
                pattern: "MARKER".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        // Two-space indent, THEN the colour open — indent is not coloured, and
        // the colour closes before the rest of the line (and the newline).
        assert!(
            out.contains("  \u{1b}[93mMARKER\u{1b}[39m at the very start\n"),
            "indent should sit outside the highlight: {out:?}"
        );
    }

    #[test]
    fn a_pattern_does_not_match_across_a_newline() {
        // Per-line matching (ADR-0026): a phrase split over a hard newline never
        // matches, which is also why a highlight can never span two lines.
        let (code, out) = grep_one("Subject", "ends with END\nSTART begins", "END START");
        assert_eq!(code, Exit::NoMatch);
        assert!(out.is_empty(), "out={out:?}");
    }

    /// A stdout that fails every write with a chosen `ErrorKind`, modelling a
    /// pipe closed by `| head` (`BrokenPipe`) or a full disk on a redirect
    /// (`StorageFull` / other).
    struct FailingWriter(std::io::ErrorKind);
    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.0, "write failed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_mid_stream_is_a_match_not_a_no_match_or_failure() {
        // `tk grep PATTERN | head` closes stdout after a block; the next write
        // fails BrokenPipe. A write only happens after a block has started, so
        // matches WERE found — exit must be Ok (match), never 1 (which a script
        // would read as "no match" since stderr is empty) and never a Failure.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body: "the PIPEWORD appears here",
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
        let mut stdout = FailingWriter(std::io::ErrorKind::BrokenPipe);
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
        let code = match run(
            &mut deps,
            Args {
                pattern: "PIPEWORD".to_owned(),
                ..Args::default()
            },
        ) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "grep");
                exit
            }
        };
        assert_eq!(
            code,
            Exit::Ok,
            "broken pipe mid-stream means a match was found and piped"
        );
        assert!(
            stderr.is_empty(),
            "a broken pipe writes no diagnostic: {stderr:?}"
        );
    }

    #[test]
    fn non_broken_pipe_write_error_fails_with_a_stderr_diagnostic() {
        // A stdout write error that is NOT a broken pipe (e.g. a full disk on
        // `tk grep X > file`) must NOT collapse to the empty-stderr exit 1 of
        // NoMatch: it writes a diagnostic (so stderr is non-empty, distinct from
        // a no-match) and returns Failure, honouring Exit::Failure's contract.
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Subject",
                body: "the PIPEWORD appears here",
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
        let mut stdout = FailingWriter(std::io::ErrorKind::StorageFull);
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
        let code = match run(
            &mut deps,
            Args {
                pattern: "PIPEWORD".to_owned(),
                ..Args::default()
            },
        ) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "grep");
                exit
            }
        };
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(
            stderr.contains("tk grep: failed to write output"),
            "a non-broken-pipe write error must write a diagnostic: {stderr:?}"
        );
    }

    #[test]
    fn epic_match_renders_the_epic_facet_branch() {
        // An Epic block uses the `Epic · Created: …` facet, not the
        // `P_ · Kind` Ticket facet — the shared header's Epic branch reached
        // through grep, not just `tk show`.
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
                title: "Epic subject",
                body: "rollout EPICWORD plan",
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
                pattern: "EPICWORD".to_owned(),
                ..Args::default()
            },
        );
        assert_eq!(code, Exit::Ok);
        let out = String::from_utf8(h.stdout).unwrap();
        assert!(
            out.contains("  Epic \u{b7} Created: 2026-05-09"),
            "out={out:?}"
        );
        assert!(!out.contains("Task"), "out={out:?}");
    }
}
