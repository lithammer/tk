//! `tk adopt` — bring one existing Backend issue into the Repository Store as a
//! Backend Ticket (ADR-0034: Adopt is the sole Backend → tk intake path in v1).
//!
//! `tk adopt <key>` eagerly fetches the single issue named by `<key>` through
//! the configured Backend Adapter and inserts it as an `accepted` Backend
//! Ticket (Display ID `gh-<n>` for GitHub). It is the inverse intake direction
//! to Promotion and records the Backend's current state without a Mutation.
//!
//! `<key>` is the backend-native identifier (a bare issue number for GitHub),
//! passed to the adapter verbatim — tk does not normalise URLs or `#`-prefixes,
//! because the command is backend-agnostic and the Adapter owns
//! canonicalization. The Store checks canonical Backend identity under the
//! insertion transaction.
//!
//! Canonical identity ownership spans former history too: when `<key>`
//! canonicalizes to a Former Backend Identity, intake is Re-Adopt — the
//! original Item is rebound to that Backend object and takes its current
//! shared fields, instead of a second Item appearing for one Backend object
//! (ADR-0047).
//!
//! Per ADR-0032, [`run`] returns `Result<Exit, CommandError>` and the dispatch
//! seam frames failures as `tk adopt: <body>`.

use std::fmt::Write as _;

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
use crate::commands::resolver;
use crate::domain::dependency_rule::DependencyRejection;
use crate::domain::relationship_plan::{RelationshipFinding, RelationshipItem};
use crate::remote::adapter::AdapterReadError;
use crate::remote::factory::{self, OpenError as FactoryOpenError};
use crate::store::sync::{self as store_sync, AdoptOutcome, AdoptStoreError, BackendCohortError};

/// Flags for `tk adopt`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Backend-native key of the issue to adopt (a bare issue number for
    /// GitHub). Passed to the Backend Adapter verbatim — tk does not normalise
    /// URLs or `#`-prefixes before Adapter canonicalization.
    #[arg(value_name = "KEY")]
    pub key: String,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    let mut store = resolver::open_for_command(deps.runner, deps.cwd, deps.clock)
        .map_err(|err| resolver::open_error(&err))?;
    let now = deps.clock.now_iso();
    let _workflow = store
        .lock_remote_workflow()
        .map_err(CommandError::failure)?;
    store_sync::ensure_adopt_available(store.conn()).map_err(adopt_store_error)?;

    let Some(expected) = store_sync::configured_remote_kind(store.conn())
        .map_err(|err| resolver::storage_error(&err))?
    else {
        return Err(no_remote());
    };
    if let Some(row) = store_sync::find_adopted_ticket(store.conn(), expected, &args.key)
        .map_err(adopt_store_error)?
    {
        let _ = writeln!(deps.stdout, "Already adopted: {}", row.display_id);
        return Ok(Exit::Ok);
    }

    let adapter_opt = match factory::open_configured(store.conn(), deps.runner, deps.cwd) {
        Ok(adapter) => adapter,
        Err(err @ FactoryOpenError::NotImplemented) => return Err(CommandError::failure(err)),
        Err(FactoryOpenError::Storage(err)) => return Err(resolver::storage_error(&err)),
    };
    let Some(mut adapter) = adapter_opt else {
        return Err(no_remote());
    };

    // Canonicalize the single issue through its Backend Adapter. Adopt does not
    // trust raw input for idempotence because Backends may accept aliases.
    // A non-existent issue or a PR (tk-34's guard) surfaces verbatim.
    let adopted = adapter
        .adopt_ticket(&args.key)
        .map_err(adapter_read_error)?;
    let capabilities = match store_sync::readopt_requirements(store.conn(), expected, &adopted)
        .map_err(adopt_store_error)?
    {
        Some(requirements) => adapter
            .resolve_promotion_capabilities(requirements)
            .map_err(adapter_read_error)?,
        None => crate::domain::promotion_capability::PromotionCapabilities::none(),
    };

    let outcome = store_sync::adopt_backend_ticket(
        store.conn_mut(),
        expected,
        &mut *deps.rng,
        &adopted,
        capabilities,
        &now,
    )
    .map_err(adopt_store_error)?;
    let stored = match outcome {
        AdoptOutcome::Inserted(row) => row,
        AdoptOutcome::AlreadyExists(row) => {
            let _ = writeln!(deps.stdout, "Already adopted: {}", row.display_id);
            return Ok(Exit::Ok);
        }
        AdoptOutcome::Readopted(report) => {
            render_readopt(deps.stdout, &report);
            return Ok(Exit::Ok);
        }
    };

    // The Status line carries the allow-closed signal: a closed issue is
    // adopted as a `done` Backend Ticket
    // (held out of `tk next`/`tk list` and never refreshed), so `Status: done`
    // is how Adopt avoids silently inserting an inert Ticket.
    let _ = writeln!(
        deps.stdout,
        "Adopted Ticket: {} - {}",
        stored.display_id, stored.title
    );
    if let Some(kind) = stored.ticket_kind {
        let _ = writeln!(deps.stdout, "Kind: {kind}");
    }
    if let Some(priority) = stored.priority {
        let _ = writeln!(deps.stdout, "Priority: {priority}");
    }
    let _ = writeln!(deps.stdout, "Status: {}", stored.status);
    Ok(Exit::Ok)
}

