//! Parallel-writer tests for the Repository Store (tk-10).
//!
//! Several agents may run write commands against one Repository Store at
//! nearly the same time. The store's answer is `BEGIN IMMEDIATE` on every
//! write transaction ([`store::write_transaction`]) plus the connection's
//! 5-second `busy_timeout`: concurrent writers queue instead of failing.
//!
//! A failure here means a write path regressed to a deferred transaction:
//! a read-then-write upgrade gets `SQLITE_BUSY` immediately — the busy
//! timeout does not apply to a snapshot upgrade — and the command aborts
//! with "Repository Store is busy" long before the timeout could matter.
//! The fix is to route the offending path through `write_transaction`, not
//! to widen the timeout.
//!
//! Real subprocesses throughout, per ADR-0031: each writer is the built
//! `tk` binary racing the others through the OS, exactly like concurrent
//! agent sessions.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

const WRITERS: usize = 12;

fn init_store() -> TempDir {
    let dir = TempDir::new().expect("create scratch dir");
    let git = Command::new("git")
        .args(["init", "-q", "."])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    assert!(git.status.success(), "git init failed");
    let init = tk(dir.path(), &["init"]);
    assert!(init.status.success(), "tk init failed");
    dir
}

fn tk(cwd: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("tk")
        .expect("tk binary")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CEILING_DIRECTORIES", cwd.parent().unwrap())
        .output()
        .expect("spawn tk")
}

/// Run one `tk` invocation per writer, released simultaneously by a barrier,
/// and return each writer's `Output`.
fn storm(dir: &TempDir, argv_per_writer: Vec<Vec<String>>) -> Vec<Output> {
    let barrier = Arc::new(Barrier::new(argv_per_writer.len()));
    let handles: Vec<_> = argv_per_writer
        .into_iter()
        .map(|argv| {
            let barrier = Arc::clone(&barrier);
            let cwd = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                barrier.wait();
                tk(&cwd, &args)
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

fn failures(outputs: &[Output]) -> Vec<String> {
    outputs
        .iter()
        .filter(|o| !o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
        .collect()
}

/// Seed one Ticket per writer sequentially and return their Display IDs,
/// parsed from the `Created Ticket: <id> - <title>` line.
fn seed_tickets(dir: &TempDir, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let out = tk(dir.path(), &["add", "-m", &format!("seed {i}")]);
            assert!(out.status.success(), "seed add {i} failed");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("Created Ticket: "))
                .and_then(|l| l.split(" - ").next())
                .expect("Display ID in tk add output")
                .to_string()
        })
        .collect()
}

#[test]
fn parallel_adds_all_succeed_with_distinct_display_ids() {
    let dir = init_store();
    let outputs = storm(
        &dir,
        (0..WRITERS)
            .map(|i| vec!["add".into(), "-m".into(), format!("parallel {i}")])
            .collect(),
    );
    assert_eq!(failures(&outputs), Vec::<String>::new());

    // Distinct Display IDs prove sequence allocation serialized: two writers
    // sharing a `display_seq` read would collide here.
    let ids: HashSet<String> = outputs
        .iter()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("Created Ticket: "))
                .and_then(|l| l.split(" - ").next())
                .expect("Display ID in tk add output")
                .to_string()
        })
        .collect();
    assert_eq!(ids.len(), WRITERS);
}

#[test]
fn parallel_updates_on_distinct_tickets_all_succeed() {
    let dir = init_store();
    let ids = seed_tickets(&dir, WRITERS);
    let outputs = storm(
        &dir,
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                vec![
                    "update".into(),
                    id.clone(),
                    "-m".into(),
                    format!("racer {i}"),
                ]
            })
            .collect(),
    );
    assert_eq!(failures(&outputs), Vec::<String>::new());
}

#[test]
fn parallel_status_transitions_all_succeed() {
    let dir = init_store();
    let ids = seed_tickets(&dir, WRITERS);
    let outputs = storm(
        &dir,
        ids.iter()
            .map(|id| vec!["start".into(), id.clone()])
            .collect(),
    );
    assert_eq!(failures(&outputs), Vec::<String>::new());

    let outputs = storm(
        &dir,
        ids.iter()
            .map(|id| vec!["done".into(), id.clone()])
            .collect(),
    );
    assert_eq!(failures(&outputs), Vec::<String>::new());
}
