//! Runs the sync engine for a command and prints the warnings its Pull raises.
//!
//! A warning follows a Pull that committed, so the dispatch seam, which frames
//! only failures, does not frame it; each line carries its own `tk <command>:`
//! prefix (ADR-0032).

use std::io::Write;

use rusqlite::Connection;

use crate::remote::adapter::Adapter;
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::sync::SkippedDependency;
use crate::sync::{self, RunSyncError, SyncReport};

/// Run one sync, then warn about each Dependency the Backend lists that Pull
/// did not import because it would close a cycle (ADR-0056).
pub(crate) fn sync_and_warn<W: Write + ?Sized>(
    stderr: &mut W,
    command: &str,
    conn: &mut Connection,
    adapter: &mut dyn Adapter,
    workflow: &RemoteWorkflowGuard,
    now: &str,
) -> Result<SyncReport, RunSyncError> {
    let result = sync::run_sync(conn, adapter, workflow, now);
    if let Ok(report) = &result {
        warn_skipped(stderr, command, &report.skipped_dependencies);
    }
    result
}

fn warn_skipped<W: Write + ?Sized>(stderr: &mut W, command: &str, skipped: &[SkippedDependency]) {
    for SkippedDependency {
        blocked_display_id: blocked,
        blocking_display_id: blocking,
        through_display_ids: through,
    } in skipped
    {
        let through = if through.is_empty() {
            String::new()
        } else {
            format!(" through {}", through.join(", "))
        };
        let _ = writeln!(
            stderr,
            "tk {command}: warning: did not import Dependency {blocked} blocked by {blocking}: {blocking} already waits on {blocked}{through}"
        );
    }
}
