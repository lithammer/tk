//! Real-Git CLI scenarios through the command dependency seam (ADR-0053).
//!
//! Each command runs in a child test process with an isolated data root.
//! Environment changes stay per-child; Store IDs and dates are redacted only
//! where their values are outside the rendered contract.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use tempfile::TempDir;

#[test]
fn stderr_diagnostics_honor_forced_color_and_no_color() {
    let repo = Repo::new("color");
    repo.run("init");
    let args = ["sync", "log", "7"].map(str::to_owned);
    for (env, expected) in [
        (vec![], "tk sync log: Mutation 7 not found\n"),
        (
            vec![("CLICOLOR_FORCE", "1")],
            "\x1b[1m\x1b[31mtk sync log:\x1b[39m\x1b[22m Mutation 7 not found\n",
        ),
        (
            vec![("CLICOLOR_FORCE", "0")],
            "\x1b[1m\x1b[31mtk sync log:\x1b[39m\x1b[22m Mutation 7 not found\n",
        ),
        (
            vec![("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")],
            "tk sync log: Mutation 7 not found\n",
        ),
    ] {
        let out = support::run(&repo.cwd, &repo.root, &args, &env);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert_eq!(out.stderr, expected.as_bytes(), "env={env:?}");
    }
}

#[test]
fn plan_bulk_edits_reject_invalid_batches_without_partial_changes() {
    let repo = Repo::new("plan");
    repo.run("init");
    repo.run("add -m 'First'");
    repo.run("add -m 'Second'");
    repo.run("add --epic -m 'Epic'");
    for invalid in ["missing", "plan-3"] {
        assert!(
            repo.run(&format!("plan add plan-1 {invalid} plan-2"))
                .starts_with("exit 1\n")
        );
        assert!(repo.run("plan").contains("0/0 done"));
    }
    assert_eq!(
        repo.run("plan add plan-1 PLAN-1 plan-2"),
        "Added to Plan: plan-1\nAdded to Plan: plan-2\n"
    );
    let before = repo.run("plan");
    for invalid in ["missing", "plan-3"] {
        assert!(
            repo.run(&format!("plan remove plan-1 {invalid} plan-2"))
                .starts_with("exit 1\n")
        );
        assert_eq!(repo.run("plan"), before);
    }
    assert_eq!(repo.run("plan add plan-1"), "Already in Plan: plan-1\n");
    repo.run("plan remove plan-1");
    assert_eq!(repo.run("plan remove plan-1"), "Not in Plan: plan-1\n");
    assert!(repo.run("plan add").starts_with("exit 2\n"));
    assert!(repo.run("plan remove").starts_with("exit 2\n"));
    assert_eq!(repo.run("plan clear"), "Cleared Plan (1 removed)\n");
    assert_eq!(repo.run("plan clear"), "Cleared Plan (0 removed)\n");
}

#[test]
fn plan_scope_intersects_epics_and_preserves_priority_paths() {
    let repo = Repo::new("plan");
    repo.run("init");
    repo.run("add --epic -m 'First Epic'");
    repo.run("add --epic -m 'Second Epic'");
    repo.run("add -m 'Helper' -p P3 -P plan-1");
    repo.run("add -m 'Ordinary work' -p P2 -P plan-1");
    repo.run("add -m 'Included child' -p P1 -P plan-2");
    repo.run("add -m 'Excluded child' -p P0 -P plan-2");
    repo.run("block plan-2 plan-3");
    repo.run("plan add plan-3 plan-4 plan-5");
    let selected = repo.run("next --plan");
    assert!(selected.contains("plan-5: Included child"), "{selected}");
    // Child readiness is independent of its Epic's blockers.
    repo.run("block plan-5 plan-3");
    let selected = repo.run("next --plan");
    assert!(selected.contains("plan-3: Helper"), "{selected}");
    assert!(selected.contains("via plan-5"), "{selected}");
    assert!(!selected.contains("via plan-6"), "{selected}");
    assert_eq!(repo.run("next plan-1 --plan"), "plan-4: Ordinary work\n");
    assert_eq!(
        repo.run_env("next --plan", &[("TK_SCOPE", "plan-1")]),
        "plan-4: Ordinary work\n"
    );
    assert_eq!(
        repo.run_env("next plan-1 --plan -q", &[("TK_SCOPE", "plan-2")]),
        "plan-4\n"
    );
    assert!(
        repo.run("next plan-2 --plan")
            .contains("no ready Tickets in Plan and Epic plan-2")
    );
    repo.run("plan remove plan-5");
    assert_eq!(repo.run("next --plan"), "plan-4: Ordinary work\n");
}

#[test]
fn plan_selection_bounds_candidates_and_priority() {
    let repo = Repo::new("plan");
    repo.run("init");
    repo.run("add -m 'Helper' -p P3");
    repo.run("add -m 'Other release work' -p P2");
    repo.run("add -m 'Outside urgent work' -p P0");
    repo.run("block plan-3 plan-1");
    repo.run("plan add plan-1 plan-2");
    assert!(repo.run("next").contains("plan-1: Helper\n"));
    assert_eq!(repo.run("next --plan"), "plan-2: Other release work\n");
    assert_eq!(repo.run("next --plan -q"), "plan-2\n");
    repo.run("add -m 'Included downstream work' -p P1");
    repo.run("block plan-4 plan-3");
    repo.run("plan add plan-4");
    assert_eq!(repo.run("next --plan"), "plan-2: Other release work\n");
    repo.run("plan remove plan-4");
    repo.run("plan remove plan-1 plan-2");
    repo.run("plan add plan-3");
    assert!(repo.run("next --plan").contains("no ready Tickets in Plan"));
}

#[test]
fn plan_membership_is_local_and_explicit() {
    let repo = Repo::new("plan");
    repo.run("init");
    repo.run("add -m 'First release work'");
    repo.run("add -m 'Later work'");
    assert_eq!(repo.run("plan add plan-1"), "Added to Plan: plan-1\n");
    assert_eq!(
        repo.run("plan"),
        "Ready\n  ○ plan-1 ● P2 First release work\n\n1 remaining · 0/1 done\n"
    );
    assert_eq!(
        repo.run("plan remove plan-1"),
        "Removed from Plan: plan-1\n"
    );
    assert_eq!(
        repo.run("plan"),
        "No Tickets in Plan.\n\n0 remaining · 0/0 done\n"
    );
    assert_eq!(repo.run("sync log"), "No Mutations recorded.\n");
}

#[test]
fn plan_view_covers_every_state_and_ignores_scope() {
    let repo = Repo::new("plan");
    repo.run("init");
    for title in [
        "Ready work",
        "Active work",
        "Outside helper",
        "Blocked work",
        "Parked work",
    ] {
        repo.run(&format!("add -m '{title}'"));
    }
    repo.run("add --triage -m 'Needs triage'");
    repo.run("add -m 'Finished work'");
    repo.run("start plan-2");
    repo.run("block plan-4 plan-3");
    repo.run("park plan-5");
    repo.run("done plan-7");
    repo.run("plan add plan-1 plan-2 plan-4 plan-5 plan-6 plan-7");
    insta::assert_snapshot!(repo.run_env("plan", &[("TK_SCOPE", "not-an-epic")]), @"
    Ready
      ○ plan-1 ● P2 Ready work

    In progress
      ◐ plan-2 ● P2 Active work

    Waiting
      ○ plan-4 ● P2 Blocked work [blocked by plan-3 (outside Plan)]
      ○ plan-5 ● P2 Parked work [parked]
      ○ plan-6 Needs triage [triage]

    Done
      ✓ plan-7 ● P2 Finished work

    5 remaining · 1/6 done
    ");
    assert_eq!(repo.run("plan clear"), "Cleared Plan (6 removed)\n");
    assert!(repo.run("show plan-2").contains("◐ plan-2"));
    assert!(repo.run("show plan-7").contains("✓ plan-7"));
    repo.run("plan add plan-7");
    insta::assert_snapshot!(repo.run("plan"), @"
    Done
      ✓ plan-7 ● P2 Finished work

    0 remaining · 1/1 done
    ");
}

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
        let repo = Self::without_git(name);
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo.cwd)
            .output()
            .expect("git init");
        repo
    }

    /// A scratch directory with no git repo — for refusal scenarios.
    fn without_git(name: &str) -> Self {
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
        let out = support::run(&self.cwd, &self.root, &args, env);
        render(&out, &self.root)
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.cwd)
            .env("GIT_CONFIG_GLOBAL", support::global_config(&self.root))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn db_path(&self) -> PathBuf {
        self.root
            .join("data/tk/stores")
            .join(self.git(&["config", "--local", "--get", "tk.storeId"]))
            .join("tk.db")
    }

    fn move_to_legacy(&self) -> PathBuf {
        let source = self.cwd.join(".git/tk");
        fs::rename(self.db_path().parent().unwrap(), &source).unwrap();
        fs::remove_file(source.join("store.json")).unwrap();
        fs::remove_file(source.join("association.lock")).unwrap();
        self.git(&["config", "--local", "--unset", "tk.storeId"]);
        source
    }

    /// Write one Mutation straight into this repo's Mutation Log.
    ///
    /// No `tk` command leaves a `pending` or `failed` Mutation on a Local
    /// Ticket, and a scenario has no Remote to reach one through, so the
    /// states that drive the `Sync:` banner and the `Mutation Log:` count are
    /// out of reach otherwise. Keeps the `mutations` column list in one place,
    /// so a migration adding a column breaks one literal.
    fn seed_mutation(&self, display_value: &str, state: &str, failure_json: Option<&str>) {
        let conn = rusqlite::Connection::open(self.db_path()).unwrap();
        let item_id: String = conn
            .query_row(
                "select id from items where display_value = ?1",
                [display_value],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "insert into mutations( \
                sequence, mutation_type, item_id, item_class, payload_json, state, \
                failure_json, created_at, state_changed_at \
             ) values ( \
                1, 'update_ticket', ?1, 'ticket', '{\"title\":\"seeded\"}', ?2, ?3, \
                '2026-05-01T00:00:00.000Z', '2026-05-01T00:00:00.000Z' \
             )",
            rusqlite::params![&item_id, state, failure_json],
        )
        .unwrap();
    }
}

/// Render a command result with `$TESTROOT` redacted: bare stdout on the happy
/// path, and a framed block exposing exit/stderr only when they are non-trivial.
fn render(out: &Output, root: &Path) -> String {
    static STORE_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\$TESTROOT[/\\]data[/\\]tk[/\\]stores[/\\][0-9a-f]{32}[/\\]tk\.db")
            .unwrap()
    });
    let redact = |bytes: &[u8]| {
        STORE_PATH
            .replace_all(
                &String::from_utf8_lossy(bytes)
                    .replace(root.to_str().expect("utf-8 root"), "$TESTROOT"),
                "$$TESTROOT/data/tk/stores/[STORE_ID]/tk.db",
            )
            .to_string()
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
    // `dated:` redacts the facet bar's creation date, which varies by day.
    (dated: $repo:expr, $cmd:expr, @$snapshot:literal) => {
        insta::with_settings!({filters => vec![(r"Created: \d{4}-\d{2}-\d{2}", "Created: [DATE]")]}, {
            insta::assert_snapshot!($repo.run($cmd), @$snapshot)
        })
    };
}

