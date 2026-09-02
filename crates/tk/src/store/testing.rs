//! Test-only fixture helpers for the Repository Store.
//!
//! These bypass the production write API surface so unit tests for
//! read-side queries (list / next / show / resolve) can seed Tickets,
//! Epics, Aliases, Dependencies, External Blockers, Mutations, and the
//! singleton Remote without going through commands that don't exist yet
//! or that would themselves write Mutations.
//!
//! Available to crate tests only — `mod testing` is gated on `#[cfg(test)]`
//! in `store/mod.rs`. Individual helpers may be unused while only a subset
//! of the repository surface has landed; the umbrella `#[allow(dead_code)]`
//! is removed once the surface is complete.

#![allow(dead_code)]

use std::path::PathBuf;

use rand::SeedableRng;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use crate::domain::backend_operation::BackendItemIdentity;
use crate::domain::lifecycle::Lifecycle;
use crate::domain::work_state::WorkState;

/// Apply a Promotion receipt inside the write transaction its deferred Display
/// ID foreign key requires.
pub fn apply_promotion_receipt(
    conn: &mut Connection,
    item_id: &str,
    backend_kind: &str,
    receipt: &BackendItemIdentity,
    now: &str,
) -> Result<(), crate::store::promotion::ApplyReceiptError> {
    let tx = crate::store::write_transaction(conn)?;
    crate::store::promotion::apply_receipt(&tx, item_id, backend_kind, receipt, now)?;
    Ok(tx.commit()?)
}

/// On-disk scaffolding for a fake Git repository plus its `git rev-parse`
/// stdout payload. The `tk init` discovery layer expects two newline-
/// separated absolute paths (git-common-dir, top-level); planting the same
/// shape via the fake subprocess runner lets `Store::open_existing` exercise
/// the production discovery flow.
pub struct TmpStore {
    _tmp: TempDir,
    pub common_dir: PathBuf,
    pub toplevel: PathBuf,
}

impl TmpStore {
    /// Create a temporary `<basename>/.git` skeleton under a fresh tempdir.
    ///
    /// `basename` chooses the toplevel directory name — the seed prefix the
    /// store derives from it pins downstream Display IDs (e.g. picking
    /// `"my-test-repo"` makes the first item resolve as `my-test-repo-1`).
    pub fn new(basename: &str) -> Self {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let toplevel = tmp.path().join(basename);
        let common_dir = toplevel.join(".git");
        std::fs::create_dir_all(&common_dir).expect("create .git skeleton");
        Self {
            _tmp: tmp,
            common_dir,
            toplevel,
        }
    }

    /// Concrete path the store opens for this fake repository.
    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.common_dir.join("tk").join("tk.db")
    }

    /// Path to the `tk/` directory the store would create on `tk init`.
    #[must_use]
    pub fn tk_dir(&self) -> PathBuf {
        self.common_dir.join("tk")
    }

    /// Build the `git rev-parse --git-common-dir --show-toplevel` stdout
    /// payload this repo would produce. Feed it to the fake subprocess
    /// runner via `FakeRunner::expect`.
    #[must_use]
    pub fn git_rev_parse_stdout(&self) -> Vec<u8> {
        format!(
            "{}\n{}\n",
            self.common_dir.display(),
            self.toplevel.display()
        )
        .into_bytes()
    }
}

/// Raw Repository Store item fixture used by read-side tests.
///
/// Deliberately bypasses production write APIs so tests can seed Epics,
/// backend-origin items, Dependencies, and External Blockers before the
/// matching write commands exist.
#[derive(Debug, Clone, Copy)]
pub struct FixtureItem<'a> {
    pub id: &'a str,
    pub display: &'a str,
    pub item_class: &'a str,
    pub ticket_kind: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    /// Item Status the fixture seeds. Split across the two stored axes on the
    /// way in, mirroring production, so `"active"` keeps meaning what it did
    /// before ADR-0043 split the column.
    pub status: &'a str,
    /// Work State override. `None` derives it from `status`, which is what
    /// every ordinary fixture wants; set it to seed the `(done, active)` row
    /// no writer produces. Only valid on an `accepted` Ticket or an Epic —
    /// the relocated ADR-0029 conjunct rejects `active` on triage or parked —
    /// and only against a store past the split, since a pre-split `items` has
    /// no column to put it in. Setting it on an older store panics.
    pub work_state: Option<&'a str>,
    pub origin: &'a str,
    pub backend_kind: Option<&'a str>,
    pub backend_key: Option<&'a str>,
    pub container_id: Option<&'a str>,
    /// Selection State override for Ticket fixtures (ADR-0027). `None` derives
    /// the default `accepted`; set `Some("triage")` (with `priority: None`) or
    /// `Some("parked")` to fixture those states. Ignored for Epics, which
    /// always store `NULL`.
    pub selection_state: Option<&'a str>,
    pub created_seq: i64,
    pub created_at: &'a str,
    pub updated_at: &'a str,
}

