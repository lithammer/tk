//! CLI scenario tests for the `tk` binary.
//!
//! Each scenario drives the built `tk` binary through a real git repo inside a
//! `$TESTROOT` scratch directory and asserts the rendered stdout/stderr/exit:
//!
//! - Behavioural scenarios use inline [`insta`] snapshots via the [`tk!`] macro
//!   (write `@""`, run `cargo insta test --accept`, review the diff).
//! - `tk manpage` / `tk prime` assert their output equals the embedded
//!   source-of-truth files, so the snapshot does not duplicate them.
//! - The clap-generated `--help` output is captured as file snapshots: a
//!   change is surfaced for review, not asserted as a hand-authored contract.
//!
//! Isolation: every `tk` runs as its own subprocess, so the per-child env set
//! here is never the test process's global state and scenarios run in parallel
//! safely. The colour-policy env (`NO_COLOR` / `CLICOLOR_FORCE`) is scrubbed so
//! a developer's shell cannot tint the output. The binary reads no determinism
//! env knobs (tk-105), so nothing else needs pinning: the random `items.id`
//! never appears in output and OS entropy keeps it distinct across a scenario's
//! `tk add` calls; no current scenario surfaces a timestamp; and a value that
//! does vary (git's refusal stderr) is handled with an insta redaction filter.
//! `GIT_CEILING_DIRECTORIES` pins git discovery to the scratch tree so a
//! `$TESTROOT` under an ambient repo cannot make a refusal scenario pass
//! spuriously.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

/// A `$TESTROOT`-rooted scratch area for one scenario.
struct Repo {
    _tmp: TempDir,
    root: PathBuf,
    cwd: PathBuf,
}

impl Repo {
    /// A scratch repo with a fresh git tree in `<testroot>/<name>`. The repo
    /// directory name becomes the Display ID prefix, so `name` controls the
    /// `<prefix>-N` IDs a scenario produces.
    fn new(name: &str) -> Self {
        let repo = Self::bare(name);
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo.cwd)
            .output()
            .expect("git init");
        repo
    }

    /// A scratch directory with no git repo — for refusal scenarios.
    fn bare(name: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");
        let cwd = root.join(name);
        fs::create_dir(&cwd).expect("create repo dir");
        Self {
            _tmp: tmp,
            root,
            cwd,
        }
    }

    /// Run `tk <cmd>` (shell-split) in the repo and return the rendered output.
    fn run(&self, cmd: &str) -> String {
        self.run_env(cmd, &[])
    }

    /// Like [`run`] but with extra environment variables set on the child,
    /// used to exercise the `TK_SCOPE` Scope channel (ADR-0022).
    fn run_env(&self, cmd: &str, env: &[(&str, &str)]) -> String {
        let args = shlex::split(cmd).expect("command must shell-split");
        let mut command = Command::cargo_bin("tk").expect("cargo bin tk");
        command
            .args(&args)
            .current_dir(&self.cwd)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            // Scrub the colour-policy env (per-child, never the test process's
            // global state) so a developer's shell cannot tint the output. The
            // binary reads no determinism env knobs (tk-105), so there is
            // nothing else to scrub.
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("TK_SCOPE");
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().expect("run tk");
        render(&out, &self.root)
    }
}

/// Render a command result with `$TESTROOT` redacted: bare stdout on the happy
/// path, and a framed block exposing exit/stderr only when they are non-trivial.
fn render(out: &Output, root: &Path) -> String {
    let redact = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes).replace(root.to_str().expect("utf-8 root"), "$TESTROOT")
    };
    let code = out.status.code().unwrap_or(-1);
    let stdout = redact(&out.stdout);
    let stderr = redact(&out.stderr);
    if code == 0 && stderr.is_empty() {
        stdout
    } else {
        format!("exit {code}\n-- stdout --\n{stdout}-- stderr --\n{stderr}")
    }
}

/// Repository root, two levels up from `crates/tk`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

/// Run a `tk` command and snapshot its rendered output inline.
macro_rules! tk {
    ($repo:expr, $cmd:expr, @$snapshot:literal) => {
        insta::assert_snapshot!($repo.run($cmd), @$snapshot)
    };
}

#[test]
fn init_fresh() {
    let p = Repo::new("repo");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/repo/.git/tk/tk.db");
}