#[test]
fn durable_store_survives_checkout_and_has_a_valid_association() {
    let p = Repo::new("repo");
    let out = p.run("init");
    assert!(out.starts_with("Initialized Repository Store at "), "{out}");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    assert_eq!(id.len(), 32, "init must install a Store ID");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

#[test]
fn durable_store_reopens_and_healthy_init_preserves_metadata() {
    let p = Repo::new("repo");
    p.run("init");
    let db = p.db_path();
    let manifest = db.with_file_name("store.json");
    let before = fs::read(&manifest).unwrap();
    let config = fs::read(p.cwd.join(".git/config")).unwrap();
    assert!(p.run("add -m 'Keep me'").contains("repo-1"));
    assert!(
        p.run("init")
            .starts_with("Repository Store already initialized at ")
    );
    assert!(p.run("show repo-1").contains("Keep me"));
    assert_eq!(fs::read(&manifest).unwrap(), before);
    assert_eq!(fs::read(p.cwd.join(".git/config")).unwrap(), config);
    let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(
        value["association"]["git_common_dir"],
        fs::canonicalize(p.cwd.join(".git"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(db.with_file_name("backups").is_dir());
    fs::remove_dir_all(&p.cwd).unwrap();
    assert!(db.is_file());
    assert_eq!(fs::read(&manifest).unwrap(), before);
}

#[test]
fn durable_store_linked_workspaces_share_but_copied_pointers_refuse() {
    let mut p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Shared work'");
    p.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "initial",
    ]);
    let linked = p.root.join("linked");
    p.git(&["worktree", "add", "-b", "linked", "../linked"]);
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.cwd = linked;
    assert!(p.run("show repo-1").contains("Shared work"));
    assert!(
        p.run("init")
            .starts_with("Repository Store already initialized at ")
    );
    let clone = p.root.join("independent");
    fs::create_dir(&clone).unwrap();
    p.cwd = clone;
    p.git(&["init", "-q"]);
    p.git(&["config", "--local", "tk.storeId", &id]);
    for command in ["list", "init"] {
        let out = p.run(command);
        assert!(out.contains("another Git Common Directory"), "{out}");
    }
    assert_eq!(p.run("prime"), "");
}

#[cfg(unix)]
#[test]
fn durable_store_canonical_alias_reopens() {
    let mut p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Shared through alias'");
    let alias = p.root.join("alias");
    std::os::unix::fs::symlink(&p.cwd, &alias).unwrap();
    p.cwd = alias;
    assert!(p.run("show repo-1").contains("Shared through alias"));
    assert!(
        p.run("init")
            .starts_with("Repository Store already initialized at ")
    );
}

#[test]
fn durable_store_pointer_scope_and_duplicates() {
    let p = Repo::new("repo");
    fs::write(
        p.root.join("global.gitconfig"),
        "[tk]\nstoreId = invalid-global\n",
    )
    .unwrap();
    let include = p.cwd.join(".git/included.config");
    fs::write(&include, "[tk]\nstoreId = invalid-include\n").unwrap();
    p.git(&["config", "--local", "include.path", "included.config"]);
    p.git(&["config", "--local", "extensions.worktreeConfig", "true"]);
    p.git(&["config", "--worktree", "tk.storeId", "invalid-worktree"]);
    assert!(
        p.run("init")
            .starts_with("Initialized Repository Store at ")
    );
    assert!(p.run("add -m 'Local authority'").contains("repo-1"));
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--add", "tk.storeId", &id]);
    for command in ["list", "init"] {
        assert!(p.run(command).contains("multiple repository-local"));
    }
    assert_eq!(p.run("prime"), "");
    p.git(&["config", "--local", "--replace-all", "tk.storeId", "../bad"]);
    assert!(p.run("list").contains("32 lowercase hexadecimal"));
    assert!(p.run("init").contains("32 lowercase hexadecimal"));
}

#[test]
fn durable_store_refuses_corrupt_future_or_mismatched_manifests() {
    for fault in ["corrupt", "future", "identity", "relative", "missing"] {
        let p = Repo::new("repo");
        p.run("init");
        let db = p.db_path();
        let path = db.with_file_name("store.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match fault {
            "corrupt" => fs::write(&path, "{").unwrap(),
            "missing" => fs::remove_file(&path).unwrap(),
            _ => {
                match fault {
                    "future" => manifest["version"] = 2.into(),
                    "identity" => manifest["store_id"] = "ffffffffffffffffffffffffffffffff".into(),
                    "relative" => manifest["association"]["git_common_dir"] = "relative".into(),
                    _ => unreachable!(),
                }
                fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            }
        }
        let before = fs::read(&db).unwrap();
        for command in ["list", "init"] {
            assert!(p.run(command).starts_with("exit 1\n"), "{fault}");
        }
        assert_eq!(p.run("prime"), "");
        assert_eq!(fs::read(&db).unwrap(), before);
    }
}

#[test]
fn durable_store_refuses_legacy_and_lost_pointers() {
    let p = Repo::new("repo");
    let legacy = p.cwd.join(".git/tk");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("tk.db"), "retained legacy data").unwrap();
    for command in ["init", "list"] {
        assert!(p.run(command).contains("legacy Repository Store"));
    }
    assert_eq!(
        fs::read_to_string(legacy.join("tk.db")).unwrap(),
        "retained legacy data"
    );
    assert_eq!(
        fs::read_dir(p.root.join("data/tk/stores")).unwrap().count(),
        0
    );
    fs::remove_dir_all(legacy).unwrap();
    p.run("init");
    p.run("add -m 'Keep this Store'");
    let db = p.db_path();
    p.git(&["config", "--local", "--unset", "tk.storeId"]);
    assert!(p.run("init").contains("Store evidence requires recovery"));
    assert!(p.run("list").contains("Repository Store not initialized"));
    assert!(db.is_file());
    assert_eq!(
        fs::read_dir(p.root.join("data/tk/stores")).unwrap().count(),
        1
    );
}

#[test]
fn durable_store_root_failures_never_choose_a_fallback() {
    let p = Repo::new("repo");
    for root in ["missing", "relative"] {
        assert!(
            p.run_env("init", &[("TK_TEST_DATA_ROOT", root)])
                .contains("data directory is unavailable or is not absolute")
        );
        assert_eq!(p.run_env("prime", &[("TK_TEST_DATA_ROOT", root)]), "");
    }
    fs::write(p.root.join("data"), "not a directory").unwrap();
    assert!(p.run("init").contains("filesystem error"));
    assert_eq!(
        fs::read_to_string(p.root.join("data")).unwrap(),
        "not a directory"
    );
    assert!(!p.cwd.join(".git/tk").exists());
}

#[test]
fn durable_store_collision_preserves_an_unrelated_store() {
    let mut p = Repo::new("repo");
    assert!(
        p.run_env("init", &[("TK_TEST_SEED", "42")])
            .starts_with("Initialized")
    );
    let db = p.db_path();
    let before = fs::read(&db).unwrap();
    let other = p.root.join("other");
    fs::create_dir(&other).unwrap();
    p.cwd = other;
    p.git(&["init", "-q"]);
    assert!(
        p.run_env("init", &[("TK_TEST_SEED", "42")])
            .contains("Store ID collision")
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(
        fs::read_dir(p.root.join("data/tk/stores")).unwrap().count(),
        1
    );
}

#[test]
fn durable_store_publication_failure_retains_recoverable_data() {
    let p = Repo::new("repo");
    fs::write(p.cwd.join(".git/config.lock"), "held by another writer").unwrap();
    assert!(
        p.run("init")
            .contains("failed to install repository-local Store config")
    );
    fs::remove_file(p.cwd.join(".git/config.lock")).unwrap();
    let stores = p.root.join("data/tk/stores");
    let dir = fs::read_dir(&stores)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(dir.join("tk.db").is_file());
    assert!(dir.join("store.json").is_file());
    assert!(p.run("init").starts_with("Attached Repository Store at "));
    assert_eq!(fs::read_dir(stores).unwrap().count(), 1);
}

#[test]
fn durable_store_evidence_is_exact_sorted_and_omits_credentials() {
    let p = Repo::new("repo");
    for (name, url) in [
        ("z", "https://example.invalid/z.git"),
        ("a", "https://example.invalid/a.git"),
        ("duplicate", "https://example.invalid/z.git"),
        ("userinfo", "https://user:secret@example.invalid/r.git"),
        ("query", "https://example.invalid/r.git?token=secret"),
        ("local", "/secret/path"),
        ("helper", "ext::secret command"),
    ] {
        p.git(&["config", "--local", &format!("remote.{name}.url"), url]);
    }
    p.run("init");
    let bytes = fs::read(p.db_path().with_file_name("store.json")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        manifest["evidence"]["git_remote_urls"],
        serde_json::json!([
            "https://example.invalid/a.git",
            "https://example.invalid/z.git"
        ])
    );
}

#[cfg(unix)]
#[test]
fn durable_store_tightens_only_new_directories() {
    use std::os::unix::fs::PermissionsExt;
    let p = Repo::new("repo");
    let data = p.root.join("data");
    fs::create_dir(&data).unwrap();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
    p.run("init");
    assert_eq!(
        fs::metadata(&data).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let db = p.db_path();
    for dir in [
        data.join("tk"),
        data.join("tk/stores"),
        db.parent().unwrap().to_path_buf(),
        db.with_file_name("backups"),
    ] {
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let store_dir = db.parent().unwrap();
    fs::set_permissions(store_dir, fs::Permissions::from_mode(0o750)).unwrap();
    p.run("init");
    assert_eq!(
        fs::metadata(store_dir).unwrap().permissions().mode() & 0o777,
        0o750
    );
}

#[test]
fn durable_store_incomplete_publication_blocks_fresh_init() {
    let p = Repo::new("repo");
    let orphan = p
        .root
        .join("data/tk/stores/00000000000000000000000000000000");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("tk.db"), "interrupted data").unwrap();
    assert!(p.run("init").contains("Store evidence requires recovery"));
    assert_eq!(
        fs::read_to_string(orphan.join("tk.db")).unwrap(),
        "interrupted data"
    );
    assert_eq!(fs::read_dir(orphan.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn durable_store_init_refuses_foreign_and_future_databases_unchanged() {
    for sql in [
        "pragma application_id = 1",
        "insert into schema_migrations(version, applied_at) values (9999, '2099-01-01T00:00:00.000Z')",
    ] {
        let p = Repo::new("repo");
        p.run("init");
        let db = p.db_path();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(sql).unwrap();
        conn.execute_batch("pragma journal_mode=delete").unwrap();
        drop(conn);
        let before = fs::read(&db).unwrap();
        assert!(p.run("init").starts_with("exit 1\n"));
        assert_eq!(fs::read(&db).unwrap(), before);
    }
}

#[test]
fn init_fresh() {
    let p = Repo::new("repo");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/data/tk/stores/[STORE_ID]/tk.db");
}

#[test]
fn init_idempotent() {
    let p = Repo::new("repo");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/data/tk/stores/[STORE_ID]/tk.db");
    tk!(p, "init", @"Repository Store already initialized at $TESTROOT/data/tk/stores/[STORE_ID]/tk.db");
}

#[test]
fn create_epic_child() {
    let p = Repo::new("project");
    tk!(p, "init", @"Initialized Repository Store at $TESTROOT/data/tk/stores/[STORE_ID]/tk.db");
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

/// The positional Scope argument end to end (ADR-0022): the real binary
/// resolves the Epic, filters to it, and prints the `Scope:` hint fenced off
/// the List Tree the way `render_chrome`'s rule line closes it below. A
/// failure means the positional argument, the fence, or the chrome around the
/// tree moved; the unit tests in `commands/list.rs` say which.
#[test]
fn list_scoped_to_an_epic_fences_the_scope_hint_from_the_tree() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Feature Epic'"); // project-1
    p.run("add --parent project-1 -m 'Build child Ticket'"); // project-2

    tk!(p, "list project-1", @r"
    Scope: project-1 (Epic + child Tickets)

    ○ project-1 [epic] Feature Epic
    └── ○ project-2 ● P2 Build child Ticket
    --------------------------------------------------------------------------------
    Total: 2 items (2 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");
}

/// The `Sync:` banner through the real binary, which no other scenario
/// reaches — they all run a quiet Mutation Log. Pins the whole chrome a
/// failed queue head produces, in one artifact: the banner stacked under the
/// ADR-0022 `Scope:` hint, the fence below the pair, the row's Mutation
/// marker, the `Mutations:` legend, and the `Mutation Log:` trailer below it.
/// A failure means one of those five moved, and the unit tests in
/// `commands/list.rs` say which.
///
/// The trailer counts the Mutation under an active Scope: it reports the
/// Mutation Log, not the rows in view.
#[test]
fn list_fences_a_stacked_scope_and_sync_banner_block_from_the_tree() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Feature Epic'"); // project-1
    p.run("add --parent project-1 -m 'Build child Ticket'"); // project-2

    // `failed` is one of the two queue-head states that earn a `Sync:` banner.
    p.seed_mutation("project-2", "failed", Some(r#"{"detail":"boom"}"#));

    tk!(p, "list project-1", @r"
    Scope: project-1 (Epic + child Tickets)
    Sync: Mutation 1 failed on project-2 (tk sync log 1)

    ○ project-1 [epic] Feature Epic
    └── ○ project-2 ● P2 ⚑ Build child Ticket
    --------------------------------------------------------------------------------
    Total: 2 items (2 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    Mutations: ⚑ failed

    Mutation Log: 1 failed
    ");
}

/// The `Mutation Log:` trailer where the summary chrome never runs: with
/// nothing open, `tk list` writes the empty-view line and the trailer follows
/// it. A `pending` Mutation earns no banner and a `done` Item reaches no row,
/// so here the trailer is the only mention of the queue in the output.
#[test]
fn list_reports_unresolved_mutations_below_the_empty_view_line() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Fix the bug'"); // project-1
    p.run("done project-1");

    p.seed_mutation("project-1", "pending", None);

    tk!(p, "list", @r"
    No open or active items.

    Mutation Log: 1 pending
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
fn show_renders_selection_state_for_tickets_only() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Capture me'"); // project-1
    p.run("add --epic -m 'Big work'"); // project-2

    // `tk show` surfaces a non-deterministic date, so assert on substrings: a
    // normal `tk add` Ticket is accepted (ADR-0027), rendered on its own line
    // directly under the facet bar.
    let ticket = p.run("show project-1");
    assert!(
        ticket.contains("\n  Selection: accepted\n"),
        "ticket={ticket}"
    );

    // Epics stay outside Selection State: no Selection line.
    let epic = p.run("show project-2");
    assert!(
        !epic.contains("Selection:"),
        "Epics omit Selection State: epic={epic}"
    );
}

/// A bodyless Item separates its first section from the header block, through
/// the real binary (gh-54). The child Ticket's PARENT follows a `Selection:`
/// line; the Epic's TICKETS follows the facet bar directly, the shortest
/// header block `tk show` renders. A failure means the separator regressed
/// for one of the two header shapes.
#[test]
fn show_separates_the_first_section_from_the_header_block() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Big work'"); // project-1
    p.run("add -m 'Small step' --parent project-1"); // project-2

    tk!(dated: p, "show project-2", @r"
    ○ project-2 · Small step
      P2 · Task · Created: [DATE]
      Selection: accepted

    PARENT
      ↑ ○ project-1: (Epic) Big work
    ");

    tk!(dated: p, "show project-1", @r"
    ○ project-1 · Big work
      Epic · Created: [DATE]

    TICKETS
      ↓ ○ project-2: Small step ● P2
    ");
}

#[test]
fn triage_capture_and_acceptance_flow() {
    let p = Repo::new("project");
    p.run("init");

    // Capture a triage bug — no Priority, surfaces its Selection State.
    tk!(p, "add --triage --bug -m 'Investigate flaky test'", @r"
    Created Ticket: project-1 - Investigate flaky test
    Kind: bug
    Selection: triage
    Status: open
    ");

    // Both AC2 rejections, asserted independently of the help doc-comment
    // (clap renders that prose verbatim and would not catch a dropped
    // conflict): `--triage --priority` and `--epic --triage` are exit 2.
    let conflict = p.run("add --triage --priority P1 -m 'nope'");
    assert!(conflict.contains("exit 2"), "conflict={conflict}");
    let epic_conflict = p.run("add --epic --triage -m 'nope'");
    assert!(
        epic_conflict.contains("exit 2"),
        "epic_conflict={epic_conflict}"
    );

    // tk next ignores triage; list --triage surfaces it.
    tk!(p, "next", @"
    exit 1
    -- stdout --
    -- stderr --
    tk next: no ready Tickets
    ");
    let triage_view = p.run("list --triage");
    assert!(
        triage_view.contains("project-1"),
        "triage_view={triage_view}"
    );

    // Reprioritizing via update is refused with a pointer to accept.
    let upd = p.run("update project-1 --priority P1");
    assert!(
        upd.contains("tk update: 'project-1' is in triage; set a Priority by accepting it with 'tk accept project-1 --priority Pn'"),
        "upd={upd}"
    );

    // Accept ranks it and makes it selectable.
    tk!(p, "accept project-1 --priority P1", @r"
    Accepted Ticket: project-1 - Investigate flaky test
    Priority: P1
    ");
    assert_eq!(p.run("next").trim(), "project-1: Investigate flaky test");

    // Re-accepting is an idempotent success.
    let again = p.run("accept project-1");
    assert!(
        again.contains("project-1 is already accepted"),
        "again={again}"
    );
}

#[test]
fn accepting_a_blocked_triage_ticket_preserves_the_blocker() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --triage -m 'Maybe later'"); // project-1 (triage)
    p.run("add -m 'Prerequisite'"); // project-2 (accepted)
    p.run("block project-1 project-2"); // project-2 blocks project-1

    // A blocked triage Ticket is excluded from the blocked view.
    let before = p.run("list --blocked");
    assert!(
        !before.contains("project-1"),
        "triage hidden from blocked: {before}"
    );

    p.run("accept project-1 --priority P1");

    // Acceptance preserves the blocker, so the now-accepted Ticket appears in
    // the blocked view rather than as ready work.
    let blocked = p.run("list --blocked");
    assert!(blocked.contains("project-1"), "blocked={blocked}");
    // project-1 is still blocked, so project-2 is the pick; it inherits
    // Effective Priority P1 from the blocked project-1, which is why the
    // stderr rationale names project-1 as the contributor.
    tk!(p, "next", @"
    exit 0
    -- stdout --
    project-2: Prerequisite
    -- stderr --
    project-2: Effective Priority P1 (via project-1)
    ");
}

#[test]
fn parking_and_unparking_flow() {
    let p = Repo::new("project");
    p.run("init");

    // An accepted Ticket is selectable by default.
    p.run("add -m 'Build the thing'"); // project-1 (accepted, P2)
    assert_eq!(p.run("next").trim(), "project-1: Build the thing");

    // Park it: the Priority is preserved and echoed (the held work stays ranked).
    tk!(p, "park project-1", @r"
    Parked Ticket: project-1 - Build the thing
    Priority: P2
    ");

    // Parked work drops out of automatic selection but stays visible: the
    // focused view lists it and plain list marks it with a dim [parked] badge.
    tk!(p, "next", @"
    exit 1
    -- stdout --
    -- stderr --
    tk next: no ready Tickets
    ");
    let parked_view = p.run("list --parked");
    assert!(
        parked_view.contains("project-1"),
        "parked_view={parked_view}"
    );
    let plain = p.run("list");
    assert!(plain.contains("[parked]"), "plain={plain}");

    // `tk show` reflects the parked Selection State (AC #5's third state).
    let parked_show = p.run("show project-1");
    assert!(
        parked_show.contains("\n  Selection: parked\n"),
        "parked_show={parked_show}"
    );

    // Re-parking is an idempotent success.
    let again = p.run("park project-1");
    assert!(
        again.contains("project-1 is already parked"),
        "again={again}"
    );

    // Parked work remains reprioritizable (AC: tk update --priority stays valid).
    p.run("update project-1 --priority P0");

    // Unpark restores it to selectable work at the (updated) held Priority,
    // without requiring a flag.
    tk!(p, "unpark project-1", @r"
    Unparked Ticket: project-1 - Build the thing
    Priority: P0
    ");
    assert_eq!(p.run("next").trim(), "project-1: Build the thing");

    // Re-unparking is an idempotent success.
    let reunpark = p.run("unpark project-1");
    assert!(
        reunpark.contains("project-1 is already accepted"),
        "reunpark={reunpark}"
    );

    // Triage work must be accepted before it can be parked.
    p.run("add --triage -m 'Maybe later'"); // project-2 (triage)
    let park_triage = p.run("park project-2");
    assert!(
        park_triage.contains(
            "tk park: 'project-2' is in triage; accept it first with 'tk accept project-2 --priority P0..P4'"
        ),
        "park_triage={park_triage}"
    );
}

#[test]
fn next_shows_the_title_by_default_and_quiet_restores_the_bare_id() {
    let p = Repo::new("project");
    p.run("init");

    p.run("add -m 'Ship the feature'"); // project-1 (accepted, P2)

    tk!(p, "next", @"project-1: Ship the feature");
    tk!(p, "next -q", @"project-1");
}

#[test]
fn plain_list_badges_every_selection_state() {
    // Lock the finished read contract (tk-78): in the default List Tree an
    // accepted Ticket carries no badge, a triage Ticket gets a dim `[triage]`,
    // and a parked Ticket a dim `[parked]`. The triage row is also blocked, so
    // the badge is pinned rendering alongside the `⊘` blocked indicator.
    let p = Repo::new("project");
    p.run("init");

    p.run("add -m 'Accepted work'"); // project-1 (accepted, P2)
    p.run("add --triage -m 'Captured idea'"); // project-2 (triage)
    p.run("add -m 'Held work'"); // project-3 (accepted, P2)
    p.run("park project-3"); // project-3 -> parked
    p.run("block project-2 project-1"); // triage Ticket carries an unresolved blocker

    tk!(p, "list", @"
    ○ project-1 ● P2 Accepted work
    ○ project-2 ⊘ [triage] Captured idea
    ○ project-3 ● P2 [parked] Held work
    --------------------------------------------------------------------------------
    Total: 3 items (3 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");

    // The view flags are primary and mutually exclusive: clap rejects a
    // combination at the parser layer with exit 2 (AC #4).
    let conflict = p.run("list --triage --ready");
    assert!(conflict.contains("exit 2"), "conflict={conflict}");
}

/// `tk done` fires on a Ticket that is *being worked*, which `start -> stop
/// -> done` never reaches: the close has to clear Work State as well as land
/// the Lifecycle. `tk list` renders the glyph for the first three steps; the
/// default List Tree drops closed work, so `tk search` — the one view that
/// spans every Item Status — shows the last.
#[test]
fn a_ticket_is_started_stopped_restarted_and_then_closed() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Auth rework'"); // project-1 (accepted, P2)

    tk!(p, "start project-1", @"Started Ticket: project-1 - Auth rework");
    tk!(p, "list", @"
    ◐ project-1 ● P2 Auth rework
    --------------------------------------------------------------------------------
    Total: 1 item (1 active)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");

    tk!(p, "stop project-1", @"Stopped Ticket: project-1 - Auth rework");
    tk!(p, "list", @"
    ○ project-1 ● P2 Auth rework
    --------------------------------------------------------------------------------
    Total: 1 item (1 open)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");

    // The restart is what gives the close something to clear; the glyph it
    // produces is already pinned by the first `list` above.
    tk!(p, "start project-1", @"Started Ticket: project-1 - Auth rework");

    tk!(p, "done project-1", @"Done Ticket: project-1 - Auth rework");
    tk!(p, "search auth", @"
    ✓ project-1 ● P2 Auth rework
    --------------------------------------------------------------------------------
    Total: 1 item (1 done)

    Status: ○ open  ◐ active  ✓ done
    Blocked: ⊘ blocked
    ");
}

#[test]
fn lifecycle_guards_respect_selection_state() {
    let p = Repo::new("project");
    p.run("init");

    // Triage work cannot be started — it must be accepted first.
    p.run("add --triage -m 'Maybe later'"); // project-1 (triage)
    let start_triage = p.run("start project-1");
    assert!(
        start_triage.contains(
            "tk start: 'project-1' is in triage; accept it first with 'tk accept project-1 --priority P0..P4'"
        ),
        "start_triage={start_triage}"
    );

    // Parked work cannot be started — it must be unparked first.
    p.run("add -m 'Held work'"); // project-2 (accepted)
    p.run("park project-2");
    let start_parked = p.run("start project-2");
    assert!(
        start_parked.contains(
            "tk start: 'project-2' is parked; unpark it first with 'tk unpark project-2'"
        ),
        "start_parked={start_parked}"
    );

    // Active work cannot be parked directly — stop it first, then park.
    p.run("add -m 'Active work'"); // project-3 (accepted)
    p.run("start project-3");
    let park_active = p.run("park project-3");
    assert!(
        park_active
            .contains("tk park: 'project-3' is active; stop it first with 'tk stop project-3'"),
        "park_active={park_active}"
    );
    p.run("stop project-3");
    tk!(p, "park project-3", @r"
    Parked Ticket: project-3 - Active work
    Priority: P2
    ");

    // done closes captured work from any Selection State without accepting it.
    tk!(p, "done project-1 -m 'Not doing it'", @"Done Ticket: project-1 - Maybe later");
}

#[test]
fn init_refuses_outside_git_repository() {
    let p = Repo::without_git("scratch");
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

/// `tk manpage` writes the embedded bytes verbatim. The binary embeds this
/// same file, so a failure here means the build is stale — not that two
/// copies of the manual drifted apart.
#[test]
fn manpage_emits_embedded_manpage() {
    let p = Repo::new("repo");
    let expected = fs::read_to_string(repo_root().join("man/tk.1")).expect("read man/tk.1");
    assert_eq!(p.run("manpage"), expected);
}

/// A failure means tk(1) still describes a subsystem ADR-0022 removed — fix
/// the prose, don't relax the list. Whitespace collapses first because groff
/// prose wraps mid-phrase, so a retired term can straddle two source lines.
#[test]
fn manpage_uses_no_retired_vocabulary() {
    let manpage = fs::read_to_string(repo_root().join("man/tk.1")).expect("read man/tk.1");
    let flattened = manpage.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in ["Workspace Scope", "tk worktree"] {
        assert!(
            !flattened.contains(phrase),
            "man/tk.1 still says {phrase:?}, retired by ADR-0022"
        );
    }
}

#[test]
fn prime_emits_context_and_commands() {
    let p = Repo::new("repo");
    p.run("init");
    let output = p.run("prime");
    assert!(
        output.starts_with("# tk Context\n\n## Current Work\n"),
        "{output}"
    );
    assert!(output.contains("Next: no ready Tickets\n"));
    assert!(output.contains("Active: none\n"));
    assert!(output.contains("Plan: empty\n"));
    assert!(output.contains("## Finding Work\n"));
    assert!(output.contains("tk start <id>"));
    assert!(output.contains("## Plan\n"));
    assert!(output.contains("tk --help"));
    assert!(output.contains("man tk"));
    for absent in ["tk sync", "tk promote", "Remote", "Backend", "Recovery"] {
        assert!(!output.contains(absent), "unexpected {absent}: {output}");
    }
    assert!(output.ends_with('\n'));
    assert!(!output.ends_with("\n\n"));
    assert!(!output.contains('\r'));
}

#[test]
fn prime_selects_from_the_plan_and_shows_all_progress() {
    let p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'First ready' -p P3");
    p.run("add -m 'Urgent ready' -p P1");
    p.run("add -m 'Unplanned' -p P0");
    p.run("add --epic -m 'Active Epic'");
    p.run("add --parent repo-4 -m 'Active child'");
    p.run("add -m 'Finished'");
    p.run("start repo-4");
    p.run("start repo-5");
    p.run("done repo-6");
    p.run("plan add repo-1 repo-2 repo-5 repo-6");
    let output = p.run("prime");
    let current = output.split("## Finding Work").next().unwrap();
    assert!(
        current.contains("Next in Plan: repo-2: Urgent ready\n"),
        "{current}"
    );
    assert!(current.contains("`tk start repo-2`"));
    assert!(current.contains("◐ repo-4 [epic] Active Epic"));
    assert!(current.contains("◐ repo-5 ● P2 Active child"));
    assert!(current.contains(&p.run("plan")));
    assert!(!current.contains("Unplanned"));
    p.run("block repo-1 repo-3");
    p.run("block repo-2 repo-3");
    let output = p.run("prime");
    assert!(output.contains("Next in Plan: no ready Tickets\n"));
    assert!(!output.contains("`tk start repo-3`"));
    p.run("plan clear");
    assert!(p.run("prime").contains("Next: repo-3: Unplanned\n"));
    p.run("plan add repo-6");
    let output = p.run("prime");
    assert!(output.contains("Next in Plan: no ready Tickets\n"));
    assert!(output.contains("0 remaining · 1/1 done\n"));
}

#[test]
fn prime_scope_limits_current_work_but_keeps_the_whole_plan() {
    let p = Repo::new("repo");
    p.run("init");
    p.run("add --epic -m 'Feature'");
    p.run("add --parent repo-1 -m 'Child' -p P2");
    p.run("add -m 'Outside' -p P0");
    p.run("add --parent repo-1 -m 'Busy child'");
    p.run("add -m 'Busy outside'");
    p.run("start repo-4");
    p.run("start repo-5");
    p.run("plan add repo-2 repo-3");
    let output = p.run_env("prime", &[("TK_SCOPE", "repo-1")]);
    assert!(
        output.contains("Scope: repo-1 (Epic + child Tickets)\n"),
        "{output}"
    );
    assert!(output.contains("Next in Plan within Scope repo-1: repo-2: Child\n"));
    assert!(output.contains("◐ repo-4 ● P2 Busy child"));
    assert!(!output.contains("Busy outside"));
    assert!(output.contains(&p.run("plan")));
    p.run("plan clear");
    assert!(
        p.run_env("prime", &[("TK_SCOPE", "repo-1")])
            .contains("Next within Scope repo-1: repo-2: Child\n")
    );
    p.run("plan add repo-3");
    assert!(
        p.run_env("prime", &[("TK_SCOPE", "repo-1")])
            .contains("Next in Plan within Scope repo-1: no ready Tickets\n")
    );
    for (scope, reason) in [
        ("missing", "not a known Display ID or Alias"),
        ("repo-2", "not an Epic"),
    ] {
        let output = p.run_env("prime", &[("TK_SCOPE", scope)]);
        let current = output.split("## Finding Work").next().unwrap();
        assert!(
            current.contains(&format!("Warning: scope '{scope}' is {reason}")),
            "{current}"
        );
        assert!(!current.contains("Next"));
        assert!(!current.contains("Active"));
        assert!(current.contains(&p.run("plan")));
    }
}

#[test]
fn prime_remote_counts_are_facts_without_recovery_instructions() {
    let p = Repo::new("repo");
    p.run("init");
    p.run("remote set github");
    p.run("add -m 'Work'");
    let clean = p.run("prime");
    assert!(clean.contains("Mutation Log: clean\n"), "{clean}");
    assert!(clean.contains("## Remote Work\n"));
    assert!(clean.contains("tk sync log"));
    assert!(!clean.contains('\r'));
    assert!(clean.ends_with('\n') && !clean.ends_with("\n\n"));
    let headings: Vec<_> = clean
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect();
    assert_eq!(
        headings,
        [
            "## Current Work",
            "## Finding Work",
            "## Creating and Updating",
            "## Dependencies",
            "## Plan",
            "## Scope",
            "## Remote Work"
        ]
    );
    p.seed_mutation("repo-1", "pending", None);
    let conn = rusqlite::Connection::open(p.db_path()).unwrap();
    for (sequence, state, kind, failure) in [
        (
            2,
            "failed",
            "update_ticket",
            Some(r#"{"detail":"refused"}"#),
        ),
        (3, "applying", "promote_ticket", None),
        (4, "skipped", "update_ticket", None),
        (5, "cancelled", "update_ticket", None),
        (6, "abandoned", "promote_ticket", None),
    ] {
        conn.execute(
            "insert into mutations(sequence, mutation_type, item_id, item_class, payload_json, state, failure_json, created_at, state_changed_at)
             select ?1, ?2, item_id, item_class, payload_json, ?3, ?4, created_at, state_changed_at from mutations where sequence = 1",
            rusqlite::params![sequence, kind, state, failure],
        ).unwrap();
    }
    let output = p.run("prime");
    let current = output.split("## Finding Work").next().unwrap();
    assert!(current.contains("Mutation Log: 1 pending · 1 failed · 1 applying · 1 skipped · 1 cancelled · 1 abandoned\n"), "{current}");
    for instruction in ["tk sync", "tk promote", "Inspect", "Recovery"] {
        assert!(!current.contains(instruction), "{current}");
    }
    assert_eq!(
        output.split("## Finding Work").nth(1),
        clean.split("## Finding Work").nth(1)
    );
    conn.execute(
        "update mutations set state = 'applied', failure_json = null",
        [],
    )
    .unwrap();
    assert!(p.run("prime").contains("Mutation Log: clean\n"));
}

#[test]
fn prime_does_not_cap_active_items_or_plan_members() {
    let p = Repo::new("repo");
    p.run("init");
    for i in 1..=25 {
        p.run(&format!("add -m 'Work {i}'"));
        p.run(&format!("start repo-{i}"));
    }
    let members = (1..=25)
        .map(|i| format!("repo-{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    p.run(&format!("plan add {members}"));
    let output = p.run("prime");
    let active = output
        .split("Active (Store context):\n")
        .nth(1)
        .unwrap()
        .split("Plan (whole Store):")
        .next()
        .unwrap();
    assert_eq!(
        active
            .lines()
            .filter(|line| line.contains("◐ repo-"))
            .count(),
        25
    );
    assert!(output.contains(&p.run("plan")));
    assert!(output.contains("25 remaining · 0/25 done"));
}

#[test]
fn prime_sanitizes_current_work_and_scope_warnings() {
    let p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Work'");
    let conn = rusqlite::Connection::open(p.db_path()).unwrap();
    conn.execute("update items set title = ?1", ["Title\r\nnext\x1b[31m"])
        .unwrap();
    p.run("plan add repo-1");
    for started in [false, true] {
        if started {
            p.run("start repo-1");
        }
        let output = p.run("prime");
        assert!(!output.contains('\r'));
        assert!(!output.contains('\x1b'));
        assert!(output.contains("Title  next\\x1b[31m"), "{output}");
    }
    let output = p.run_env("prime", &[("TK_SCOPE", "bad\nScope\x07")]);
    assert!(
        output.contains("Warning: scope 'bad Scope\\x07'"),
        "{output}"
    );
}

#[test]
fn prime_read_failure_reports_only_a_diagnostic() {
    for table in ["plan_members", "item_ids", "remotes"] {
        let p = Repo::new("repo");
        p.run("init");
        let conn = rusqlite::Connection::open(p.db_path()).unwrap();
        conn.execute(&format!("drop table {table}"), []).unwrap();
        let output = p.run_env("prime", &[("TK_SCOPE", "missing")]);
        assert!(
            output.starts_with(
                "exit 1\n-- stdout --\n-- stderr --\ntk prime: failed to read Repository Store\n"
            ),
            "{table}: {output}"
        );
        assert!(!output.contains("# tk Context"));
    }
}

#[test]
fn prime_is_silent_for_unopenable_stores() {
    for fault in ["foreign", "future", "corrupt"] {
        let p = Repo::new("repo");
        p.run("init");
        let path = p.db_path();
        if fault == "corrupt" {
            fs::write(path, "not a SQLite database").unwrap();
        } else {
            let conn = rusqlite::Connection::open(path).unwrap();
            let sql = if fault == "foreign" {
                "pragma application_id = 0"
            } else {
                "insert into schema_migrations(version, applied_at) values (999999, '2026-09-11T00:00:00.000Z'); pragma user_version = 999999"
            };
            conn.execute_batch(sql).unwrap();
        }
        assert_eq!(p.run("prime"), "", "{fault}");
    }
}

#[test]
fn prime_is_silent_without_initialized_store() {
    let p = Repo::new("repo");
    assert_eq!(p.run("prime"), "");
}

#[test]
fn prime_is_silent_outside_git_repository() {
    let p = Repo::without_git("scratch");
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
    insta::assert_snapshot!("help_tk", p.run("--help"));
    for command in [
        "accept", "add", "block", "detach", "done", "grep", "init", "list", "next", "park", "plan",
        "promote", "search", "show", "sync", "unblock", "unpark", "update",
    ] {
        insta::assert_snapshot!(
            format!("help_{command}"),
            p.run(&format!("{command} --help"))
        );
    }
    insta::assert_snapshot!("help_promote_reconcile", p.run("promote reconcile --help"));
    insta::assert_snapshot!("help_promote_retry", p.run("promote retry --help"));
    insta::assert_snapshot!("help_promote_cancel", p.run("promote cancel --help"));
    for subcommand in ["add", "remove", "clear"] {
        insta::assert_snapshot!(
            format!("help_plan_{subcommand}"),
            p.run(&format!("plan {subcommand} --help"))
        );
    }
}

#[test]
fn detach_adopted_ticket_through_cli_dispatch() {
    let p = Repo::new("project");
    p.run("init");
    let db_path = p.db_path();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "insert into items( \
                id, display_value, item_class, ticket_kind, priority, title, body, origin, \
                backend_kind, backend_key, status, work_state, selection_state, created_seq, \
                created_at, updated_at \
             ) values ( \
                'stable-id', 'gh-42', 'ticket', 'task', 'P2', 'Backend work', 'Details', \
                'backend', 'github', 'https://github.com/o/r/issues/42', 'open', 'idle', \
                'accepted', 1, '2026-05-01T00:00:00.000Z', '2026-05-01T00:00:00.000Z' \
             )",
            [],
        )
        .unwrap();
        tx.execute(
            "insert into item_ids(value, source, item_id, created_at) \
             values ('gh-42', 'display', 'stable-id', '2026-05-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        tx.execute(
            "insert into mutations( \
                sequence, mutation_type, item_id, item_class, payload_json, state, \
                created_at, state_changed_at \
             ) values ( \
                1, 'update_ticket', 'stable-id', 'ticket', \
                '{\"title\":\"Backend work\",\"body\":\"Details\"}', 'pending', \
                '2026-05-01T00:00:00.000Z', '2026-05-01T00:00:00.000Z' \
             )",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    tk!(p, "detach gh-42", @r"
    Detached: Backend Ticket gh-42 → Local Ticket project-1
    Backend object left unchanged: https://github.com/o/r/issues/42
    Withdrew update_ticket for project-1 (Mutation 1)
    ");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let current: (String, String, Option<String>, Option<String>) = conn
        .query_row(
            "select id, origin, backend_kind, backend_key from items where display_value = 'project-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(current, ("stable-id".into(), "local".into(), None, None));
    let former: String = conn
        .query_row(
            "select backend_key from former_backend_identities where item_id = 'stable-id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(former, "https://github.com/o/r/issues/42");
    let old_resolver_rows: i64 = conn
        .query_row(
            "select count(*) from item_ids where value = 'gh-42'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_resolver_rows, 0);
    let withdrawn: String = conn
        .query_row(
            "select state from mutations where sequence = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(withdrawn, "cancelled");
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
    tk!(dated: p, "grep auth", @"
    ○ project-2 · Add middleware
      P2 · Task · Created: [DATE]
      the handler validates the auth token

    ○ project-3 · Refactor auth layer
      P2 · Task · Created: [DATE]
    ");
}

/// `-i` flips grep's case-sensitive default (ADR-0026) for one invocation, so
/// the lowercase pattern now hits the capitalised `Auth` epic title (tk-117).
#[test]
fn grep_ignore_case_matches_across_case() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Auth rework'"); // project-1 (capital A)
    p.run("add -m 'Unrelated chore'"); // project-2 (no match)

    tk!(dated: p, "grep auth -i", @"
    ○ project-1 · Auth rework
      Epic · Created: [DATE]
    ");
}

/// `-F` matches the pattern as a literal (ADR-0026, tk-120): `a(b` is an invalid
/// regex (unbalanced group) but a valid literal needle, so `-F` finds it where
/// the bare pattern would be a usage error.
#[test]
fn grep_fixed_strings_matches_a_literal() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Fix parser' -m 'the token a(b breaks the lexer'"); // project-1

    tk!(dated: p, "grep 'a(b' -F", @"
    ○ project-1 · Fix parser
      P2 · Task · Created: [DATE]
      the token a(b breaks the lexer
    ");
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

    tk!(dated: p, "grep needle -C 0", @"
    ○ project-1 · Subject
      P2 · Task · Created: [DATE]
      second needle paragraph
    ");
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

#[test]
fn grep_list_prints_matching_items_in_creation_order_and_ignores_scope() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add --epic -m 'Release auth' -m 'auth rollout'"); // project-1
    p.run("add -m 'Auth middleware' -m 'auth once' -m 'auth twice'"); // project-2
    p.run("done project-2");
    p.run("add --epic -m 'Other scope'"); // project-3

    insta::assert_snapshot!(
        p.run_env("grep auth -ilF -C 0", &[("TK_SCOPE", "project-3")]),
        @"
    project-1: Release auth
    project-2: Auth middleware
    "
    );
    tk!(p, "grep auth -i --list", @"
    project-1: Release auth
    project-2: Auth middleware
    ");
}

#[test]
fn grep_list_no_match_is_silent() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Unrelated chore'");

    tk!(p, "grep absent -l", @"
    exit 1
    -- stdout --
    -- stderr --
    ");
}

#[test]
fn grep_list_prints_single_title_only_and_body_only_matches() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Title needle' -m 'Body token'");
    p.run("add -m 'Unrelated chore'");

    tk!(p, "grep needle -l", @"project-1: Title needle");
    tk!(p, "grep token --list", @"project-1: Title needle");
}

/// Reject conflicting output modes rather than let branch order pick one.
#[test]
fn grep_output_modes_conflict() {
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
    tk!(p, "grep auth -q -c", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--quiet' cannot be used with '--count'

    Usage: tk grep --quiet <PATTERN>

    For more information, try '--help'.
    ");
    tk!(p, "grep auth -l -q", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--list' cannot be used with '--quiet'

    Usage: tk grep --list <PATTERN>

    For more information, try '--help'.
    ");
    tk!(p, "grep auth -q -l", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--quiet' cannot be used with '--list'

    Usage: tk grep --quiet <PATTERN>

    For more information, try '--help'.
    ");
    tk!(p, "grep auth -l -c", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--list' cannot be used with '--count'

    Usage: tk grep --list <PATTERN>

    For more information, try '--help'.
    ");
    tk!(p, "grep auth -c -l", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the argument '--count' cannot be used with '--list'

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
    // A truly-empty pattern is still rejected (it would match every line);
    // a whitespace pattern is not (ADR-0026, amended) — covered below.
    tk!(p, "grep ''", @"
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

/// A whitespace pattern is a valid needle, matched like grep/ripgrep rather than
/// rejected as empty (ADR-0026, amended): `tk grep '  '` finds the body line with
/// a double space, and `-F` makes a whitespace literal explicit.
#[test]
fn grep_whitespace_pattern_matches_a_double_space() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Format output' -m 'aligns the  columns by padding'"); // project-1 (double space)
    p.run("add -m 'Unrelated chore'"); // project-2 (no double space)

    tk!(dated: p, "grep '  ' -F", @"
    ○ project-1 · Format output
      P2 · Task · Created: [DATE]
      aligns the  columns by padding
    ");
}

// `tk promote` scenarios cover paths that cannot open a capable Adapter:
// argument validation and absence of a Remote. Capability preflight and
// creation use FakeRunner unit tests (ADR-0031), never a live GitHub write.

#[test]
fn promote_without_a_remote_is_refused() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Local work'"); // project-1

    tk!(p, "promote project-1", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: no Remote configured; run 'tk remote set <kind>' first
    ");
}

#[test]
fn promote_an_unknown_id_names_what_was_typed() {
    let p = Repo::new("project");
    p.run("init");

    tk!(p, "promote project-404", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: 'project-404' is not a known Display ID or Alias
    ");
}

#[test]
fn promote_children_on_a_ticket_is_a_usage_error() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Local work'"); // project-1

    tk!(p, "promote project-1 --children", @"
    exit 2
    -- stdout --
    -- stderr --
    tk promote: 'project-1' is not an Epic; --children promotes the Promotion Children of an Epic
    ");
}

#[test]
fn promote_recovery_subcommands_resolve_their_explicit_target() {
    let p = Repo::new("project");
    p.run("init");

    tk!(p, "promote reconcile project-404 42", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: 'project-404' is not a known Display ID or Alias
    ");
    tk!(p, "promote retry project-404", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: 'project-404' is not a known Display ID or Alias
    ");
    tk!(p, "promote cancel project-404", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: 'project-404' is not a known Display ID or Alias
    ");
}

#[test]
fn cancelling_an_item_with_no_promotion_intent_is_refused() {
    let p = Repo::new("project");
    p.run("init");
    p.run("add -m 'Local work'"); // project-1

    tk!(p, "promote cancel project-1", @"
    exit 1
    -- stdout --
    -- stderr --
    tk promote: 'project-1' has no nonterminal Promotion to recover
    ");
}

#[test]
fn sync_log_reports_withdrawn_mutations_without_a_flag() {
    let p = Repo::new("project");
    p.run("init");

    tk!(p, "sync log", @"No Mutations recorded.");
    tk!(p, "sync log --cancelled", @"No cancelled Mutations.");
    tk!(p, "sync log --abandoned", @"No abandoned Mutations.");
}

#[test]
fn promote_recovery_subcommands_conflict_with_creation_arguments() {
    let p = Repo::new("project");

    tk!(p, "promote project-1 reconcile project-1 42", @"
    exit 2
    -- stdout --
    -- stderr --
    error: the subcommand 'reconcile' cannot be used with '<ID>'

    Usage: tk promote [OPTIONS] <ID>
           tk promote <COMMAND>

    For more information, try '--help'.
    ");
}

#[test]
fn recovery_explicitly_reattaches_a_moved_repository() {
    let mut p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Survives move'");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    let moved = p.root.join("moved");
    fs::rename(&p.cwd, &moved).unwrap();
    let original = p.cwd.clone();
    p.cwd = moved;
    assert!(
        p.run(&format!("init --attach {id}"))
            .contains("ownership is live or unknown")
    );
    fs::create_dir(&original).unwrap();
    let guidance = p.run("init");
    assert!(guidance.starts_with("exit 1\n"), "{guidance}");
    assert!(
        guidance.contains(&format!("tk init --attach {id}")),
        "{guidance}"
    );
    let attached = p.run(&format!("init --attach {id}"));
    assert!(
        attached.starts_with("Attached Repository Store at "),
        "{attached}"
    );
    assert!(p.run("show repo-1").contains("Survives move"));
    assert!(p.run("init --new").contains("healthy Store Association"));
}

#[test]
fn recovery_repairs_missing_and_multiple_pointers_explicitly() {
    for duplicate in [false, true] {
        let p = Repo::new("repo");
        p.run("init");
        p.run("add -m 'Preserved'");
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        if duplicate {
            p.git(&["config", "--local", "--add", "tk.storeId", "broken"]);
        } else {
            p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        }
        let out = p.run("init");
        assert!(out.starts_with("exit 1\n"), "{out}");
        assert!(out.contains(&format!("tk init --attach {id}")), "{out}");
        if !duplicate {
            insta::assert_snapshot!(
                "recovery_missing_pointer",
                out.replace(&id, "<store-id>").replace('\\', "/")
            );
        }
        let out = p.run(&format!("init --attach {id}"));
        assert!(out.starts_with("Attached Repository Store at "), "{out}");
        assert!(p.run("show repo-1").contains("Preserved"));
    }
}

#[test]
fn recovery_new_preserves_the_prior_store_and_never_recreates_a_missing_id() {
    let p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Preserved'");
    let db = p.db_path();
    let old = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    assert!(
        p.run("init --new")
            .starts_with("Initialized Repository Store at ")
    );
    assert_ne!(p.git(&["config", "--local", "--get", "tk.storeId"]), old);
    assert!(db.is_file());
    p.git(&[
        "config",
        "--local",
        "tk.storeId",
        "11111111111111111111111111111111",
    ]);
    assert!(p.run("init").contains("possible data loss"));
    let out = p.run("init --new");
    assert!(
        out.contains("Initialized Repository Store at ") && out.contains("possible data loss"),
        "{out}"
    );
    assert!(
        !p.root
            .join("data/tk/stores/11111111111111111111111111111111")
            .exists()
    );
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    assert!(
        p.run(&format!("init --attach {old}"))
            .starts_with("Attached Repository Store at ")
    );
    assert!(p.run("show repo-1").contains("Preserved"));
}

#[test]
fn recovery_live_owner_refuses_then_manual_release_allows_reclone() {
    let mut p = Repo::new("repo");
    p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
    p.run("init");
    p.run("add -m 'Shared history is not ownership'");
    let original = p.cwd.clone();
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.cwd = p.root.join("clone");
    fs::create_dir(&p.cwd).unwrap();
    p.git(&["init", "-q"]);
    p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
    let out = p.run("init");
    assert!(out.contains("exact Git remote URL"), "{out}");
    assert!(!out.contains(&format!("tk init --attach {id}")), "{out}");
    assert!(
        p.run(&format!("init --attach {id}"))
            .contains("another Git Common Directory")
    );
    let clone = p.cwd.clone();
    p.cwd = original;
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    p.cwd = clone;
    assert!(
        p.run(&format!("init --attach {id}"))
            .starts_with("Attached Repository Store at ")
    );
    assert!(
        p.run("show repo-1")
            .contains("Shared history is not ownership")
    );
}

#[test]
fn recovery_refuses_unknown_git_ownership_and_retries_interrupted_pointer_writes() {
    for failure in ["former", "replace-before", "replace-after"] {
        let mut p = Repo::new("repo");
        p.run("init");
        p.run("add -m 'Keep through interruption'");
        let db = p.db_path();
        let manifest = db.with_file_name("store.json");
        let before = fs::read(&manifest).unwrap();
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        p.cwd = p.root.join("clone");
        fs::create_dir(&p.cwd).unwrap();
        p.git(&["init", "-q"]);
        let command = format!("init --attach {id}");
        let out = p.run_env(&command, &[("TK_TEST_GIT_FAILURE", failure)]);
        assert!(out.starts_with("exit 1\n"), "{out}");
        if failure == "former" {
            assert_eq!(fs::read(&manifest).unwrap(), before);
        }
        let retry = if failure == "replace-after" {
            "init"
        } else {
            &command
        };
        let out = p.run(retry);
        assert!(!out.starts_with("exit "), "{out}");
        assert!(p.run("show repo-1").contains("Keep through interruption"));
    }
}

#[test]
fn recovery_open_handle_blocks_ownership_transfer_until_it_closes() {
    let mut p = Repo::new("repo");
    p.run("init");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    let original = p.cwd.clone();
    let store = tk::store::repository::open_existing(
        &tk::proc::RealRunner::new(),
        &p.cwd,
        &tk::clock::RealClock::new(),
        Some(&p.root.join("data")),
    )
    .unwrap();
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    p.cwd = p.root.join("clone");
    fs::create_dir(&p.cwd).unwrap();
    p.git(&["init", "-q"]);
    assert!(
        p.run(&format!("init --attach {id}"))
            .contains("retry when it finishes")
    );
    drop(store);
    assert!(
        p.run(&format!("init --attach {id}"))
            .starts_with("Attached Repository Store at ")
    );
    p.cwd = original;
    p.git(&["config", "--local", "tk.storeId", &id]);
    assert!(p.run("add -m 'Stale owner'").starts_with("exit 1\n"));
}

#[test]
fn recovery_two_repositories_cannot_both_attach_the_same_store() {
    let p = Repo::new("repo");
    p.run("init");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for name in ["first", "second"] {
        let cwd = p.root.join(name);
        fs::create_dir(&cwd).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&cwd)
                .status()
                .unwrap()
                .success()
        );
        let root = p.root.clone();
        let id = id.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            support::run(&cwd, &root, &["init".into(), "--attach".into(), id], &[])
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|r| r.status.success()).count(),
        1,
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .filter(|r| !r.status.success())
            .all(|r| r.status.code() == Some(1))
    );
}

#[test]
fn recovery_invalid_metadata_and_databases_never_offer_attachment() {
    for fault in [
        "missing",
        "corrupt",
        "identity",
        "future",
        "database",
        "foreign",
        "future-database",
        "pending-only",
    ] {
        let p = Repo::new("repo");
        p.run("init");
        let db = p.db_path();
        let manifest = db.with_file_name("store.json");
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        let original = fs::read(&manifest).unwrap();
        match fault {
            "missing" => fs::remove_file(&manifest).unwrap(),
            "corrupt" => fs::write(&manifest, "broken").unwrap(),
            "identity" | "future" => {
                let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                if fault == "identity" {
                    value["store_id"] = "ffffffffffffffffffffffffffffffff".into();
                } else {
                    value["version"] = 2.into();
                }
                fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "database" => fs::remove_file(&db).unwrap(),
            "foreign" | "future-database" => {
                let conn = rusqlite::Connection::open(&db).unwrap();
                if fault == "foreign" {
                    conn.execute_batch("pragma application_id = 0").unwrap();
                } else {
                    conn.execute_batch(
                        "insert into schema_migrations(version, applied_at) values (999, 'now')",
                    )
                    .unwrap();
                }
            }
            "pending-only" => fs::rename(
                &manifest,
                manifest.with_extension("json.interrupted.pending"),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let before = fs::read(&manifest).ok();
        let out = p.run("init");
        assert!(out.starts_with("exit 1\n"), "{fault}: {out}");
        assert!(
            !out.contains(&format!("tk init --attach {id}")),
            "{fault}: {out}"
        );
        assert!(
            p.run(&format!("init --attach {id}"))
                .starts_with("exit 1\n")
        );
        assert_eq!(fs::read(&manifest).ok(), before, "{fault}");
        assert_eq!(
            fs::read_dir(p.root.join("data/tk/stores")).unwrap().count(),
            1
        );
    }
}

#[test]
fn recovery_ranks_all_matching_facts_without_migrating_candidates() {
    let p = Repo::new("repo");
    p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
    let mut ids = Vec::new();
    for _ in 0..5 {
        assert!(
            p.run("init --new")
                .contains("Initialized Repository Store at ")
        );
        ids.push(p.git(&["config", "--local", "--get", "tk.storeId"]));
        p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    }
    ids.sort();
    let common = fs::canonicalize(p.cwd.join(".git")).unwrap();
    let stores = p.root.join("data/tk/stores");
    for (i, id) in ids.iter().enumerate() {
        let path = stores.join(id).join("store.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if i != 3 {
            manifest["association"]["git_common_dir"] =
                p.root.join("gone/.git").to_str().unwrap().into();
        }
        if i == 2 || i == 4 {
            manifest["evidence"]["previous_git_common_dirs"] = serde_json::json!([common]);
        }
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    p.git(&["config", "--local", "tk.storeId", &ids[4]]);
    let candidate_db = stores.join(&ids[3]).join("tk.db");
    {
        let conn = rusqlite::Connection::open(&candidate_db).unwrap();
        conn.execute_batch("delete from schema_migrations where version = (select max(version) from schema_migrations)").unwrap();
    }
    let before = fs::read(&candidate_db).unwrap();
    let out = p.run("init");
    let positions: Vec<_> = [4, 3, 2, 0, 1]
        .iter()
        .map(|i| out.find(&format!("Store \"{}\":", ids[*i])).unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{out}");
    assert!(
        out.contains("referenced Store ID")
            && out.contains("historical canonical path")
            && out.contains("associated current canonical path")
            && out.contains("exact Git remote URL"),
        "{out}"
    );
    assert_eq!(fs::read(candidate_db).unwrap(), before);
    assert_eq!(
        fs::read_dir(stores.join(&ids[3]).join("backups"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn recovery_remote_spelling_is_exact_and_options_are_mutually_exclusive() {
    let mut p = Repo::new("repo");
    p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
    p.run("init");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    assert!(
        p.run(&format!("init --new --attach {id}"))
            .starts_with("exit 2\n")
    );
    assert!(p.run("init --force").starts_with("exit 2\n"));
    p.cwd = p.root.join("clone");
    fs::create_dir(&p.cwd).unwrap();
    p.git(&["init", "-q"]);
    p.git(&["remote", "add", "origin", "https://example.com/repo"]);
    assert!(
        p.run("init")
            .starts_with("Initialized Repository Store at ")
    );
}

#[test]
fn recovery_copied_config_refuses_live_owner_but_accepts_reused_path() {
    let mut p = Repo::new("repo");
    p.git(&[
        "remote",
        "add",
        "origin",
        "https://example.com/original.git",
    ]);
    p.run("init");
    p.run("add -m 'Retained across path reuse'");
    let db = p.db_path();
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    let original = p.cwd.clone();
    p.cwd = p.root.join("copy");
    fs::create_dir(&p.cwd).unwrap();
    p.git(&["init", "-q"]);
    fs::copy(original.join(".git/config"), p.cwd.join(".git/config")).unwrap();
    assert!(
        p.run(&format!("init --attach {id}"))
            .contains("another Git Common Directory")
    );
    let copy = p.cwd.clone();
    fs::remove_dir_all(original.join(".git")).unwrap();
    p.cwd = original.clone();
    p.git(&["init", "-q"]);
    p.cwd = copy;
    p.git(&["remote", "set-url", "origin", "https://example.com/new.git"]);
    assert!(
        p.run(&format!("init --attach {id}"))
            .starts_with("Attached Repository Store at ")
    );
    assert!(p.run("show repo-1").contains("Retained across path reuse"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(db.with_file_name("store.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["evidence"]["previous_git_common_dirs"],
        serde_json::json!([fs::canonicalize(original.join(".git")).unwrap()])
    );
    assert_eq!(
        manifest["evidence"]["git_remote_urls"],
        serde_json::json!([
            "https://example.com/new.git",
            "https://example.com/original.git"
        ])
    );
}

#[test]
fn recovery_manifest_publication_failure_keeps_the_valid_manifest_for_retry() {
    use rand::SeedableRng;
    let mut p = Repo::new("repo");
    p.run("init");
    p.run("add -m 'Keep through manifest failure'");
    let db = p.db_path();
    let manifest = db.with_file_name("store.json");
    let before = fs::read(&manifest).unwrap();
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    p.cwd = p.root.join("clone");
    fs::create_dir(&p.cwd).unwrap();
    p.git(&["init", "-q"]);
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let staged_id = tk::store::association::StoreId::generate(&mut rng);
    let staged = db.with_file_name(format!("store.json.{}.pending", staged_id.text()));
    fs::write(&staged, "interrupted publication").unwrap();
    let out = p.run_env(&format!("init --attach {id}"), &[("TK_TEST_SEED", "42")]);
    assert!(out.starts_with("exit 1\n"), "{out}");
    assert_eq!(fs::read(&manifest).unwrap(), before);
    assert_eq!(
        fs::read_to_string(&staged).unwrap(),
        "interrupted publication"
    );
    assert!(
        p.run(&format!("init --attach {id}"))
            .starts_with("Attached Repository Store at ")
    );
    assert!(
        p.run("show repo-1")
            .contains("Keep through manifest failure")
    );
}

#[test]
fn vacant_recovery_repairs_a_missing_pointer_and_allows_ordinary_access() {
    let p = Repo::new("repo");
    p.run("init");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    let out = p.run("init");
    assert!(out.starts_with("Attached Repository Store at "), "{out}");
    assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
    assert!(p.run("add -m 'After repair'").contains("repo-1"));
    assert!(p.run("show repo-1").contains("After repair"));
}

#[test]
fn vacant_recovery_preserves_each_kind_of_user_data() {
    for category in [
        "ticket", "epic", "remote", "plan", "sequence", "config", "other",
    ] {
        let p = Repo::new("repo");
        p.run("init");
        let db = p.db_path();
        match category {
            "ticket" => {
                p.run("add -m 'Preserved'");
            }
            "epic" => {
                p.run("add --epic -m 'Preserved'");
            }
            "plan" => {
                p.run("add -m 'Preserved'");
                p.run("plan add repo-1");
            }
            _ => {
                let conn = rusqlite::Connection::open(&db).unwrap();
                conn.execute_batch(match category {
                    "remote" => "insert into remotes values ('primary', 'github', '{}', 'now', 'now')",
                    "sequence" => "update sequences set value = 1 where name = 'display_seq'",
                    "config" => "pragma ignore_check_constraints = on; insert into store_config values ('user_setting', 'keep')",
                    "other" => "create table user_notes(body text); insert into user_notes values ('keep')",
                    _ => unreachable!(),
                }).unwrap();
            }
        }
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        let before = fs::read(&db).unwrap();
        p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        let out = p.run("init");
        assert!(out.starts_with("exit 1\n"), "{category}: {out}");
        assert_eq!(fs::read(&db).unwrap(), before, "{category}");
        assert!(p.run("list").contains("Repository Store not initialized"));
        assert!(db.parent().unwrap().join("store.json").exists());
        assert!(out.contains(&id), "{out}");
    }
}

#[test]
fn vacant_recovery_preserves_every_mutation_state() {
    use tk::domain::mutation_state::MutationState;
    for state in MutationState::ALL {
        let p = Repo::new("repo");
        p.run("init");
        p.run("add -m 'Preserved'");
        let db = p.db_path();
        p.seed_mutation("repo-1", "pending", None);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "update mutations set mutation_type = ?1, state = ?2, failure_json = ?3",
            rusqlite::params![
                if matches!(
                    state,
                    MutationState::Applying | MutationState::Cancelled | MutationState::Abandoned
                ) {
                    "promote_ticket"
                } else {
                    "update_ticket"
                },
                state.text(),
                if state == MutationState::Failed {
                    Some("{}")
                } else {
                    None
                }
            ],
        )
        .unwrap();
        drop(conn);
        let before = fs::read(&db).unwrap();
        p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        let out = p.run("init");
        assert!(out.starts_with("exit 1\n"), "{state:?}: {out}");
        assert_eq!(fs::read(db).unwrap(), before);
    }
}

#[test]
fn vacant_recovery_inspects_every_backup_without_changing_it() {
    for contents in [
        "vacant",
        "ticket",
        "corrupt",
        "future",
        "missing_table",
        "directory",
        "old_vacant",
    ] {
        let p = Repo::new("repo");
        p.run("init");
        let db = p.db_path();
        let backup = db.parent().unwrap().join("backups/retained.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "vacuum into ?1",
                [backup.parent().unwrap().join("vacant.db").to_str().unwrap()],
            )
            .unwrap();
        if contents == "ticket" {
            p.run("add -m 'Only in backup'");
        }
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("vacuum into ?1", [backup.to_str().unwrap()])
                .unwrap();
            if contents == "ticket" {
                conn.execute_batch("pragma foreign_keys = off; delete from item_ids; delete from items; update sequences set value = 0").unwrap();
            }
        }
        match contents {
            "corrupt" => fs::write(&backup, "broken backup").unwrap(),
            "directory" => {
                fs::remove_file(&backup).unwrap();
                fs::create_dir(&backup).unwrap();
            }
            "future" | "missing_table" | "old_vacant" => {
                let conn = rusqlite::Connection::open(&backup).unwrap();
                conn.execute_batch(match contents {
                    "future" => "insert into schema_migrations values (999, 'now'); pragma user_version = 999",
                    "missing_table" => "drop table plan_members",
                    "old_vacant" => "drop table plan_members; delete from schema_migrations where version = 17; pragma user_version = 16",
                    _ => unreachable!(),
                }).unwrap();
            }
            _ => {}
        }
        let before = fs::read(&backup).ok();
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        let out = p.run("init");
        if matches!(contents, "vacant" | "old_vacant") {
            assert!(
                out.starts_with("Attached Repository Store at "),
                "{contents}: {out}"
            );
            assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
        } else {
            assert!(out.starts_with("exit 1\n"), "{contents}: {out}");
        }
        assert_eq!(fs::read(&backup).ok(), before, "{contents}");
        assert_eq!(fs::read_dir(backup.parent().unwrap()).unwrap().count(), 2);
    }
}

#[test]
fn vacant_recovery_requires_unique_identity_and_released_ownership() {
    for evidence in [
        "referenced",
        "live",
        "unknown",
        "remote",
        "history",
        "unrelated",
        "multiple",
        "malformed",
    ] {
        let mut p = Repo::new("repo");
        p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
        p.run("init");
        let db = p.db_path();
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        let original = p.cwd.clone();
        if evidence != "live" {
            p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        }
        if evidence == "multiple" {
            p.run("init --new");
            p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
        } else if evidence == "malformed" {
            p.git(&["config", "--local", "tk.storeId", "broken"]);
        } else {
            p.cwd = p.root.join("clone");
            fs::create_dir(&p.cwd).unwrap();
            p.git(&["init", "-q"]);
            match evidence {
                "referenced" | "live" | "unknown" => {
                    p.git(&["config", "--local", "tk.storeId", &id]);
                }
                "remote" => {
                    p.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
                }
                "history" => {
                    let path = db.parent().unwrap().join("store.json");
                    let mut manifest: serde_json::Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    manifest["evidence"]["previous_git_common_dirs"] =
                        serde_json::json!([fs::canonicalize(p.cwd.join(".git")).unwrap()]);
                    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                }
                _ => {}
            }
            if evidence == "unknown" {
                fs::rename(&original, p.root.join("moved-away")).unwrap();
            }
        }
        let before = fs::read(db.parent().unwrap().join("store.json")).unwrap();
        let out = p.run("init");
        match evidence {
            "referenced" => {
                assert!(out.starts_with("Attached Repository Store at "), "{out}");
                assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
                assert!(p.run("add -m 'Reused'").contains("repo-1"));
            }
            "unrelated" => {
                assert!(out.starts_with("Initialized Repository Store at "), "{out}");
                assert_ne!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
                assert_eq!(
                    fs::read(db.parent().unwrap().join("store.json")).unwrap(),
                    before
                );
            }
            _ => {
                assert!(out.starts_with("exit 1\n"), "{evidence}: {out}");
                assert_eq!(
                    fs::read(db.parent().unwrap().join("store.json")).unwrap(),
                    before
                );
            }
        }
    }
}

#[test]
fn vacant_recovery_excludes_open_writers_and_rechecks_after_they_close() {
    let p = Repo::new("repo");
    p.run("init");
    let store = tk::store::repository::open_existing(
        &tk::proc::RealRunner::new(),
        &p.cwd,
        &tk::clock::RealClock::new(),
        Some(&p.root.join("data")),
    )
    .unwrap();
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    let out = p.run("init");
    assert!(out.contains("retry when it finishes"), "{out}");
    store
        .conn()
        .execute_batch("update sequences set value = 1 where name = 'display_seq'")
        .unwrap();
    drop(store);
    let out = p.run("init");
    assert!(out.starts_with("exit 1\n"), "{out}");
    assert!(out.contains("Store evidence requires recovery"), "{out}");
    assert!(p.run("list").contains("Repository Store not initialized"));
}

#[cfg(unix)]
#[test]
fn vacant_recovery_refuses_an_unreadable_backup() {
    use std::os::unix::fs::PermissionsExt;
    let p = Repo::new("repo");
    p.run("init");
    let backup = p.db_path().parent().unwrap().join("backups/unreadable.db");
    fs::copy(p.db_path(), &backup).unwrap();
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o000)).unwrap();
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    let out = p.run("init");
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(out.starts_with("exit 1\n"), "{out}");
    assert!(p.run("list").contains("Repository Store not initialized"));
}

#[test]
fn vacant_recovery_accepts_defaults_seeded_from_a_linked_workspace() {
    let mut p = Repo::new("repo");
    p.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--allow-empty",
        "-qm",
        "Initial",
    ]);
    let linked = p.root.join("linked");
    p.git(&["worktree", "add", "-qb", "linked", "../linked"]);
    p.cwd = linked;
    p.run("init");
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    p.git(&["config", "--local", "--unset-all", "tk.storeId"]);
    let out = p.run("init");
    assert!(out.starts_with("Attached Repository Store at "), "{out}");
    assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
    assert!(p.run("add -m 'After repair'").contains("linked-1"));
}

#[test]
fn store_lifecycle_preserves_work_through_migration_linked_access_and_recovery() {
    let mut p = Repo::new("legacy");
    p.run("init");
    p.run("add --epic -m 'Release'");
    p.run("add --bug -m 'Keep this work' -P legacy-1");
    p.run("start legacy-2");
    p.run("plan add legacy-2");
    let shown = p.run("show legacy-2");
    let plan = p.run("plan");
    let original = p.db_path();
    let backup = original.parent().unwrap().join("backups/manual.db");
    rusqlite::Connection::open(&original)
        .unwrap()
        .execute("vacuum into ?1", [backup.to_str().unwrap()])
        .unwrap();
    let backup_bytes = fs::read(&backup).unwrap();
    let legacy = p.move_to_legacy();
    assert!(p.run("list").contains("run 'tk init'"));
    assert_eq!(p.run("prime"), "");
    let result = p.run("init");
    assert!(result.contains("Migrated Repository Store"), "{result}");
    assert_eq!(p.run("show legacy-2"), shown);
    assert_eq!(p.run("plan"), plan);
    assert_eq!(
        fs::read(p.db_path().parent().unwrap().join("backups/manual.db")).unwrap(),
        backup_bytes
    );
    assert!(!legacy.exists());
    let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
    assert!(p.run("init").contains("already initialized"));
    assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
    p.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--allow-empty",
        "-m",
        "Initial",
    ]);
    p.git(&["worktree", "add", "-q", "../linked"]);
    let main = p.cwd.clone();
    p.cwd = p.root.join("linked");
    assert_eq!(p.run("show legacy-2"), shown);
    assert!(
        p.run("update legacy-2 --title 'Worked from linked Workspace'")
            .contains("Updated")
    );
    p.cwd = main;
    let shown = p.run("show legacy-2");
    assert!(shown.contains("Worked from linked Workspace"));
    p.git(&["worktree", "remove", "../linked"]);
    let original = p.cwd.clone();
    let moved = p.root.join("moved checkout å");
    fs::rename(&original, &moved).unwrap();
    p.cwd = moved;
    fs::create_dir(&original).unwrap();
    let guidance = p.run("init");
    assert!(guidance.starts_with("exit 1\n"), "{guidance}");
    assert!(
        guidance.contains(&format!("tk init --attach {id}")),
        "{guidance}"
    );
    assert_eq!(p.run("prime"), "");
    p.git(&["config", "--local", "--unset", "tk.storeId"]);
    let interrupted = p.run_env(
        &format!("init --attach {id}"),
        &[("TK_TEST_GIT_FAILURE", "replace-before")],
    );
    assert!(interrupted.starts_with("exit 1\n"), "{interrupted}");
    assert!(
        p.run(&format!("init --attach {id}"))
            .starts_with("Attached Repository Store at ")
    );
    assert_eq!(p.run("show legacy-2"), shown);
    assert_eq!(
        p.run("plan"),
        plan.replace("Keep this work", "Worked from linked Workspace")
    );
    assert_eq!(p.run("sync log"), "No Mutations recorded.\n");
    assert_eq!(
        fs::read(p.db_path().parent().unwrap().join("backups/manual.db")).unwrap(),
        backup_bytes
    );
    assert!(p.run("add -m 'After recovery'").contains("legacy-3"));
}

fn legacy_repo() -> Repo {
    let p = Repo::new("legacy");
    p.run("init");
    p.run("add -m 'Before migration'");
    p.run("plan add legacy-1");
    let dir = p.db_path().parent().unwrap().to_path_buf();
    let backup = dir.join("backups/retained.db");
    rusqlite::Connection::open(dir.join("tk.db"))
        .unwrap()
        .execute("vacuum into ?1", [backup.to_str().unwrap()])
        .unwrap();
    p.move_to_legacy();
    p
}

#[test]
fn legacy_migration_resumes_after_process_exit_at_every_boundary() {
    for boundary in [
        "Recorded",
        "Reserved",
        "Staged",
        "Validated",
        "Published",
        "Pointed",
        "Cleanup",
        "CleanedFile",
        "Removed",
        "Finished",
    ] {
        let p = legacy_repo();
        let backup = fs::read(p.cwd.join(".git/tk/backups/retained.db")).unwrap();
        let out = p.run_env(
            "init",
            &[("TK_TEST_MIGRATION_FAILURE", &format!("crash:{boundary}"))],
        );
        assert!(out.starts_with("exit 99"), "{boundary}: {out}");
        let progress: serde_json::Value =
            serde_json::from_slice(&fs::read(p.cwd.join(".git/tk-migration.json")).unwrap())
                .unwrap();
        let id = progress["store_id"].as_str().unwrap();
        let result = p.run("init");
        assert!(
            result.contains("Migrated Repository Store"),
            "{boundary}: {result}"
        );
        assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
        assert!(p.run("show legacy-1").contains("Before migration"));
        assert!(p.run("plan").contains("legacy-1"));
        assert_eq!(
            fs::read(p.db_path().parent().unwrap().join("backups/retained.db")).unwrap(),
            backup
        );
        assert!(!p.cwd.join(".git/tk").exists());
    }
}

#[test]
fn legacy_migration_rebuilds_before_pointer_and_preserves_divergence_after_it() {
    for boundary in ["Published", "Pointed"] {
        let p = legacy_repo();
        let out = p.run_env("init", &[("TK_TEST_MIGRATION_FAILURE", boundary)]);
        assert!(out.contains(&format!("interrupted at {boundary}")), "{out}");
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(p.cwd.join(".git/tk-migration.json")).unwrap())
                .unwrap();
        let id = record["store_id"].as_str().unwrap();
        let source = p.cwd.join(".git/tk/tk.db");
        let conn = rusqlite::Connection::open(&source).unwrap();
        conn.execute("update items set title = 'Later legacy write'", [])
            .unwrap();
        drop(conn);
        if boundary == "Pointed" {
            assert!(
                p.run("update legacy-1 --title 'New authoritative write'")
                    .contains("Updated")
            );
            let out = p.run("init");
            assert!(out.contains("legacy files changed after cutover"), "{out}");
            assert!(p.run("show legacy-1").contains("New authoritative write"));
            assert!(source.is_file());
            assert_eq!(
                rusqlite::Connection::open(&source)
                    .unwrap()
                    .query_row("select title from items", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "Later legacy write"
            );
        } else {
            let out = p.run("init");
            assert!(out.contains("Migrated Repository Store"), "{out}");
            assert!(p.run("show legacy-1").contains("Later legacy write"));
            assert!(!source.exists());
        }
        assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
    }
}

#[test]
fn legacy_migration_refuses_old_wal_connections_and_includes_uncheckpointed_data() {
    let p = legacy_repo();
    let path = p.cwd.join(".git/tk/tk.db");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.pragma_update(None, "journal_mode", "wal").unwrap();
    old.execute("update items set title = 'Old process write'", [])
        .unwrap();
    let out = p.run("init");
    assert!(out.contains("database is locked"), "{out}");
    assert!(path.is_file());
    drop(old);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "legacy_wal_child", "--ignored"])
        .env("TK_TEST_LEGACY_DATABASE", &path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        fs::metadata(path.with_file_name("tk.db-wal"))
            .unwrap()
            .len()
            > 32
    );
    let out = p.run("init");
    assert!(out.contains("Migrated Repository Store"), "{out}");
    assert!(p.run("show legacy-1").contains("Uncheckpointed WAL write"));
}

#[test]
#[ignore = "legacy SQLite process exits without closing its WAL connection"]
fn legacy_wal_child() {
    let path = std::env::var_os("TK_TEST_LEGACY_DATABASE").unwrap();
    let conn = rusqlite::Connection::open(PathBuf::from(path)).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute("update items set title = 'Uncheckpointed WAL write'", [])
        .unwrap();
    std::process::exit(0);
}

#[test]
fn legacy_migration_excludes_competing_init_attach_open_and_writes() {
    let p = legacy_repo();
    let gate = tempfile::tempdir().unwrap();
    let cwd = p.cwd.clone();
    let root = p.root.clone();
    let gate_path = gate.path().to_path_buf();
    let migration = std::thread::spawn(move || {
        support::run(
            &cwd,
            &root,
            &["init".into()],
            &[
                ("TK_TEST_MIGRATION_FAILURE", "pause:Published"),
                ("TK_TEST_MIGRATION_GATE", gate_path.to_str().unwrap()),
            ],
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !gate.path().join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "migration must reach publication"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(p.cwd.join(".git/tk-migration.json")).unwrap()).unwrap();
    let id = record["store_id"].as_str().unwrap();
    for command in [
        "init".to_string(),
        "init --new".to_string(),
        format!("init --attach {id}"),
    ] {
        assert!(
            p.run(&command)
                .contains("another Store lifecycle operation")
        );
    }
    assert!(p.run("list").contains("legacy Repository Store"));
    assert!(
        p.run("update legacy-1 --title 'Must not write'")
            .contains("legacy Repository Store")
    );
    let old = rusqlite::Connection::open(p.cwd.join(".git/tk/tk.db")).unwrap();
    old.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(
        old.execute("update items set title = 'Must not write'", [])
            .is_err()
    );
    drop(old);
    fs::write(gate.path().join("release"), "").unwrap();
    let out = migration.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(p.run("show legacy-1").contains("Before migration"));
    assert_eq!(p.git(&["config", "--local", "--get", "tk.storeId"]), id);
}

#[test]
fn legacy_migration_preserves_all_rows_and_mutation_states() {
    let p = Repo::new("legacy");
    for command in [
        "init",
        "remote set github",
        "add --epic -m 'Epic'",
        "add --bug -p P1 -P legacy-1 -m 'Child'",
        "add -m 'Done'",
        "done legacy-3 -m 'Kept reason'",
        "add --triage -m 'Triage'",
        "add -m 'Parked'",
        "park legacy-5",
        "start legacy-2",
        "block legacy-5 legacy-2",
        "plan add legacy-2 legacy-3 legacy-4 legacy-5",
    ] {
        let out = p.run(command);
        assert!(!out.starts_with("exit"), "{command}: {out}");
    }
    p.seed_mutation("legacy-2", "pending", None);
    let db = p.db_path();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("update items set body = 'Retained body with unicode: å', updated_at = '2026-01-02T03:04:05.006Z' where display_value = 'legacy-2'", []).unwrap();
    conn.execute("insert into item_ids(value, source, item_id, created_at) select 'old-2', 'alias', id, created_at from items where display_value = 'legacy-2'", []).unwrap();
    for (sequence, state, kind, failure) in [
        (
            2,
            "failed",
            "update_ticket",
            Some(r#"{"detail":"refused"}"#),
        ),
        (3, "applying", "promote_ticket", None),
        (4, "applied", "update_ticket", None),
        (5, "skipped", "update_ticket", None),
        (6, "cancelled", "update_ticket", None),
        (7, "abandoned", "promote_ticket", None),
    ] {
        conn.execute("insert into mutations(sequence, mutation_type, item_id, item_class, payload_json, state, failure_json, created_at, state_changed_at) select ?1, ?2, item_id, item_class, payload_json, ?3, ?4, created_at, state_changed_at from mutations where sequence = 1", rusqlite::params![sequence, kind, state, failure]).unwrap();
    }
    conn.execute(
        "update sequences set value = 7 where name = 'mutation_seq'",
        [],
    )
    .unwrap();
    drop(conn);
    let before = database_rows(&db);
    let show = p.run("show old-2");
    let log = p.run("sync log");
    p.move_to_legacy();
    let interrupted = p.run_env("init", &[("TK_TEST_MIGRATION_FAILURE", "crash:Pointed")]);
    assert!(interrupted.starts_with("exit 99\n"), "{interrupted}");
    let out = p.run("init");
    assert!(out.contains("Migrated Repository Store"), "{out}");
    assert_eq!(database_rows(&p.db_path()), before);
    assert_eq!(p.run("show old-2"), show);
    assert_eq!(p.run("sync log"), log);
    assert!(p.run("add -m 'Next ID'").contains("legacy-6"));
}

fn database_rows(path: &Path) -> Vec<(String, Vec<String>)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let tables = conn.prepare("select name from sqlite_schema where type = 'table' and name not like 'sqlite_%' order by name").unwrap()
        .query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut query = conn
                .prepare(&format!("select * from \"{}\"", table.replace('"', "\"\"")))
                .unwrap();
            let columns = query.column_count();
            let mut rows = query
                .query_map([], |r| {
                    (0..columns)
                        .map(|i| r.get::<_, rusqlite::types::Value>(i))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .map(|row| format!("{:?}", row.unwrap()))
                .collect::<Vec<_>>();
            rows.sort();
            (table, rows)
        })
        .collect()
}

#[test]
fn legacy_migration_requires_its_receipt_and_blocks_orphan_attachment() {
    let p = legacy_repo();
    assert!(p.run("init --new").contains("legacy Repository Store"));
    p.run_env("init", &[("TK_TEST_MIGRATION_FAILURE", "Published")]);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(p.cwd.join(".git/tk-migration.json")).unwrap()).unwrap();
    let id = record["store_id"].as_str().unwrap();
    assert!(
        p.run(&format!("init --attach {id}"))
            .contains("legacy Repository Store")
    );
    let other = Repo::new("other");
    let out = other.run_env(
        &format!("init --attach {id}"),
        &[("TK_TEST_DATA_ROOT", p.root.join("data").to_str().unwrap())],
    );
    assert!(out.contains("migration is pending"), "{out}");
    let destination = p.root.join("data/tk/stores").join(id);
    let before = fs::read(destination.join("tk.db")).unwrap();
    fs::remove_file(destination.join("migration.json")).unwrap();
    let out = p.run("init");
    assert!(out.starts_with("exit 1"), "{out}");
    assert_eq!(fs::read(destination.join("tk.db")).unwrap(), before);
    assert!(p.cwd.join(".git/tk/tk.db").is_file());
}

#[test]
fn legacy_migration_retains_source_on_storage_failures_and_retries() {
    for failure in ["full:Staged", "deny:Validated", "deny:Cleanup"] {
        let p = legacy_repo();
        let backup = fs::read(p.cwd.join(".git/tk/backups/retained.db")).unwrap();
        let out = p.run_env("init", &[("TK_TEST_MIGRATION_FAILURE", failure)]);
        assert!(out.starts_with("exit 1"), "{failure}: {out}");
        assert!(p.cwd.join(".git/tk/tk.db").is_file());
        assert_eq!(
            fs::read(p.cwd.join(".git/tk/backups/retained.db")).unwrap(),
            backup
        );
        let out = p.run("init");
        assert!(
            out.contains("Migrated Repository Store"),
            "{failure}: {out}"
        );
        assert!(p.run("show legacy-1").contains("Before migration"));
    }
}

#[cfg(unix)]
#[test]
fn legacy_migration_refuses_hard_links_without_losing_sqlite_locks() {
    let p = legacy_repo();
    let source = p.cwd.join(".git/tk");
    fs::hard_link(source.join("tk.db"), source.join("backups/hardlink.db")).unwrap();
    let out = p.run("init");
    assert!(out.contains("legacy database has hard links"), "{out}");
    assert!(source.join("tk.db").is_file());
    fs::remove_file(source.join("backups/hardlink.db")).unwrap();
    assert!(p.run("init").contains("Migrated Repository Store"));
}

#[test]
fn healthy_store_work_does_not_write_git_config_or_refresh_manifest() {
    let p = Repo::new("repo");
    p.run("init");
    let manifest = p.db_path().with_file_name("store.json");
    let before = fs::read(&manifest).unwrap();
    let modified = fs::metadata(&manifest).unwrap().modified().unwrap();
    let config = p.cwd.join(".git/config");
    let config_before = fs::read(&config).unwrap();
    let config_modified = fs::metadata(&config).unwrap().modified().unwrap();
    fs::write(p.cwd.join(".git/config.lock"), "held by another process").unwrap();
    assert!(p.run("add -m 'Normal work'").contains("repo-1"));
    assert!(p.run("start repo-1").contains("Started"));
    assert!(p.run("plan add repo-1").contains("Added to Plan"));
    assert!(p.run("show repo-1").contains("Normal work"));
    assert!(p.run("prime").contains("Normal work"));
    assert_eq!(p.run("sync log"), "No Mutations recorded.\n");
    assert_eq!(fs::read(&manifest).unwrap(), before);
    assert_eq!(
        fs::metadata(&manifest).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(fs::read(&config).unwrap(), config_before);
    assert_eq!(
        fs::metadata(&config).unwrap().modified().unwrap(),
        config_modified
    );
}

#[cfg(unix)]
#[test]
fn native_data_root_uses_an_isolated_home() {
    let p = Repo::new("repo");
    let home = p.root.join("home å");
    fs::create_dir(&home).unwrap();
    let xdg = home.join("xdg data");
    for use_xdg in [false, true] {
        let env = [
            ("TK_TEST_DATA_ROOT", "native"),
            ("HOME", home.to_str().unwrap()),
            (
                "XDG_DATA_HOME",
                if use_xdg { xdg.to_str().unwrap() } else { "" },
            ),
        ];
        let result = p.run_env("init", &env);
        assert!(
            result.starts_with("Initialized Repository Store at "),
            "{result}"
        );
        let id = p.git(&["config", "--local", "--get", "tk.storeId"]);
        let data = if cfg!(target_os = "macos") {
            home.join("Library/Application Support")
        } else if use_xdg {
            xdg.clone()
        } else {
            home.join(".local/share")
        };
        let store = data.join("tk/stores").join(id);
        assert!(store.join("tk.db").is_file());
        assert!(p.run_env("add -m 'Native root'", &env).contains("repo-1"));
        p.git(&["config", "--local", "--unset", "tk.storeId"]);
        fs::remove_dir_all(data.join("tk")).unwrap();
    }
}