impl Default for FixtureItem<'_> {
    /// Defaults shaped like the most common live row: a P2 task ticket,
    /// open, local-origin, no parent. Tests override only the fields they
    /// care about.
    fn default() -> Self {
        Self {
            id: "",
            display: "",
            item_class: "ticket",
            ticket_kind: Some("task"),
            priority: Some("P2"),
            title: "",
            body: "",
            status: "open",
            work_state: None,
            origin: "local",
            backend_kind: None,
            backend_key: None,
            container_id: None,
            selection_state: None,
            created_seq: 0,
            created_at: "2026-05-09T00:00:00.000Z",
            updated_at: "2026-05-09T00:00:00.000Z",
        }
    }
}

/// Insert one current-state item plus its `display`-source resolver row.
// A by-value `FixtureItem` is the ergonomic call shape — every caller passes a
// fresh inline literal with `..FixtureItem::default()`. The struct crossed the
// pedantic 256-byte threshold when `selection_state` was added (tk-74); a
// by-reference signature would add `&` to ~30 fixture call sites for no real
// gain in a test-only builder.
#[allow(clippy::large_types_passed_by_value)]
pub fn insert_fixture_item(conn: &Connection, item: FixtureItem<'_>) -> rusqlite::Result<()> {
    // Selection State (ADR-0027) is Ticket-only: a Ticket takes the explicit
    // `selection_state` override or defaults to `accepted`; an Epic always
    // stores NULL. Deriving from item_class keeps every Epic call site correct
    // without an override, while triage/parked Ticket fixtures opt in via the
    // field (paired with `priority: None` for triage, per the combined CHECK).
    let selection_state =
        (item.item_class == "ticket").then(|| item.selection_state.unwrap_or("accepted"));
    // Item Status is derived, not stored (ADR-0043): split the three-valued
    // fixture spelling across the two columns the way production writes them,
    // so callers seeding `"active"` keep working untouched. An explicit
    // `work_state` overrides the derivation — the only way to reach a
    // `(done, active)` row, which no writer produces.
    let (status, work_state) = match item.status {
        "active" => ("open", item.work_state.unwrap_or("active")),
        lifecycle => (lifecycle, item.work_state.unwrap_or("idle")),
    };
    insert_item_row(conn, &item, status, Some(work_state), selection_state)
}

/// Seed one item into a store frozen BELOW the schema version that split Work
/// State out of Item Status.
///
/// Migration tests reach back past the split with `apply_through`, where
/// `items.status` still carries the fused three-valued spelling and
/// `work_state` does not exist. They call this rather than
/// [`insert_fixture_item`] so the schema version stays where it is statically
/// known — at the call site that pinned it — instead of being re-discovered
/// per insert. Both share the `items` + `item_ids` write below, which is the
/// only part that was ever common.
// By value for the same reason as its sibling above: callers pass a fresh
// inline literal, and the struct sits just over clippy's pedantic threshold.
#[allow(clippy::large_types_passed_by_value)]
pub fn insert_pre_split_fixture_item(
    conn: &Connection,
    item: FixtureItem<'_>,
) -> rusqlite::Result<()> {
    assert!(
        item.work_state.is_none(),
        "a store below the split has no `work_state` column to seed"
    );
    let selection_state =
        (item.item_class == "ticket").then(|| item.selection_state.unwrap_or("accepted"));
    insert_item_row(conn, &item, item.status, None, selection_state)
}

/// Write the `items` row and its `display`-source resolver row together.
///
/// The two are a pair: `items` carries a deferred composite foreign key onto
/// `item_ids`, so a fixture that writes one without the other leaves the
/// transaction unable to commit. `work_state` is `None` only for a store below
/// the split, where the column does not exist.
fn insert_item_row(
    conn: &Connection,
    item: &FixtureItem<'_>,
    status: &str,
    work_state: Option<&str>,
    selection_state: Option<&str>,
) -> rusqlite::Result<()> {
    let container_class = item.container_id.map(|_| "epic");
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "insert into items(\
            id, display_value, item_class, ticket_kind, priority, title, body, \
            container_id, container_class, origin, backend_kind, backend_key, \
            status, selection_state, created_seq, created_at, updated_at\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            item.id,
            item.display,
            item.item_class,
            item.ticket_kind,
            item.priority,
            item.title,
            item.body,
            item.container_id,
            container_class,
            item.origin,
            item.backend_kind,
            item.backend_key,
            status,
            selection_state,
            item.created_seq,
            item.created_at,
            item.updated_at,
        ],
    )?;
    // `idle` is what migration 011 declares as the column default, so writing
    // it back would be a no-op on every ordinary fixture. Only a deliberate
    // `active` — or the `(done, active)` pair no writer produces — needs the
    // second statement.
    if let Some(work_state) = work_state.filter(|w| *w != "idle") {
        tx.execute(
            "update items set work_state = ?2 where id = ?1",
            params![item.id, work_state],
        )?;
    }
    tx.execute(
        "insert into item_ids(value, source, item_id, created_at) values (?1, 'display', ?2, ?3)",
        params![item.display, item.id, item.created_at],
    )?;
    tx.commit()
}