/// Report the restored identity mapping and the shared fields the Backend
/// snapshot replaced (ADR-0047).
///
/// The Display ID mapping comes first because Re-Adopt moves the Item's
/// user-facing identity, and the imported set is named in full: an Item that
/// stayed local for a while may have a title and body the Backend is about to
/// overwrite.
fn render_readopt(stdout: &mut dyn std::io::Write, report: &store_sync::ReadoptReport) {
    let _ = writeln!(
        stdout,
        "Re-adopted Ticket: {} - {}",
        report.backend_display_id, report.title
    );
    let _ = writeln!(stdout, "Backend object: {}", report.backend_key);
    let _ = writeln!(
        stdout,
        "Local Display ID kept as an Alias: {}",
        report.local_display_id
    );
    let _ = writeln!(
        stdout,
        "Imported title, body, Ticket Kind, and Lifecycle from the Backend"
    );
    let _ = writeln!(stdout, "Kind: {}", report.ticket_kind);
    let _ = writeln!(stdout, "Status: {}", report.status);
    for mutation in &report.queued_relationships {
        let _ = writeln!(
            stdout,
            "Queued {} for {} (Mutation {})",
            mutation.mutation_type, mutation.target_display_id, mutation.sequence
        );
    }
    if !report.queued_relationships.is_empty() {
        let _ = writeln!(stdout, "Run 'tk sync' to apply the queued relationships.");
    }
}

/// The no-Remote diagnostic returned when the Adapter factory finds none.
fn no_remote() -> CommandError {
    CommandError::failure("no Remote configured; run 'tk remote set <kind>' first")
}

/// Map an [`AdapterReadError`] to a seam-framed failure.
///
/// Both arms preserve the Adapter boundary's body: `Failed` carries the
/// Backend CLI stderr or Adapter validation diagnostic, while `Env` carries
/// the subprocess environment failure.
fn adapter_read_error(err: AdapterReadError) -> CommandError {
    match err {
        AdapterReadError::Failed(detail) => CommandError::failure(detail),
        AdapterReadError::Env(e) => CommandError::failure(e),
    }
}

/// Map an [`AdoptStoreError`] to a seam-framed failure.
fn adopt_store_error(err: AdoptStoreError) -> CommandError {
    match err {
        AdoptStoreError::DisplayIdCollision(id) => CommandError::failure(format!(
            "Display ID '{id}' already claimed by an existing Item"
        )),
        AdoptStoreError::BackendItemIsEpic(id) => {
            CommandError::failure(format!("{id} is a Backend Epic, not a Ticket"))
        }
        AdoptStoreError::RemoteChanged { .. } => CommandError::failure(
            "the configured Remote changed while contacting the Backend; retry 'tk adopt'",
        ),
        AdoptStoreError::Storage(e)
        | AdoptStoreError::BackendCohort(BackendCohortError::Storage(e))
        | AdoptStoreError::Append(crate::store::mutations::AppendError::Sqlite(e)) => {
            resolver::storage_error(&e)
        }
        AdoptStoreError::Sequence(e)
        | AdoptStoreError::Append(crate::store::mutations::AppendError::Sequence(e)) => {
            CommandError::failure(format!("Repository Store corruption: {e}"))
        }
        AdoptStoreError::BackendCohort(other) => {
            CommandError::failure(format!("Repository Store corruption: {other}"))
        }
        AdoptStoreError::FormerIdentityIsEpic(display_id) => CommandError::failure(format!(
            "{display_id} is a former Backend Epic; restoring one is not implemented in this build"
        )),
        AdoptStoreError::ReadoptBoundElsewhere {
            backend_display_id,
            bound_display_id,
        } => CommandError::failure(format!(
            "{backend_display_id} belongs to the Item now bound as {bound_display_id}; \
             run 'tk detach {bound_display_id}' before re-adopting {backend_display_id}"
        )),
        AdoptStoreError::ReadoptPendingPromotion {
            backend_display_id,
            local_display_id,
        } => CommandError::failure(format!(
            "{backend_display_id} belongs to {local_display_id}, which has a Pending Promotion; \
             run 'tk promote cancel {local_display_id}' before re-adopting {backend_display_id}"
        )),
        AdoptStoreError::ReadoptRelationships {
            item_id,
            backend_display_id,
            backend_kind,
            findings,
        } => readopt_refusal(&item_id, &backend_display_id, backend_kind, &findings),
        AdoptStoreError::BackendBinding(err) => resolver::backend_binding_error(&err),
        AdoptStoreError::ApplyingMutation(sequence) => CommandError::failure(format!(
            "Mutation {sequence} has an indeterminate Backend creation outcome; resolve it before adopting another Item\nUse 'tk promote reconcile <id> <backend-key>' if the Backend object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked."
        )),
    }
}

