//! Backend Adapter factory.
//!
//! [`open_configured`] reads the singleton Remote row via
//! [`crate::store::sync::get_remote`] and returns the concrete Adapter for it:
//! [`GithubAdapter`](super::github::GithubAdapter) for `github`,
//! [`OpenError::NotImplemented`] for `jira` (tk-35). The repository is resolved
//! by `gh` from the command cwd (ADR-0033), so the adapter is built from the
//! injected runner and cwd rather than from stored config. The engine's tests
//! bypass the factory and substitute [`crate::remote::fake::FakeAdapter`]
//! directly.

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

use super::adapter::Adapter;
use super::github::GithubAdapter;
use crate::domain::backend_kind::BackendKind;
use crate::proc::ProcRunner;
use crate::store::sync::get_remote;

/// Error returned by [`open_configured`].
#[derive(Debug, Error)]
pub enum OpenError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// The configured Remote's backend kind has no Adapter in this build —
    /// `jira` (tk-35). `tk sync` renders this as a verbatim diagnostic and
    /// exits 1.
    #[error("the configured Remote's adapter is not implemented in this build")]
    NotImplemented,
}

/// Look up the configured Remote and return an Adapter for it, borrowing the
/// `runner` and `cwd` the adapter drives `gh` through. Returns `Ok(None)` when
/// no Remote is configured.
pub fn open_configured<'a>(
    conn: &Connection,
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
) -> Result<Option<Box<dyn Adapter + 'a>>, OpenError> {
    let Some(remote) = get_remote(conn)? else {
        return Ok(None);
    };
    match remote.backend_kind.parse::<BackendKind>() {
        Ok(BackendKind::Github) => Ok(Some(Box::new(GithubAdapter::new(runner, cwd)))),
        // Jira has no adapter yet (tk-35); an unparseable kind cannot occur
        // under the `remotes.backend_kind` CHECK and maps here defensively.
        Ok(BackendKind::Jira) | Err(_) => Err(OpenError::NotImplemented),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::FakeRunner;
    use crate::store::migrations;
    use crate::store::testing::{FixtureRemote, insert_fixture_remote};
    use rusqlite::Connection;

    fn open_seeded() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        conn
    }

    #[test]
    fn returns_none_when_no_remote_configured() {
        let conn = open_seeded();
        let runner = FakeRunner::new();
        assert!(
            open_configured(&conn, &runner, Path::new("/tmp"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn github_remote_returns_an_adapter() {
        let conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: "{}",
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let runner = FakeRunner::new();
        assert!(matches!(
            open_configured(&conn, &runner, Path::new("/tmp")),
            Ok(Some(_))
        ));
    }

    #[test]
    fn jira_remote_returns_not_implemented() {
        let conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "jira",
                config_json: r#"{"site":"x","project":"P"}"#,
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        let runner = FakeRunner::new();
        assert!(matches!(
            open_configured(&conn, &runner, Path::new("/tmp")),
            Err(OpenError::NotImplemented)
        ));
    }
}