/// Insert an Alias resolver row for an existing Ticket or Epic fixture.
pub fn insert_alias(conn: &Connection, value: &str, item_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "insert into item_ids(value, source, item_id, created_at) \
         values (?1, 'alias', ?2, '2026-05-09T00:00:00.000Z')",
        params![value, item_id],
    )?;
    Ok(())
}

/// Insert a Dependency edge from a Blocking Item to a Blocked Item.
pub fn insert_dependency(
    conn: &Connection,
    blocking_id: &str,
    blocked_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "insert into dependencies(blocking_id, blocked_id, created_at) \
         values (?1, ?2, '2026-05-09T00:00:00.000Z')",
        params![blocking_id, blocked_id],
    )?;
    Ok(())
}

/// Insert an External Blocker fixture; `resolved_at = None` means unresolved.
pub fn insert_external_blocker(
    conn: &Connection,
    id: &str,
    item_id: &str,
    resolved_at: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "insert into external_blockers(id, item_id, reason, created_at, resolved_at) \
         values (?1, ?2, 'fixture blocker', '2026-05-09T00:00:00.000Z', ?3)",
        params![id, item_id, resolved_at],
    )?;
    Ok(())
}

/// Return the current count of rows in `items`, for tests asserting that a
/// path rebound an existing Item rather than inserting another one.
pub fn item_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("select count(*) from items", [], |row| row.get(0))
}

/// Return the current count of rows in the `mutations` outbox.
pub fn mutation_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("select count(*) from mutations", [], |r| r.get(0))
}

/// The two stored Item Status axes for one Item, as the read-back every
/// transition test needs: a writer that lands the right Lifecycle while
/// forgetting its Work State clear leaves a row that renders correctly
/// everywhere, so only reading both columns catches it (ADR-0043).
pub fn item_axes(conn: &Connection, id: &str) -> rusqlite::Result<(Lifecycle, WorkState)> {
    conn.query_row(
        "select status, work_state from items where id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Every Mutation Type in the outbox in Mutation Sequence order — the shape
/// that shows both which intent a write path appended and that it landed
/// behind the Mutations already in the log.
pub fn mutation_types(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("select mutation_type from mutations order by sequence")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// Give the Item at internal `items.id` `id` a Pending Promotion the way
/// `tk promote` does: preflight the real Promotion graph and commit the
/// planner's outbox.
///
/// The one helper here that drives production code rather than bypassing it.
/// The Backend Binding gates (ADR-0036) are validated against Mutation Log rows
/// `commit_plan` actually wrote, so a change to the Promotion payload
/// or to what counts as pending breaks the gate tests too.
pub fn commit_promotion(conn: &mut Connection, id: &str) {
    use crate::domain::backend_kind::BackendKind;
    use crate::domain::promotion_capability::PromotionCapabilities;

    let graph =
        crate::store::promotion::read_graph(conn, id, false).expect("read the Promotion graph");
    let plan = crate::promotion::plan::plan_promotion(
        &graph,
        PromotionCapabilities::all(),
        BackendKind::Github,
    )
    .expect("a promotable fixture");
    if crate::store::sync::configured_remote_kind(conn)
        .expect("read fixture Remote")
        .is_none()
    {
        crate::store::sync::set_remote(conn, BackendKind::Github, "{}", "2026-05-09T00:00:00.000Z")
            .expect("configure fixture Remote");
    }
    crate::store::promotion::commit_plan(
        conn,
        &crate::store::repository::RemoteWorkflowGuard::for_test(),
        &plan,
        BackendKind::Github,
        &mut rand::rngs::StdRng::seed_from_u64(7),
        "2026-05-09T00:00:00.000Z",
    )
    .expect("commit the Promotion outbox");
}

/// Raw Mutation Log fixture for sync engine and read-side outbox tests.
///
/// Bypasses production `mutations::append` so tests can seed `failed`,
/// `skipped`, and `applied` Mutations before the sync command surface
/// exists. The caller picks `sequence` directly; this helper does NOT
/// touch the `mutation_seq` counter, so tests that mix fixture inserts
/// with live appends must advance the counter themselves.
#[derive(Debug, Clone, Copy)]
pub struct FixtureMutation<'a> {
    pub sequence: i64,
    pub mutation_type: &'a str,
    pub item_id: &'a str,
    pub item_class: &'a str,
    pub payload_json: &'a str,
    pub state: &'a str,
    pub failure_json: Option<&'a str>,
    pub created_at: &'a str,
    pub state_changed_at: &'a str,
    /// Promotion Operation grouping (ADR-0036). `None` seeds a pre-Promotion
    /// Mutation; set it to fixture a row belonging to one `tk promote`
    /// invocation's outbox writes.
    pub promotion_operation_id: Option<&'a str>,
}

impl Default for FixtureMutation<'_> {
    fn default() -> Self {
        Self {
            sequence: 1,
            mutation_type: "",
            item_id: "",
            item_class: "ticket",
            payload_json: "{}",
            state: "pending",
            failure_json: None,
            created_at: "2026-05-09T00:00:00.000Z",
            state_changed_at: "2026-05-09T00:00:00.000Z",
            promotion_operation_id: None,
        }
    }
}

pub fn insert_fixture_mutation(
    conn: &Connection,
    mutation: FixtureMutation<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        "insert into mutations(\
            sequence, mutation_type, item_id, item_class, payload_json, \
            state, failure_json, created_at, state_changed_at, promotion_operation_id\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            mutation.sequence,
            mutation.mutation_type,
            mutation.item_id,
            mutation.item_class,
            mutation.payload_json,
            mutation.state,
            mutation.failure_json,
            mutation.created_at,
            mutation.state_changed_at,
            mutation.promotion_operation_id,
        ],
    )?;
    Ok(())
}

