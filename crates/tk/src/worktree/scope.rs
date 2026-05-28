//! Workspace Scope discovery and Repository-Store-side resolution.
//!
//! The git-side inputs are two short shell calls: `git config --worktree
//! --get tk.scope` (configured) and `git symbolic-ref --short HEAD`
//! (current branch for inference). Either may be absent or fail in
//! benign ways — both collapse to `None` rather than distinguishing
//! failure shapes.
//!
//! Resolution applies the configured value first via the case-insensitive
//! `item_ids` lookup. When unset, a branch name of shape `tk/<tail>`
//! triggers a longest-prefix match against `item_ids` so a slug like
//! `tk/proj-1-foo-bar` still picks up the `proj-1` Item. Two non-resolving
//! cases are distinguished:
//!
//! - configured value with no `item_ids` row → [`ResolveOutcome::ConfiguredUnresolved`]
//!   so callers can render `tk worktree: Workspace Scope '<stored>' is not …`.
//! - branch inference miss → [`ResolveOutcome::None`].

use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::proc::{ProcError, ProcRunner};
use crate::store::repository::Store;

const TICKET_BRANCH_PREFIX: &str = "tk/";

/// Workspace Scope and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub source: ScopeSource,
    /// Current Display ID after Alias resolution — what `tk worktree`
    /// renders to the user.
    pub display_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeSource {
    Configured,
    Inferred,
}

/// Raw discovery inputs handed off from the git-side reader.
#[derive(Debug, Default, Clone)]
pub struct Raw {
    /// Trimmed stdout of `git config --worktree --get tk.scope`, or `None`
    /// when the key is unset / the extension is disabled.
    pub configured_value: Option<String>,
    /// Trimmed stdout of `git symbolic-ref --short HEAD`, or `None` on a
    /// detached HEAD / git-error.
    pub branch_name: Option<String>,
}

/// Outcome of [`resolve_against_store`].
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// No Workspace Scope.
    None,
    /// Configured or inferred scope, resolved to a live item.
    Scope(Scope),
    /// `tk.scope` held a value the resolver could not find. Carries the
    /// stored string so callers can render it verbatim.
    ConfiguredUnresolved(String),
}

/// Read the two git-side inputs needed to resolve Workspace Scope.
///
/// Every benign git failure (key absent, `extensions.worktreeConfig`
/// disabled, detached HEAD, git binary missing, spawn failure) folds the
/// corresponding slot to `None`. Genuinely unexpected runner errors
/// surface — the trait's [`ProcError`] is the only variant tk emits.
pub fn read_git_side<R: ProcRunner + ?Sized>(runner: &R, cwd: &Path) -> Raw {
    Raw {
        configured_value: read_single_line(
            runner,
            cwd,
            &["git", "config", "--worktree", "--get", "tk.scope"],
        ),
        branch_name: read_single_line(runner, cwd, &["git", "symbolic-ref", "--short", "HEAD"]),
    }
}