/// Render every ordered relationship finding from one Re-Adopt preflight.
fn readopt_refusal(
    item_id: &str,
    backend_display_id: &str,
    backend_kind: crate::domain::backend_kind::BackendKind,
    findings: &[RelationshipFinding],
) -> CommandError {
    let mut body = format!("cannot re-adopt {backend_display_id}:");
    for finding in findings {
        body.push_str("\n  ");
        match finding {
            RelationshipFinding::DependencyRejected {
                blocked,
                blocking,
                reason: DependencyRejection::BackendBlockedLocalBlocking,
            } => {
                let _ = write!(
                    body,
                    "{} would be blocked by Local {} '{}'; remove the Dependency first",
                    readopt_display_id(blocked, item_id, backend_display_id),
                    blocking.item_class.label(),
                    readopt_display_id(blocking, item_id, backend_display_id)
                );
            }
            RelationshipFinding::DependencyRejected {
                blocked,
                blocking,
                reason: DependencyRejection::BackendKindMismatch,
            } => {
                let _ = write!(
                    body,
                    "{} and {} would be backed by different Backends; remove the Dependency first",
                    readopt_display_id(blocked, item_id, backend_display_id),
                    readopt_display_id(blocking, item_id, backend_display_id)
                );
            }
            RelationshipFinding::DependencyNotRepresentable { blocked, blocking } => {
                let _ = write!(
                    body,
                    "{} depends on {}, and the {backend_kind} Backend cannot represent a Dependency",
                    readopt_display_id(blocked, item_id, backend_display_id),
                    readopt_display_id(blocking, item_id, backend_display_id)
                );
            }
            RelationshipFinding::EpicMembershipNotRepresentable { ticket, epic } => {
                let _ = write!(
                    body,
                    "{} belongs to Epic {}, and the {backend_kind} Backend cannot represent Epic membership",
                    readopt_display_id(ticket, item_id, backend_display_id),
                    readopt_display_id(epic, item_id, backend_display_id)
                );
            }
        }
    }
    CommandError::failure(body)
}

