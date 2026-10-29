//! Runs the sync engine for a command and prints the warnings its Pull raises.
//!
//! A warning follows a Pull that committed, so the dispatch seam, which frames
//! only failures, does not frame it; each line carries its own `tk <command>:`
//! prefix (ADR-0032).

use std::io::Write;

use rusqlite::Connection;

use crate::remote::adapter::Adapter;
use crate::store::repository::RemoteWorkflowGuard;
use crate::store::repository::dependency::through_clause;
use crate::store::sync::{SkippedContainment, SkippedDependency, SkippedRelationships};
use crate::sync::{self, RunSyncError, SyncReport};

/// Run one sync, then warn about each relationship the Backend lists that
/// Pull did not import because tk refuses it (ADR-0056, ADR-0057).
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
        warn_skipped(stderr, command, &report.skipped);
    }
    result
}

fn warn_skipped<W: Write + ?Sized>(stderr: &mut W, command: &str, skipped: &SkippedRelationships) {
    for SkippedDependency {
        blocked_display_id: blocked,
        blocking_display_id: blocking,
        through_display_ids: through,
    } in &skipped.dependencies
    {
        let through = through_clause(through);
        let _ = writeln!(
            stderr,
            "tk {command}: warning: did not import Dependency {blocked} blocked by {blocking}: {blocking} already waits on {blocked}{through}"
        );
    }
    for SkippedContainment {
        ticket_display_id: ticket,
        container_display_id: container,
        reason,
    } in &skipped.containments
    {
        let _ = writeln!(
            stderr,
            "tk {command}: warning: did not import Containment {ticket} held by {container}: {reason}"
        );
    }
}