fn read_single_line<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    argv: &[&str],
) -> Option<String> {
    let out = match runner.run(argv, cwd) {
        Ok(out) => out,
        Err(ProcError::ExecutableNotFound | ProcError::SpawnFailed) => return None,
    };
    if !out.succeeded() {
        return None;
    }
    // Path bytes pass through `String::from_utf8`; non-UTF-8 git output
    // collapses to `None` like every other benign failure.
    let stdout = String::from_utf8(out.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Resolve raw discovery output against the Repository Store.
pub fn resolve_against_store(store: &Store, raw: &Raw) -> Result<ResolveOutcome, rusqlite::Error> {
    if let Some(stored) = raw.configured_value.as_deref() {
        return match crate::store::repository::resolve_item_ref(store.conn(), stored)? {
            Some(resolved) => Ok(ResolveOutcome::Scope(load_scope(
                store,
                &resolved.id,
                ScopeSource::Configured,
            )?)),
            None => Ok(ResolveOutcome::ConfiguredUnresolved(stored.to_owned())),
        };
    }
    if let Some(branch) = raw.branch_name.as_deref() {
        if let Some(tail) = branch.strip_prefix(TICKET_BRANCH_PREFIX) {
            if let Some(item_id) = longest_prefix_match(store, tail)? {
                return Ok(ResolveOutcome::Scope(load_scope(
                    store,
                    &item_id,
                    ScopeSource::Inferred,
                )?));
            }
        }
    }
    Ok(ResolveOutcome::None)
}

fn load_scope(store: &Store, item_id: &str, source: ScopeSource) -> Result<Scope, rusqlite::Error> {
    let (display_id, title): (String, String) = store.conn().query_row(
        "select display_value, title from items where id = ?1",
        params![item_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Scope {
        source,
        display_id,
        title,
    })
}

/// Longest stored Display ID or Alias that is a `-`-bounded prefix of the
/// branch tail. The `-` boundary keeps `proj` from silently shadowing
/// `tk/project-1`; `collate nocase` matches the `item_ids` PK collation.
fn longest_prefix_match(store: &Store, tail: &str) -> Result<Option<String>, rusqlite::Error> {
    if tail.is_empty() {
        return Ok(None);
    }
    store
        .conn()
        .query_row(
            "select item_id \
               from item_ids \
              where (?1 = value collate nocase \
                     or ?1 like value || '-%' collate nocase) \
              order by length(value) desc \
              limit 1",
            params![tail],
            |r| r.get::<_, String>(0),
        )
        .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::migrations;
    use crate::store::testing::{FixtureItem, insert_alias, insert_fixture_item};
    use rusqlite::Connection;

    fn open_seeded() -> Store {
        let mut conn = Connection::open_in_memory().expect("open :memory:");
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_all(&mut conn, "2026-05-09T00:00:00.000Z").unwrap();
        Store { conn }
    }

    fn seed(store: &Store, id: &str, display: &str, created_seq: i64) {
        insert_fixture_item(
            store.conn(),
            FixtureItem {
                id,
                display,
                title: display,
                created_seq,
                ..FixtureItem::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn resolves_configured_scope_when_present() {
        let store = open_seeded();
        seed(&store, "t1", "tk-1", 1);
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: Some("tk-1".into()),
                branch_name: Some("tk/other".into()),
            },
        )
        .unwrap();
        match outcome {
            ResolveOutcome::Scope(s) => {
                assert_eq!(s.source, ScopeSource::Configured);
                assert_eq!(s.display_id, "tk-1");
            }
            other => panic!("expected Scope, got {other:?}"),
        }
    }

    #[test]
    fn reports_configured_unresolved_when_stored_value_misses() {
        let store = open_seeded();
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: Some("nope".into()),
                branch_name: None,
            },
        )
        .unwrap();
        assert!(matches!(
            outcome,
            ResolveOutcome::ConfiguredUnresolved(s) if s == "nope"
        ));
    }

    #[test]
    fn infers_scope_from_tk_branch_prefix_match() {
        let store = open_seeded();
        seed(&store, "t1", "tk-1", 1);
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: None,
                branch_name: Some("tk/tk-1-port-the-thing".into()),
            },
        )
        .unwrap();
        match outcome {
            ResolveOutcome::Scope(s) => {
                assert_eq!(s.source, ScopeSource::Inferred);
                assert_eq!(s.display_id, "tk-1");
            }
            other => panic!("expected Scope, got {other:?}"),
        }
    }

    #[test]
    fn longest_prefix_wins_over_shorter() {
        // Both `tk-1` and `tk-12` could match `tk-12-foo`; the longer
        // Display ID must win.
        let store = open_seeded();
        seed(&store, "t-short", "tk-1", 1);
        seed(&store, "t-long", "tk-12", 2);
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: None,
                branch_name: Some("tk/tk-12-foo".into()),
            },
        )
        .unwrap();
        match outcome {
            ResolveOutcome::Scope(s) => assert_eq!(s.display_id, "tk-12"),
            other => panic!("expected Scope, got {other:?}"),
        }
    }

    #[test]
    fn branch_without_tk_prefix_is_no_scope() {
        let store = open_seeded();
        seed(&store, "t1", "tk-1", 1);
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: None,
                branch_name: Some("main".into()),
            },
        )
        .unwrap();
        assert!(matches!(outcome, ResolveOutcome::None));
    }

    #[test]
    fn alias_resolves_through_branch_inference() {
        let store = open_seeded();
        seed(&store, "t1", "tk-1", 1);
        insert_alias(store.conn(), "alpha", "t1").unwrap();
        let outcome = resolve_against_store(
            &store,
            &Raw {
                configured_value: None,
                branch_name: Some("tk/alpha-extra".into()),
            },
        )
        .unwrap();
        match outcome {
            ResolveOutcome::Scope(s) => assert_eq!(s.display_id, "tk-1"),
            other => panic!("expected Scope, got {other:?}"),
        }
    }
}
