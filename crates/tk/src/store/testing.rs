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
//! is removed in the slice that completes the surface.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use tempfile::TempDir;

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

    #[must_use]
    pub fn toplevel(&self) -> &Path {
        &self.toplevel
    }
}

/// Raw Repository Store item fixture used by read-side tests.
///
/// Deliberately bypasses production write APIs so slices can seed Epics,
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
    pub status: &'a str,
    pub origin: &'a str,
    pub backend_kind: Option<&'a str>,
    pub backend_key: Option<&'a str>,
    pub container_id: Option<&'a str>,
    pub container_class: Option<&'a str>,
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
            origin: "local",
            backend_kind: None,
            backend_key: None,
            container_id: None,
            container_class: None,
            created_seq: 0,
            created_at: "2026-05-09T00:00:00.000Z",
            updated_at: "2026-05-09T00:00:00.000Z",
        }
    }
}

/// Insert one current-state item plus its `display`-source resolver row.
pub fn insert_fixture_item(conn: &Connection, item: FixtureItem<'_>) -> rusqlite::Result<()> {
    let container_class = item
        .container_id
        .map(|_| item.container_class.unwrap_or("epic"));
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "insert into items(\
            id, display_value, item_class, ticket_kind, priority, title, body, \
            container_id, container_class, origin, backend_kind, backend_key, \
            status, created_seq, created_at, updated_at\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
            item.status,
            item.created_seq,
            item.created_at,
            item.updated_at,
        ],
    )?;
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

/// Return the current count of rows in the `mutations` outbox.
pub fn mutation_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("select count(*) from mutations", [], |r| r.get(0))
}

/// Raw Mutation Log fixture for sync engine and read-side outbox tests.
///
/// Bypasses production `mutations::append` so slices can seed `failed`,
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
            state, failure_json, created_at, state_changed_at\
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
