//! Backend Adapter factory.
//!
//! [`open_configured`] reads the singleton Remote row via
//! [`crate::store::sync::get_remote`] and returns the Adapter for it. No
//! backend kind has a real Adapter yet — `github` / `jira` implementations
//! land in tk-40 — so a configured Remote returns
//! [`OpenError::NotImplemented`]. The engine's tests bypass the factory and
//! substitute [`crate::remote::fake::FakeAdapter`] directly.

use rusqlite::Connection;
use thiserror::Error;

use super::adapter::Adapter;
use crate::store::sync::get_remote;

/// Error returned by [`open_configured`].
#[derive(Debug, Error)]
pub enum OpenError {
    #[error(transparent)]
    Storage(#[from] rusqlite::Error),
    /// A Remote row exists but its backend kind has no Adapter implementation
    /// in this build (real `github` / `jira` adapters land in tk-40). `tk sync`
    /// renders this as a verbatim diagnostic and exits 1.
    #[error("the configured Remote's adapter is not implemented in this build")]
    NotImplemented,
}

/// Look up the configured Remote and return an Adapter for it.
///
/// Returns `Ok(None)` when no Remote is configured. Until real adapters land,
/// any configured Remote returns [`OpenError::NotImplemented`]; the
/// `Ok(Some(_))` arm is reachable only once tk-40 ships a concrete adapter.
pub fn open_configured(conn: &Connection) -> Result<Option<Box<dyn Adapter>>, OpenError> {
    match get_remote(conn)? {
        None => Ok(None),
        Some(_) => Err(OpenError::NotImplemented),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(open_configured(&conn).unwrap().is_none());
    }

    #[test]
    fn github_remote_returns_not_implemented() {
        let conn = open_seeded();
        insert_fixture_remote(
            &conn,
            FixtureRemote {
                backend_kind: "github",
                config_json: r#"{"repo":"o/r"}"#,
                ..FixtureRemote::default()
            },
        )
        .unwrap();
        assert!(matches!(
            open_configured(&conn),
            Err(OpenError::NotImplemented)
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
        assert!(matches!(
            open_configured(&conn),
            Err(OpenError::NotImplemented)
        ));
    }
}