/// Show the restored Backend Display ID for the Item Re-Adopt binds.
fn readopt_display_id<'a>(
    item: &'a RelationshipItem,
    item_id: &str,
    backend_display_id: &'a str,
) -> &'a str {
    if item.id == item_id {
        backend_display_id
    } else {
        &item.display_id
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::commands::detach;
    use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::domain::item_class::ItemClass;
    use crate::domain::lifecycle::Lifecycle;
    use crate::domain::work_state::WorkState;
    use crate::proc::{ProcError, RunOutput};
    use crate::store::testing::{
        FixtureItem, FixtureMutation, FixtureRemote, TmpStore, apply_promotion_receipt,
        insert_dependency, insert_fixture_item, insert_fixture_mutation, insert_fixture_remote,
        item_axes, item_count, mutation_count,
    };
    use rusqlite::Connection;

    fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn fail(exit_code: i32, stderr: &str) -> RunOutput {
        RunOutput {
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A `gh issue view --json` object shaped like the adapter's `GhIssue`.
    fn issue_json(number: i64, state: &str, issue_type: &str, url: &str) -> String {
        let it = if issue_type == "null" {
            "null".to_string()
        } else {
            format!(r#"{{"name":"{issue_type}"}}"#)
        };
        format!(
            r#"{{"number":{number},"title":"Fix login","body":"B","state":"{state}","issueType":{it},"updatedAt":"2026-06-20T00:00:00Z","url":"{url}"}}"#
        )
    }

    /// Drive `run` and frame any error exactly as the dispatch seam does
    /// (ADR-0032: `tk adopt: <body>`), so a test asserts the framed bytes.
    fn run_rendered(h: &mut Harness<'_>, key: &str) -> Exit {
        let mut deps = h.deps();
        match run(&mut deps, Args { key: key.into() }) {
            Ok(exit) => exit,
            Err(err) => {
                let exit = err.exit();
                err.render(deps.stderr, "adopt");
                exit
            }
        }
    }

    #[test]
    fn adopts_an_open_issue_and_renders_the_created_block() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "https://github.com/o/r/issues/42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "https://github.com/o/r/issues/42");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(
            stdout.contains("Adopted Ticket: gh-42 - Fix login"),
            "{stdout}"
        );
        assert!(stdout.contains("Kind: task"), "{stdout}");
        assert!(stdout.contains("Priority: P2"), "{stdout}");
        assert!(stdout.contains("Status: open"), "{stdout}");

        // The merged row is an accepted, backend-origin Ticket — and Adopt is a
        // current-state insert, so it leaves the Mutation Log empty.
        let mutations: i64 = conn
            .query_row("select count(*) from mutations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mutations, 0, "Adopt records no Mutation");
        let (origin, selection): (String, String) = conn
            .query_row(
                "select origin, selection_state from items \
                  where backend_key = 'https://github.com/o/r/issues/42'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, "backend");
        assert_eq!(selection, "accepted");
    }

    #[test]
    fn applying_creation_blocks_adopt_before_the_backend_read() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "t1",
                display: "tk-1",
                title: "Local work",
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 9,
                mutation_type: "promote_ticket",
                item_id: "t1",
                payload_json: r#"{"title":"Local work","body":"","backend_kind":"github"}"#,
                state: "applying",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stderr).unwrap(),
            "tk adopt: Mutation 9 has an indeterminate Backend creation outcome; resolve it before adopting another Item\n\
             Use 'tk promote reconcile <id> <backend-key>' if the Backend object exists, 'tk promote retry <id>' only when creating it again is safe, or 'tk promote cancel <id>' to withdraw the Promotion Operation, leaving any object it created untracked.\n"
        );
    }

    #[test]
    fn adopting_a_closed_issue_shows_status_done() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "7"],
            ok(&issue_json(
                7,
                "CLOSED",
                "Bug",
                "https://github.com/o/r/issues/7",
            )),
        );

        let code = run_rendered(&mut h, "7");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(
            stdout.contains("Adopted Ticket: gh-7 - Fix login"),
            "{stdout}"
        );
        assert!(stdout.contains("Kind: bug"), "{stdout}");
        // The allow-closed signal: a closed issue is adopted as `done`, not
        // silently inserted as inert work.
        assert!(stdout.contains("Status: done"), "{stdout}");
    }

    #[test]
    fn already_adopted_canonical_url_returns_without_a_backend_call() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "abc",
                display: "gh-42",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/42"),
                title: "Already here",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        let code = run_rendered(&mut h, "https://github.com/o/r/issues/42");
        let stdout = String::from_utf8(h.stdout).unwrap();
        assert_eq!(code, Exit::Ok, "stderr={:?}", String::from_utf8(h.stderr));
        assert!(stdout.contains("Already adopted: gh-42"), "{stdout}");
        assert!(!stdout.contains("Adopted Ticket:"), "{stdout}");
        let title: String = conn
            .query_row("select title from items where id = 'abc'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Already here");
        let item_count: i64 = conn
            .query_row("select count(*) from items", [], |row| row.get(0))
            .unwrap();
        let created_sequence: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((item_count, created_sequence), (1, 0));
    }

    #[test]
    fn adopting_the_issue_backing_an_epic_is_not_ticket_success() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "epic",
                display: "gh-5",
                item_class: "epic",
                ticket_kind: None,
                selection_state: None,
                priority: None,
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/5"),
                title: "Roadmap",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        h.runner.expect_exact(
            &[
                "gh",
                "issue",
                "view",
                "5",
                "--json",
                "number,title,body,state,issueType,labels,url",
            ],
            ok(&issue_json(
                5,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/5",
            )),
        );

        let code = run_rendered(&mut h, "5");

        assert_eq!(code, Exit::Failure);
        assert_eq!(
            String::from_utf8(h.stderr).unwrap(),
            "tk adopt: gh-5 is a Backend Epic, not a Ticket\n"
        );
    }

    #[test]
    fn no_remote_configured_is_a_failure_with_the_sync_guidance() {
        let store = TmpStore::new("repo");
        seed_store(&store);
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: no Remote configured; run 'tk remote set <kind>' first"),
            "{stderr}"
        );
    }

    #[test]
    fn jira_remote_is_not_implemented() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "jira",
                config_json: r#"{"site":"x","project":"P"}"#,
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(
                "tk adopt: the configured Remote's adapter is not implemented in this build"
            ),
            "{stderr}"
        );
    }

    #[test]
    fn a_pull_request_is_rejected_verbatim() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "99"],
            ok(&issue_json(
                99,
                "OPEN",
                "null",
                "https://github.com/o/r/pull/99",
            )),
        );

        let code = run_rendered(&mut h, "99");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: #99 is a pull request, not an issue"),
            "{stderr}"
        );
    }

    #[test]
    fn a_non_existent_issue_surfaces_the_backend_stderr() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        let stderr_line = "GraphQL: Could not resolve to an issue or pull request \
                           with the number of 5. (repository.issue)";
        expect_git(&h, &store);
        h.runner
            .expect(&["gh", "issue", "view", "5"], fail(1, stderr_line));

        let code = run_rendered(&mut h, "5");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains(&format!("tk adopt: {stderr_line}")),
            "{stderr}"
        );
    }

    #[test]
    fn display_id_collision_is_reported() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        // A local Item already owns the `gh-42` Display ID the adapter would
        // mint for issue 42. The transactional canonical-identity lookup finds
        // no Backend Item, so the `item_ids` insert is the collision backstop.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "local1",
                display: "gh-42",
                title: "Collides",
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        let mut h = Harness::with_seed(&cwd_path, 7);
        expect_git(&h, &store);
        h.runner.expect(
            &["gh", "issue", "view", "42"],
            ok(&issue_json(
                42,
                "OPEN",
                "null",
                "https://github.com/o/r/issues/42",
            )),
        );

        let code = run_rendered(&mut h, "42");
        assert_eq!(code, Exit::Failure);
        let stderr = String::from_utf8(h.stderr).unwrap();
        assert!(
            stderr.contains("tk adopt: Display ID 'gh-42' already claimed by an existing Item"),
            "{stderr}"
        );
    }

    #[test]
    fn adapter_read_error_maps_both_arms_to_their_bodies() {
        // Failed carries the adapter body verbatim; Env is the bare runner
        // failure — both framed `tk adopt:` by the seam.
        let failed = adapter_read_error(AdapterReadError::Failed("HTTP 502".into()));
        let mut out = Vec::new();
        failed.render(&mut out, "adopt");
        assert_eq!(String::from_utf8(out).unwrap(), "tk adopt: HTTP 502\n");

        let env = adapter_read_error(AdapterReadError::Env(ProcError::ExecutableNotFound));
        let mut out = Vec::new();
        env.render(&mut out, "adopt");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk adopt: executable not found on PATH\n"
        );
    }

    #[test]
    fn relationship_refusal_reports_every_finding() {
        let item = |id: &str, display_id: &str, item_class| RelationshipItem {
            id: id.into(),
            display_id: display_id.into(),
            item_class,
        };
        let error = adopt_store_error(AdoptStoreError::ReadoptRelationships {
            item_id: "stable".into(),
            backend_display_id: "gh-42".into(),
            backend_kind: crate::domain::backend_kind::BackendKind::Github,
            findings: vec![
                RelationshipFinding::DependencyNotRepresentable {
                    blocked: item("stable", "gh-42", ItemClass::Ticket),
                    blocking: item("blocker", "gh-8", ItemClass::Ticket),
                },
                RelationshipFinding::EpicMembershipNotRepresentable {
                    ticket: item("stable", "gh-42", ItemClass::Ticket),
                    epic: item("parent", "gh-9", ItemClass::Epic),
                },
            ],
        });
        let mut out = Vec::new();
        error.render(&mut out, "adopt");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "tk adopt: cannot re-adopt gh-42:\n  \
             gh-42 depends on gh-8, and the github Backend cannot represent a Dependency\n  \
             gh-42 belongs to Epic gh-9, and the github Backend cannot represent Epic membership\n"
        );
    }

    // ---- Re-Adopt (ADR-0047) --------------------------------------------

    /// A Backend Ticket fixture keeping `origin` and the identity columns in
    /// the agreement the schema requires. Its title, body, and Ticket Kind
    /// differ from what [`issue_json`] returns, so a Re-Adopt assertion tells
    /// an imported field from a preserved one.
    fn backend_ticket<'a>(display: &'a str, backend_key: &'a str) -> FixtureItem<'a> {
        FixtureItem {
            id: "stable",
            display,
            title: "Local title",
            body: "Local body",
            origin: "backend",
            backend_kind: Some("github"),
            backend_key: Some(backend_key),
            created_seq: 1,
            ..FixtureItem::default()
        }
    }

    /// Detach a seeded Backend Item through the real command, leaving the
    /// Local Item plus the canonical Former Backend Identity that Re-Adopt
    /// matches. Seeding that history by hand would let the fixture drift from
    /// what Detach actually writes.
    fn detach_item(store: &TmpStore, cwd_path: &Path, id: &str) {
        let mut h = Harness::new(cwd_path);
        expect_git(&h, store);
        let mut deps = h.deps();
        detach::run(&mut deps, detach::Args { id: id.to_owned() })
            .expect("detach the seeded Backend Item");
        h.runner.assert_all_consumed();
    }

    /// Queue the `gh issue view` call Adopt canonicalizes `<key>` through.
    fn expect_issue_view(h: &Harness<'_>, number: i64, state: &str, issue_type: &str) {
        h.runner.expect(
            &["gh", "issue", "view", &number.to_string()],
            ok(&issue_json(
                number,
                state,
                issue_type,
                &format!("https://github.com/o/r/issues/{number}"),
            )),
        );
    }

    /// The canonical identity the fixture Item is bound to, or `""` when it is
    /// Local.
    fn stored_backend_key(conn: &Connection) -> String {
        conn.query_row(
            "select coalesce(backend_key, '') from items where id = 'stable'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// The fixture Item's Origin.
    fn stored_origin(conn: &Connection) -> String {
        conn.query_row("select origin from items where id = 'stable'", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    /// Every `item_ids` row for one Item, as `(value, source)` in value order.
    fn resolver_rows(conn: &Connection, item_id: &str) -> Vec<(String, String)> {
        conn.prepare("select value, source from item_ids where item_id = ?1 order by value")
            .unwrap()
            .query_map([item_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn readopt_restores_the_item_and_keeps_mixed_origin_relationships_local() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "parent",
                display: "tk-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                title: "Parent Epic",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                priority: Some("P1"),
                selection_state: Some("parked"),
                container_id: Some("parent"),
                ..backend_ticket("gh-42", "https://github.com/o/r/issues/42")
            },
        )
        .unwrap();
        // A Local Blocked Item behind this one: the Dependency shape the
        // resulting graph keeps local whichever way the Binding moves
        // (ADR-0035).
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "waiter",
                display: "tk-8",
                title: "Dependent work",
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "stable", "waiter").unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "Bug");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        h.runner.assert_all_consumed();
        assert_eq!(
            h.out(),
            "Re-adopted Ticket: gh-42 - Fix login\n\
             Backend object: https://github.com/o/r/issues/42\n\
             Local Display ID kept as an Alias: tk-1\n\
             Imported title, body, Ticket Kind, and Lifecycle from the Backend\n\
             Kind: bug\n\
             Status: open\n"
        );
        assert!(!h.out().contains("Run 'tk sync'"), "{}", h.out());

        // One Backend object keeps one representation: the same stable Item is
        // rebound, and no fresh created_seq is spent on a duplicate.
        assert_eq!(item_count(&conn).unwrap(), 3);
        let created_seq: i64 = conn
            .query_row(
                "select value from sequences where name = 'item_created_seq'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(created_seq, 0);

        // The Backend snapshot replaces the shared fields; Item Class, Local
        // Fields, and relationships stay as the Item held them locally.
        let shared: (String, String, String, String, String, String) = conn
            .query_row(
                "select display_value, origin, backend_key, title, body, ticket_kind \
                   from items where id = 'stable'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            shared,
            (
                "gh-42".to_owned(),
                "backend".to_owned(),
                "https://github.com/o/r/issues/42".to_owned(),
                "Fix login".to_owned(),
                "B".to_owned(),
                "bug".to_owned(),
            )
        );
        let local: (String, Option<String>, String, Option<String>) = conn
            .query_row(
                "select item_class, priority, selection_state, container_id \
                   from items where id = 'stable'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            local,
            (
                "ticket".to_owned(),
                Some("P1".to_owned()),
                "parked".to_owned(),
                Some("parent".to_owned()),
            )
        );
        let blocked_id: String = conn
            .query_row(
                "select blocked_id from dependencies where blocking_id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(blocked_id, "waiter");

        // The Backend Display ID is current and the displaced local one is an
        // Alias whose provenance a later Detach reads back.
        assert_eq!(
            resolver_rows(&conn, "stable"),
            vec![
                ("gh-42".to_owned(), "display".to_owned()),
                ("tk-1".to_owned(), "alias".to_owned()),
            ]
        );
        let provenance: (String, Option<String>) = conn
            .query_row(
                "select binding_display_provenance, binding_local_display_value \
                   from items where id = 'stable'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(provenance, ("known".to_owned(), Some("tk-1".to_owned())));

        // Re-Adopt records no Backend intent of its own, and history keeps the
        // identity it restored.
        assert_eq!(mutation_count(&conn).unwrap(), 0);
        let history: i64 = conn
            .query_row(
                "select count(*) from former_backend_identities where item_id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(history, 1);
    }

    #[test]
    fn a_second_detach_reuses_the_identity_history_already_reserves() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");
        assert_eq!(run_rendered(&mut h, "42"), Exit::Ok, "stderr={}", h.err());

        detach_item(&store, &cwd_path, "gh-42");

        // The cycle mints no identity and no Display ID: one canonical Backend
        // object keeps one history row, and the Item returns to the exact
        // local Display ID its Binding displaced (ADR-0047).
        let history: Vec<(String, i64)> = conn
            .prepare("select backend_key, detached_seq from former_backend_identities")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            history,
            vec![("https://github.com/o/r/issues/42".to_owned(), 2)],
            "the second Detach records fresh ordering on the same identity"
        );
        let display_value: String = conn
            .query_row(
                "select display_value from items where id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(display_value, "tk-1");
        assert_eq!(
            resolver_rows(&conn, "stable"),
            vec![("tk-1".to_owned(), "display".to_owned())]
        );
    }

    #[test]
    fn readopt_leaves_mutations_detach_withdrew_terminal() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "gh-8",
                title: "Backend blocking work",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/8"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "stable").unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 3,
                mutation_type: "add_dependency",
                item_id: "stable",
                payload_json: r#"{"blocking_id":"blocker"}"#,
                state: "applied",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "add_dependency",
                item_id: "stable",
                payload_json: r#"{"blocking_id":"blocker"}"#,
                state: "pending",
                ..FixtureMutation::default()
            },
        )
        .unwrap();
        conn.execute(
            "update sequences set value = 4 where name = 'mutation_seq'",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        assert!(
            h.out()
                .contains("Queued add_dependency for gh-42 (Mutation 5)\n"),
            "{}",
            h.out()
        );
        // Detach made the old intent terminal. Re-Adopt records the current
        // relationship as a fresh later Mutation instead of reviving it.
        let mutations: Vec<(i64, String)> = conn
            .prepare("select sequence, state from mutations order by sequence")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            mutations,
            vec![
                (3, "applied".to_owned()),
                (4, "cancelled".to_owned()),
                (5, "pending".to_owned()),
            ]
        );
    }

    #[test]
    fn readopt_matches_the_legacy_numeric_key_history_holds() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        // Adopted before the Adapter canonicalized keys, so Detach copied a
        // bare issue number into history.
        insert_fixture_item(&conn, backend_ticket("gh-42", "42")).unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        assert!(h.out().contains("Backend object: 42\n"), "{}", h.out());
        // The restored Binding takes history's spelling, so one Backend object
        // never sits in the Store as both an active and a former identity.
        assert_eq!(item_count(&conn).unwrap(), 1);
        assert_eq!(stored_backend_key(&conn), "42");
    }

    #[test]
    fn readopt_reopens_a_legacy_keyed_done_item() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                status: "done",
                ..backend_ticket("gh-42", "42")
            },
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        // Where the two halves meet: the reopen is authorized by comparing the
        // restored key against history, so restoring the Adapter's
        // canonicalization instead of history's spelling would leave the
        // exception unmatched and abort a Re-Adopt of a legacy-keyed done Item.
        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        assert_eq!(
            item_axes(&conn, "stable").unwrap(),
            (Lifecycle::Open, WorkState::Idle)
        );
    }

    #[test]
    fn importing_a_done_lifecycle_clears_work_state() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                status: "active",
                ..backend_ticket("gh-42", "https://github.com/o/r/issues/42")
            },
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "CLOSED", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        assert!(h.out().contains("Status: done\n"), "{}", h.out());
        assert_eq!(
            item_axes(&conn, "stable").unwrap(),
            (Lifecycle::Done, WorkState::Idle)
        );
    }

    #[test]
    fn importing_an_open_lifecycle_preserves_work_state() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                status: "active",
                ..backend_ticket("gh-42", "https://github.com/o/r/issues/42")
            },
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        // Re-Adopt is not a workflow transition: work already under way
        // survives the shared Lifecycle it imports.
        assert!(h.out().contains("Status: active\n"), "{}", h.out());
        assert_eq!(
            item_axes(&conn, "stable").unwrap(),
            (Lifecycle::Open, WorkState::Active)
        );
    }

    #[test]
    fn importing_an_open_lifecycle_clears_an_incompatible_closing_reason() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                status: "done",
                ..backend_ticket("gh-42", "https://github.com/o/r/issues/42")
            },
        )
        .unwrap();
        conn.execute(
            "update items set closing_reason = 'Superseded' where id = 'stable'",
            [],
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        assert!(h.out().contains("Status: open\n"), "{}", h.out());
        // The Closing Reason CHECK confines it to `done`, so importing `open`
        // has to drop it (ADR-0006's Re-Adopt exception, ADR-0023).
        assert_eq!(
            item_axes(&conn, "stable").unwrap(),
            (Lifecycle::Open, WorkState::Idle)
        );
        let closing_reason: Option<String> = conn
            .query_row(
                "select closing_reason from items where id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(closing_reason, None);
    }

    #[test]
    fn readopt_refuses_a_dependency_the_backend_could_not_address() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        // Blocking the now-Local Item on another Local Ticket is allowed while
        // both are Local, and Re-Adopt is what would make the edge invalid.
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "tk-8",
                title: "Blocking work",
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "stable").unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk adopt: cannot re-adopt gh-42:\n  \
             gh-42 would be blocked by Local Ticket 'tk-8'; remove the Dependency first\n"
        );
        // The whole Re-Adopt refuses before any Store state changes, so the
        // Dependency is still local truth and the Item is still Local.
        assert_eq!(stored_origin(&conn), "local");
        assert_eq!(
            resolver_rows(&conn, "stable"),
            vec![("tk-1".to_owned(), "display".to_owned())]
        );
    }

    #[test]
    fn readopt_allows_a_dependency_the_backend_can_address() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "gh-8",
                title: "Backend blocking work",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/8"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "stable").unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        h.runner.assert_all_consumed();
        assert_eq!(
            h.out(),
            "Re-adopted Ticket: gh-42 - Fix login\n\
             Backend object: https://github.com/o/r/issues/42\n\
             Local Display ID kept as an Alias: tk-1\n\
             Imported title, body, Ticket Kind, and Lifecycle from the Backend\n\
             Kind: task\n\
             Status: open\n\
             Queued add_dependency for gh-42 (Mutation 1)\n\
             Run 'tk sync' to apply the queued relationships.\n"
        );
        assert_eq!(stored_origin(&conn), "backend");
        let mutation: (i64, String, String, String, String) = conn
            .query_row(
                "select sequence, mutation_type, item_id, payload_json, state \
                   from mutations",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            mutation,
            (
                1,
                "add_dependency".to_owned(),
                "stable".to_owned(),
                r#"{"blocking_id":"blocker"}"#.to_owned(),
                "pending".to_owned(),
            )
        );
    }

    #[test]
    fn readopt_reports_ordered_membership_and_dependency_mutations() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "parent",
                display: "gh-9",
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                selection_state: None,
                title: "Backend Epic",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/9"),
                created_seq: 1,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                container_id: Some("parent"),
                created_seq: 2,
                ..backend_ticket("gh-42", "https://github.com/o/r/issues/42")
            },
        )
        .unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocked",
                display: "gh-8",
                title: "Backend blocked work",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/8"),
                created_seq: 3,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        insert_dependency(&conn, "stable", "blocked").unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Ok, "stderr={}", h.err());
        h.runner.assert_all_consumed();
        assert_eq!(
            h.out(),
            "Re-adopted Ticket: gh-42 - Fix login\n\
             Backend object: https://github.com/o/r/issues/42\n\
             Local Display ID kept as an Alias: tk-1\n\
             Imported title, body, Ticket Kind, and Lifecycle from the Backend\n\
             Kind: task\n\
             Status: open\n\
             Queued add_ticket_to_epic for gh-42 (Mutation 1)\n\
             Queued add_dependency for gh-8 (Mutation 2)\n\
             Run 'tk sync' to apply the queued relationships.\n"
        );
        let mut stmt = conn
            .prepare(
                "select sequence, mutation_type, item_id, payload_json, state \
                   from mutations order by sequence",
            )
            .unwrap();
        let mutations = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            mutations,
            vec![
                (
                    1,
                    "add_ticket_to_epic".to_owned(),
                    "stable".to_owned(),
                    r#"{"epic_id":"parent"}"#.to_owned(),
                    "pending".to_owned(),
                ),
                (
                    2,
                    "add_dependency".to_owned(),
                    "blocked".to_owned(),
                    r#"{"blocking_id":"stable"}"#.to_owned(),
                    "pending".to_owned(),
                ),
            ]
        );
    }

    #[test]
    fn relationship_append_failure_rolls_back_the_whole_readopt() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        insert_fixture_item(
            &conn,
            FixtureItem {
                id: "blocker",
                display: "gh-8",
                title: "Backend blocking work",
                origin: "backend",
                backend_kind: Some("github"),
                backend_key: Some("https://github.com/o/r/issues/8"),
                created_seq: 2,
                ..FixtureItem::default()
            },
        )
        .unwrap();
        insert_dependency(&conn, "blocker", "stable").unwrap();
        conn.execute("delete from sequences where name = 'mutation_seq'", [])
            .unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "Bug");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk adopt: Repository Store corruption: sequence counter \
             `mutation_seq` is missing from the store\n"
        );
        assert_eq!(stored_origin(&conn), "local");
        assert_eq!(stored_backend_key(&conn), "");
        assert_eq!(
            resolver_rows(&conn, "stable"),
            vec![("tk-1".to_owned(), "display".to_owned())]
        );
        let fields: (String, String, String) = conn
            .query_row(
                "select title, body, ticket_kind from items where id = 'stable'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            fields,
            (
                "Local title".to_owned(),
                "Local body".to_owned(),
                "task".to_owned(),
            )
        );
        assert_eq!(mutation_count(&conn).unwrap(), 0);
    }

    #[test]
    fn readopt_refuses_while_the_item_is_bound_to_another_backend_object() {
        let store = TmpStore::new("repo");
        let mut conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        apply_promotion_receipt(
            &mut conn,
            "stable",
            "github",
            &BackendItemIdentity {
                backend_key: "https://github.com/o/r/issues/99".into(),
                display_id: "gh-99".into(),
            },
            "2026-05-10T00:00:00.000Z",
        )
        .unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk adopt: gh-42 belongs to the Item now bound as gh-99; \
             run 'tk detach gh-99' before re-adopting gh-42\n"
        );
        // A refusal must neither rebind the Item nor create a second one.
        assert_eq!(item_count(&conn).unwrap(), 1);
        assert_eq!(
            stored_backend_key(&conn),
            "https://github.com/o/r/issues/99"
        );
    }

    #[test]
    fn readopt_refuses_while_the_item_has_a_pending_promotion() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");
        insert_fixture_mutation(
            &conn,
            FixtureMutation {
                sequence: 4,
                mutation_type: "promote_ticket",
                item_id: "stable",
                payload_json: r#"{"title":"Local title","body":"Local body","backend_kind":"github"}"#,
                state: "pending",
                promotion_operation_id: Some("op-1"),
                ..FixtureMutation::default()
            },
        )
        .unwrap();

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 42, "OPEN", "null");

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Failure);
        // The Promotion is owed a Backend identity of its own; rebinding now
        // would leave its receipt no Local Item to bind (ADR-0036).
        assert_eq!(
            h.err(),
            "tk adopt: gh-42 belongs to tk-1, which has a Pending Promotion; \
             run 'tk promote cancel tk-1' before re-adopting gh-42\n"
        );
        assert_eq!(stored_origin(&conn), "local");
    }

    #[test]
    fn readopt_of_a_former_backend_epic_is_refused_not_adopted_as_a_ticket() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_remote(&conn, FixtureRemote::default()).unwrap();
        insert_fixture_item(
            &conn,
            FixtureItem {
                item_class: "epic",
                ticket_kind: None,
                priority: None,
                selection_state: None,
                title: "Roadmap",
                ..backend_ticket("gh-5", "https://github.com/o/r/issues/5")
            },
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-5");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);
        expect_issue_view(&h, 5, "OPEN", "null");

        let exit = run_rendered(&mut h, "5");

        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            h.err(),
            "tk adopt: gh-5 is a former Backend Epic; \
             restoring one is not implemented in this build\n"
        );
        // Refusing beats falling through: ordinary intake would have inserted
        // a second Item for a Backend object the Epic still owns.
        assert_eq!(item_count(&conn).unwrap(), 1);
        assert_eq!(stored_origin(&conn), "local");
    }

    #[test]
    fn readopt_still_needs_a_configured_remote() {
        let store = TmpStore::new("repo");
        let conn = seed_store(&store);
        insert_fixture_item(
            &conn,
            backend_ticket("gh-42", "https://github.com/o/r/issues/42"),
        )
        .unwrap();
        let cwd_path = cwd();
        detach_item(&store, &cwd_path, "gh-42");

        let mut h = Harness::new(&cwd_path);
        expect_git(&h, &store);

        let exit = run_rendered(&mut h, "42");

        assert_eq!(exit, Exit::Failure);
        // Former Backend Identity is history, not a dormant Binding: it does
        // not stand in for the Remote Re-Adopt reads the snapshot through.
        h.runner.assert_all_consumed();
        assert_eq!(
            h.err(),
            "tk adopt: no Remote configured; run 'tk remote set <kind>' first\n"
        );
    }
}