/// Raw Remote configuration fixture used by `tk remote` and sync tests.
#[derive(Debug, Clone, Copy)]
pub struct FixtureRemote<'a> {
    pub backend_kind: &'a str,
    pub config_json: &'a str,
    pub last_applied_sequence: i64,
    pub created_at: &'a str,
    pub updated_at: &'a str,
}

impl Default for FixtureRemote<'_> {
    fn default() -> Self {
        Self {
            backend_kind: "github",
            config_json: "{}",
            last_applied_sequence: 0,
            created_at: "2026-05-09T00:00:00.000Z",
            updated_at: "2026-05-09T00:00:00.000Z",
        }
    }
}

/// Raw Former Backend Identity fixture: one canonical Backend identity an Item
/// detached from (ADR-0047).
///
/// Seeds history directly for tests that need an Item to arrive already
/// detached. A test that also cares what Detach *writes* should run Detach
/// instead.
#[derive(Debug, Clone, Copy)]
pub struct FixtureFormerIdentity<'a> {
    pub backend_kind: &'a str,
    /// Canonical key exactly as history stores it — a bare issue number for an
    /// Item adopted before the Adapter canonicalized keys.
    pub backend_key: &'a str,
    pub item_id: &'a str,
    pub backend_display_value: &'a str,
    /// Detach ordering, which `tk show` lists by, most recent first.
    pub detached_seq: i64,
    pub detached_at: &'a str,
}

impl Default for FixtureFormerIdentity<'_> {
    fn default() -> Self {
        Self {
            backend_kind: "github",
            backend_key: "",
            item_id: "",
            backend_display_value: "",
            detached_seq: 1,
            detached_at: "2026-05-09T00:00:00.000Z",
        }
    }
}

/// Insert one Former Backend Identity history row.
pub fn insert_fixture_former_identity(
    conn: &Connection,
    identity: FixtureFormerIdentity<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        "insert into former_backend_identities(backend_kind, backend_key, item_id, \
                                                backend_display_value, detached_seq, detached_at) \
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            identity.backend_kind,
            identity.backend_key,
            identity.item_id,
            identity.backend_display_value,
            identity.detached_seq,
            identity.detached_at,
        ],
    )?;
    Ok(())
}

/// Insert the v1 single-Remote configuration plus its Sync Cursor.
pub fn insert_fixture_remote(conn: &Connection, remote: FixtureRemote<'_>) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "insert into remotes(name, backend_kind, config_json, created_at, updated_at) \
         values ('primary', ?1, ?2, ?3, ?4)",
        params![
            remote.backend_kind,
            remote.config_json,
            remote.created_at,
            remote.updated_at,
        ],
    )?;
    tx.execute(
        "insert into sync_cursors(remote_name, backend_kind, last_applied_sequence, updated_at) \
         values ('primary', ?1, ?2, ?3)",
        params![
            remote.backend_kind,
            remote.last_applied_sequence,
            remote.updated_at,
        ],
    )?;
    tx.commit()
}
