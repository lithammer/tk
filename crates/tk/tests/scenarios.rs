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
        let args = shlex::split(cmd).expect("command must shell-split");
        let out = Command::cargo_bin("tk")
            .expect("cargo bin tk")
            .args(&args)
            .current_dir(&self.cwd)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            // Scrub the colour-policy env (per-child, never the test process's
            // global state) so a developer's shell cannot tint the output. The
            // binary reads no determinism env knobs (tk-105), so there is
            // nothing else to scrub.
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .output()
            .expect("run tk");
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

/// Clap owns `--help` formatting; these snapshots exist to surface an
/// unintended change in that generated output, not to pin a hand-authored
/// contract. Extend the list as command help is worth guarding.
#[test]
fn command_help_snapshots() {
    let p = Repo::new("repo");
    for command in ["block", "done", "list", "next", "show", "unblock", "update"] {
        insta::assert_snapshot!(
            format!("help_{command}"),
            p.run(&format!("{command} --help"))
        );
    }
}
