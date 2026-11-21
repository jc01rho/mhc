//! Port of `lifecycle/host-session-cancel-driver.ts` (omo#11765): re-drives a no-handle host-session
//! cancel that did not end on its first pass. Nothing else would in a live parent - the pending-cancel
//! finish runs only at session start, and the omo#9403 deferred stop resolves on a handle's
//! connection, which this child has none of.
//!
//! The caller just ran a pass, so the driver starts by waiting. Every exit but an engine dispose
//! settles the stop exactly once; a confirmed end finishes the cancel before that settlement.

use std::sync::Arc;

use super::context::LifecycleContext;
use super::deferred_revival::retries_stopped;
use super::host_session::HostSessionCancelSink;
use super::host_session_end::end_host_session_child;
use crate::state::TaskRecord;

pub fn drive_host_session_cancel(
    context: LifecycleContext,
    record: TaskRecord,
    sink: Arc<dyn HostSessionCancelSink>,
) {
    let fallback = Arc::clone(&sink);
    let task_id = record.task_id.clone();
    let spawned = std::thread::Builder::new()
        .name("senpi-task-host-session-cancel".to_string())
        .spawn(move || run_driver(&context, &record, sink.as_ref()));
    if let Err(error) = spawned {
        utils::logger::log(
            "senpi-task host session cancel driver failed",
            Some(&serde_json::json!({ "taskId": task_id, "error": error.to_string() })),
        );
        fallback.settle();
    }
}

fn run_driver(context: &LifecycleContext, record: &TaskRecord, sink: &dyn HostSessionCancelSink) {
    let Some(ended) = drive(context, record) else {
        return;
    };
    if ended {
        sink.ended();
    }
    sink.settle();
}

/// `Some(true)`: the end was confirmed; `Some(false)`: the driver gave up or the task moved on;
/// `None`: the engine was disposed, which settles nothing.
fn drive(context: &LifecycleContext, record: &TaskRecord) -> Option<bool> {
    for &backoff_ms in &context.host_retry.deferred_retry_backoff_ms {
        (context.host_retry_wait)(backoff_ms);
        if context.scoped_retries.is_disposed() {
            return None;
        }
        if retries_stopped(context, &record.parent_session_id) || !still_ours(context, record) {
            return Some(false);
        }
        if end_host_session_child(context, record).is_ended() {
            return Some(true);
        }
    }
    Some(false)
}

fn still_ours(context: &LifecycleContext, record: &TaskRecord) -> bool {
    let Ok(Some(fresh)) = context.store.load(&record.task_id) else {
        return false;
    };
    !fresh.status.is_terminal()
        && fresh.killed != Some(true)
        && fresh.notification.run_epoch == record.notification.run_epoch
        && fresh.residency_claim == record.residency_claim
}
