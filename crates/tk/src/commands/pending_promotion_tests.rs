//! Pending Promotion visibility through the command interface (ADR-0041).

use crate::cli::{self, Exit};
use crate::commands::testing::{Harness, cwd, expect_git, seed_store};
use crate::domain::item_class::ItemClass;
use crate::domain::mutation_type::MutationType;
use crate::store::testing::{
    FixtureItem, FixtureMutation, TmpStore, insert_fixture_item, insert_fixture_mutation,
};

#[test]
fn show_identifies_pending_promotion_in_header() {
    let store = TmpStore::new("repo");
    let conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    promotion(&conn, "work", ItemClass::Ticket, 1, "pending");
    drop(conn);
    let output = run(&store, &["show", "tk-1"]);
    assert!(
        output.contains("  Binding: pending promotion\n"),
        "{output}"
    );
    assert!(output.contains("1 pending promote_ticket"), "{output}");
}

#[test]
fn grep_identifies_pending_promotion_without_changing_content_matches() {
    let store = TmpStore::new("repo");
    let conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            body: "needle",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    promotion(&conn, "work", ItemClass::Ticket, 1, "applying");
    drop(conn);
    let output = run(&store, &["grep", "needle"]);
    assert!(
        output.contains("  Binding: pending promotion\n"),
        "{output}"
    );
    assert!(output.contains("needle"), "{output}");
    assert!(run(&store, &["grep", "--quiet", "needle"]).is_empty());
}

#[test]
fn next_identifies_pending_promotion_but_quiet_keeps_the_bare_id() {
    let store = TmpStore::new("repo");
    let conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    promotion(&conn, "work", ItemClass::Ticket, 1, "failed");
    drop(conn);
    assert_eq!(run(&store, &["next"]), "tk-1: [pending promotion] Work\n");
    assert_eq!(run(&store, &["next", "--quiet"]), "tk-1\n");
}

#[test]
fn plan_identifies_pending_promotion_even_when_the_ticket_is_done() {
    let store = TmpStore::new("repo");
    let conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            status: "done",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    promotion(&conn, "work", ItemClass::Ticket, 1, "pending");
    drop(conn);
    run(&store, &["plan", "add", "tk-1"]);
    let output = run(&store, &["plan"]);
    assert!(
        output.contains("Done\n  ✓ tk-1 ● P2 [pending promotion] Work\n"),
        "{output}"
    );
}

#[test]
fn show_labels_each_related_items_own_pending_promotion() {
    let store = TmpStore::new("repo");
    let conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "epic",
            display: "tk-1",
            title: "Epic",
            item_class: "epic",
            ticket_kind: None,
            priority: None,
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    for (id, display, title, sequence, parent) in [
        ("work", "tk-2", "Work", 2, Some("epic")),
        ("before", "tk-3", "Before", 3, None),
        ("after", "tk-4", "After", 4, None),
        ("local", "tk-5", "Local", 5, Some("epic")),
    ] {
        insert_fixture_item(
            &conn,
            FixtureItem {
                id,
                display,
                title,
                created_seq: sequence,
                container_id: parent,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }
    promotion(&conn, "epic", ItemClass::Epic, 1, "pending");
    promotion(&conn, "work", ItemClass::Ticket, 2, "pending");
    promotion(&conn, "before", ItemClass::Ticket, 3, "failed");
    promotion(&conn, "after", ItemClass::Ticket, 4, "applying");
    crate::store::testing::insert_dependency(&conn, "before", "work").unwrap();
    crate::store::testing::insert_dependency(&conn, "work", "after").unwrap();
    drop(conn);

    let output = run(&store, &["show", "tk-2"]);
    for expected in [
        "tk-1: (Epic) [pending promotion] Epic",
        "tk-3: [pending promotion] Before",
        "tk-4: [pending promotion] After",
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in {output}"
        );
    }
    let output = run(&store, &["show", "tk-1"]);
    assert!(
        output.contains("tk-2: [pending promotion] Work"),
        "{output}"
    );
    assert!(output.contains("tk-5: Local"), "{output}");
}

#[test]
fn withdrawal_removes_binding_labels_and_keeps_mutation_history() {
    let store = TmpStore::new("repo");
    let mut conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    crate::store::testing::commit_promotion(&mut conn, "work");
    drop(conn);
    assert!(run(&store, &["list", "--local"]).contains("[pending promotion]"));
    assert!(!run(&store, &["list", "--remote"]).contains("tk-1"));
    run(&store, &["promote", "cancel", "tk-1"]);
    assert!(!run(&store, &["list"]).contains("[pending promotion]"));
    let output = run(&store, &["show", "tk-1"]);
    assert!(!output.contains("Binding:"), "{output}");
    assert!(output.contains("cancelled promote_ticket"), "{output}");
}

#[test]
fn recorded_identity_removes_binding_label_while_queued_edits_stay_visible() {
    use crate::domain::backend_operation::BackendItemIdentity;
    use crate::domain::mutation_state::MutationState;

    let store = TmpStore::new("repo");
    let mut conn = seed_store(&store);
    insert_fixture_item(
        &conn,
        FixtureItem {
            id: "work",
            display: "tk-1",
            title: "Work",
            created_seq: 1,
            ..FixtureItem::default()
        },
    )
    .unwrap();
    promotion(&conn, "work", ItemClass::Ticket, 1, "applying");
    insert_fixture_mutation(
        &conn,
        FixtureMutation {
            sequence: 2,
            item_id: "work",
            ..FixtureMutation::of(MutationType::UpdateTicket)
        },
    )
    .unwrap();
    assert!(run(&store, &["search", "Work"]).contains("~ [pending promotion] Work"));
    let tx = conn.transaction().unwrap();
    let now = "2026-05-09T00:00:00.000Z";
    crate::store::promotion::apply_receipt(
        &tx,
        "work",
        "github",
        &BackendItemIdentity {
            backend_key: "https://github.com/test/repo/issues/1".into(),
            display_id: "gh-1".into(),
        },
        now,
    )
    .unwrap();
    crate::store::mutations::mark_applied(&tx, 1, MutationState::Applying, now).unwrap();
    tx.commit().unwrap();
    drop(conn);
    let output = run(&store, &["search", "Work"]);
    assert!(output.contains("gh-1 ● P2 ~ Work"), "{output}");
    assert!(!output.contains("[pending promotion]"), "{output}");
    let output = run(&store, &["show", "tk-1"]);
    assert!(output.starts_with("○ gh-1 · Work\n"), "{output}");
    assert!(!output.contains("Binding:"), "{output}");
    assert!(output.contains("2 pending update_ticket"), "{output}");
}

fn run(store: &TmpStore, args: &[&str]) -> String {
    let cwd = cwd();
    let mut harness = Harness::new(&cwd);
    expect_git(&harness, store);
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let exit = cli::run_argv(harness.deps(), &args).unwrap();
    assert_eq!(exit, Exit::Ok, "{}", harness.err());
    harness.out()
}

fn promotion(
    conn: &rusqlite::Connection,
    item_id: &str,
    item_class: ItemClass,
    sequence: i64,
    state: &str,
) {
    insert_fixture_mutation(
        conn,
        FixtureMutation {
            sequence,
            item_id,
            item_class,
            state,
            payload_json: r#"{"backend_kind":"github","title":"Work","body":""}"#,
            failure_json: (state == "failed").then_some(r#"{"detail":"rejected"}"#),
            ..FixtureMutation::of(match item_class {
                ItemClass::Ticket => MutationType::PromoteTicket,
                ItemClass::Epic => MutationType::PromoteEpic,
            })
        },
    )
    .unwrap();
}