#[test]
fn init_idempotent() {
    let p = Repo::new("repo");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/repo/.git/tk/tk.db");
    tk!(p, "init", @"Repository Store already initialized at $TESTROOT/repo/.git/tk/tk.db");
}

#[test]
fn create_epic_child() {
    let p = Repo::new("project");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/project/.git/tk/tk.db");
    tk!(p, "add --epic -m 'Feature Epic'", @r"
    Created Epic: project-1 - Feature Epic
    Status: open
    ");
    tk!(p, "add --parent project-1 -m 'Build child Ticket'", @r"
    Created Ticket: project-2 - Build child Ticket
    Kind: task
    Priority: P2
    Status: open
    Parent: project-1
    ");
    tk!(p, "list", @r"
    ○ project-1 [epic] Feature Epic
    └── ○ project-2 ● P2 Build child Ticket
    --------------------------------------------------------------------------------
    Total: 2 items (2 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");
}

#[test]
fn done_records_a_closing_reason_and_refuses_to_amend() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Fix the bug'"); // project-1

    // An empty `-m` is rejected before any transition (ADR-0023).
    tk!(p, "done project-1 -m ''", @r"
    exit 1
    -- stdout --
    -- stderr --
    tk done: closing reason must not be empty
    ");

    // A real reason closes the Ticket.
    tk!(p, "done project-1 -m 'Fixed in PR #12'", @"Done Ticket: project-1 - Fix the bug");

    // Set-once: re-closing with a new reason is refused, not amended.
    tk!(p, "done project-1 -m 'second thoughts'", @r"
    exit 1
    -- stdout --
    -- stderr --
    tk done: 'project-1' is already done; closing reason not changed
    ");
}

#[test]
fn done_trims_the_closing_reason_before_storing_it() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Trim me'"); // project-1
    p.run("done project-1 -m '   Fixed in standup   '");

    // `tk show` surfaces a non-deterministic date, so assert on substrings:
    // the stored reason is trimmed (ADR-0023), not the padded input.
    let out = p.run("show project-1");
    assert!(
        out.contains("CLOSING REASON\nFixed in standup\n"),
        "out={out}"
    );
}

#[test]
fn init_refuses_outside_git_repository() {
    let p = Repo::bare("scratch");
    let out = p.run("init");
    // Guard the trigger: an ambient repo would let init succeed; fail loud
    // rather than snapshot a non-refusal.
    assert!(
        out.contains("not a git repository"),
        "expected a not-a-repo refusal; got:\n{out}"
    );
    // tk surfaces git's own stderr; past "not a git repository" the wording
    // varies by environment (mount boundary vs parent directories) and git
    // version, so redact it. tk's contract is the `tk init:` prefix, the
    // surfaced fatal, and exit 1.
    insta::with_settings!({filters => vec![(r"(?s)not a git repository.*", "not a git repository [git detail]")]}, {
        insta::assert_snapshot!(out, @r"
        exit 1
        -- stdout --
        -- stderr --
        tk init: fatal: not a git repository [git detail]");
    });
}

#[test]
fn manpage_emits_embedded_manpage() {
    let p = Repo::new("repo");
    let expected = fs::read_to_string(repo_root().join("man/tk.1")).expect("read man/tk.1");
    assert_eq!(p.run("manpage"), expected);
}

#[test]
fn prime_emits_workflow_briefing() {
    let p = Repo::new("repo");
    p.run("init");
    let expected =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/commands/prime.md"))
            .expect("read prime.md");
    assert_eq!(p.run("prime"), expected);
}

#[test]
fn prime_is_silent_without_initialized_store() {
    let p = Repo::new("repo");
    assert_eq!(p.run("prime"), "");
}

#[test]
fn prime_is_silent_outside_git_repository() {
    let p = Repo::bare("scratch");
    assert_eq!(p.run("prime"), "");
}

/// `TK_SCOPE` is the orchestrator/AFK Scope channel (ADR-0022): a child
/// inherits it and `tk list` filters to that Epic without a positional
/// argument. Guards the env-read wiring that unit tests cannot reach.
#[test]
fn tk_scope_env_filters_list_to_the_epic() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Auth epic'"); // project-1
    p.run("add --parent project-1 -m 'Login form'"); // project-2
    p.run("add -m 'Unrelated chore'"); // project-3

    let out = p.run_env("list", &[("TK_SCOPE", "project-1")]);
    assert!(
        out.contains("Scope: project-1 (Epic + child Tickets)"),
        "out={out}"
    );
    assert!(out.contains("project-2"), "out={out}");
    assert!(!out.contains("project-3"), "out={out}");
}

/// Clap owns `--help` formatting; these snapshots exist to surface an
/// unintended change in that generated output, not to pin a hand-authored
/// contract. Extend the list as command help is worth guarding.
#[test]
fn command_help_snapshots() {
    let p = Repo::new("repo");
    for command in [
        "block", "done", "grep", "list", "next", "search", "show", "unblock", "update",
    ] {
        insta::assert_snapshot!(
            format!("help_{command}"),
            p.run(&format!("{command} --help"))
        );
    }
}

/// `tk search` is a flat, whole-store title lookup across every Item Status
/// (ADR-0025). The matched child Ticket renders flat — no List Tree nesting —
/// even though its parent Epic also matches, and the `done` match shows no
/// `⊘` despite its unresolved blocker.
#[test]
fn search_matches_titles_across_statuses() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Auth rework'"); // project-1
    p.run("add -m 'Add auth middleware'"); // project-2
    p.run("add -m 'Auth token refresh'"); // project-3
    p.run("add -m 'Unrelated chore'"); // project-4
    p.run("add --parent project-1 -m 'Auth login form'"); // project-5
    p.run("start project-2");
    p.run("done project-3 -m 'shipped'");

    tk!(p, "search auth", @"
    ○ project-1 [epic] Auth rework
    ◐ project-2 ● P2 Add auth middleware
    ✓ project-3 ● P2 Auth token refresh
    ○ project-5 ● P2 Auth login form
    --------------------------------------------------------------------------------
    Total: 4 items (2 open, 1 active, 1 done)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");
    tk!(p, "search nonexistent", @r#"No items match "nonexistent"."#);
}

/// The query is a single required positional: omitting it is a usage error,
/// and `--` lets a leading-dash query reach the positional.
#[test]
fn search_requires_a_query_and_double_dash_escapes_it() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Investigate the -v verbose flag'"); // project-1

    tk!(p, "search", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the following required arguments were not provided:
      <QUERY>

    Usage: tk search <QUERY>

    For more information, try '--help'.
    ");
    tk!(p, "search -- -v", @"
    ○ project-1 ● P2 Investigate the -v verbose flag
    --------------------------------------------------------------------------------
    Total: 1 item (1 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");
}

/// `tk grep` searches title and body for a regular expression and renders each
/// match as a `tk show`-style block — label line, facet bar, then the body
/// collapsed to the matching lines — in creation order (ADR-0026). project-2
/// matches in the body; project-3 matches in the title and so shows no body
/// hunk; project-4 does not match. Matching is case-sensitive, so the capital
/// `Auth` epic title is not hit by the lowercase pattern.
#[test]
fn grep_renders_show_style_match_context() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Auth rework'"); // project-1 (epic; capital A, no match)
    p.run("add -m 'Add middleware' -m 'the handler validates the auth token'"); // project-2
    p.run("add -m 'Refactor auth layer'"); // project-3 (title match, no body)
    p.run("add -m 'Unrelated chore' -m 'nothing relevant here'"); // project-4

    // The facet bar surfaces the creation date; redact it so the snapshot is
    // stable across days.
    insta::with_settings!({filters => vec![(r"Created: \d{4}-\d{2}-\d{2}", "Created: [DATE]")]}, {
        tk!(p, "grep auth", @"
        ○ project-2 · Add middleware
          P2 · Task · Created: [DATE]
          the handler validates the auth token

        ○ project-3 · Refactor auth layer
          P2 · Task · Created: [DATE]
        ");
    });
}

/// `-i` flips grep's case-sensitive default (ADR-0026) for one invocation, so
/// the lowercase pattern now hits the capitalised `Auth` epic title (tk-117).
#[test]
fn grep_ignore_case_matches_across_case() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Auth rework'"); // project-1 (capital A)
    p.run("add -m 'Unrelated chore'"); // project-2 (no match)

    insta::with_settings!({filters => vec![(r"Created: \d{4}-\d{2}-\d{2}", "Created: [DATE]")]}, {
        tk!(p, "grep auth -i", @"
        ○ project-1 · Auth rework
          Epic · Created: [DATE]
        ");
    });
}

/// `-F` matches the pattern as a literal (ADR-0026, tk-120): `a(b` is an invalid
/// regex (unbalanced group) but a valid literal needle, so `-F` finds it where
/// the bare pattern would be a usage error.
#[test]
fn grep_fixed_strings_matches_a_literal() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Fix parser' -m 'the token a(b breaks the lexer'"); // project-1

    insta::with_settings!({filters => vec![(r"Created: \d{4}-\d{2}-\d{2}", "Created: [DATE]")]}, {
        tk!(p, "grep 'a(b' -F", @"
        ○ project-1 · Fix parser
          P2 · Task · Created: [DATE]
          the token a(b breaks the lexer
        ");
    });
}

/// `-C 0` collapses each hunk to the matching line, overriding the default-3
/// window (ADR-0026, tk-118): only the body paragraph carrying the needle shows,
/// not the one before it.
#[test]
fn grep_context_zero_shows_only_the_matching_line() {
    let p = Repo::new("project");
    p.run("init");
    // Two body paragraphs (blank-line separated); the needle is in the second.
    p.run("add -m 'Subject' -m 'first paragraph here' -m 'second needle paragraph'"); // project-1

    insta::with_settings!({filters => vec![(r"Created: \d{4}-\d{2}-\d{2}", "Created: [DATE]")]}, {
        tk!(p, "grep needle -C 0", @"
        ○ project-1 · Subject
          P2 · Task · Created: [DATE]
          second needle paragraph
        ");
    });
}

/// `-q` suppresses all output and carries the answer in the exit code alone
/// (ADR-0026, tk-119): a match is a silent exit 0, a no-match a silent exit 1.
#[test]
fn grep_quiet_is_silent_and_signals_via_exit_code() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Add middleware' -m 'the auth token'"); // project-1

    // Match: silent, exit 0 (bare empty stdout).
    tk!(p, "grep auth -q", @"");
    // No match: silent, exit 1.
    tk!(p, "grep nonexistent -q", @"
    exit 1
    -- stdout --
    -- stderr --
    ");
}

/// `-c` prints the count of matching items, not the match blocks (ADR-0026,
/// tk-121). The unit is the item: project-1 matches on two body lines but counts
/// once, project-3 matches in its title — total 2.
#[test]
fn grep_count_prints_matching_item_total() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Add middleware' -m 'the auth token' -m 'more auth here'"); // project-1 (two matching lines)
    p.run("add -m 'Unrelated chore'"); // project-2 (no match)
    p.run("add -m 'Refactor auth layer'"); // project-3 (title match)

    tk!(p, "grep auth -c", @"2");
}

/// `-c` and `-q` are mutually exclusive (tk-121): one prints a count, the other
/// suppresses all output, so clap rejects the combination as a usage error
/// before any store work. This guard is load-bearing — without it, `-q -c` would
/// break on the first match without counting, then print a bogus `0`.
#[test]
fn grep_count_and_quiet_conflict() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Add middleware' -m 'the auth token'"); // project-1

    tk!(p, "grep auth -c -q", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--count' cannot be used with '--quiet'

    Usage: tk grep --count <PATTERN>

    For more information, try '--help'.
    ");
}

/// The pattern is required (clap usage error), an empty pattern is rejected, and
/// a no-match exits 1 with empty streams — the `grep -q`-style predicate where
/// empty stderr distinguishes "no match" from "broken" (ADR-0026).
#[test]
fn grep_requires_a_pattern_and_signals_no_match_with_exit_one() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Unrelated chore'"); // project-1

    tk!(p, "grep", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the following required arguments were not provided:
      <PATTERN>

    Usage: tk grep <PATTERN>

    For more information, try '--help'.
    ");
    tk!(p, "grep '   '", @"
    exit 2
    -- stdout --
    -- stderr --
    tk grep: pattern must not be empty
    ");
    tk!(p, "grep nonexistent", @"
    exit 1
    -- stdout --
    -- stderr --
    ");
}
