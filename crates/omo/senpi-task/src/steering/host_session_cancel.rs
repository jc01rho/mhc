//! Port of `steering/host-session-cancel.ts` (omo#11765): task_cancel on a daemon-hosted child this
//! process holds no handle for. The session may be running on a generation the recorded socket no
//! longer reaches, so the record is cancelled only after the lifecycle confirmed the session's end:
//! until then the answer is `cancel_pending`, the obligation stays on the record, and the lifecycle's
//! driver keeps trying. A repeat cancel joins the one already pending.

use std::sync::Arc;

use serde_json::json;

use super::{SteeringEngine, cancel_pending, stale_cancel};
use crate::lifecycle::{DestroyCause, HostSessionCancelSink, HostSessionEnd};
use crate::state::{CancelRequest, TaskRecord, TaskStatus, TaskTransition};
use crate::steering::types::{CancelOutcome, SteeringError, SteeringPort};

impl SteeringEngine {
    pub(super) fn cancel_unhandled_host_session(
        &self,
        record: &TaskRecord,
        reason: Option<&str>,
        expected: Option<i64>,
    ) -> Result<CancelOutcome, SteeringError> {
        if record.cancel_requested.is_some() {
            return Ok(cancel_pending(record));
        }
        let requested_at = self.now_iso();
        let mut moved: Option<CancelOutcome> = None;
        self.port.store.mutate(&record.task_id, |fresh| {
            if let Some(expected) = expected
                && fresh.notification.run_epoch != expected
            {
                moved = Some(stale_cancel(fresh));
                return fresh.clone();
            }
            TaskRecord {
                cancel_requested: Some(CancelRequest {
                    requested_at: requested_at.clone(),
                    reason: reason.map(str::to_string),
                }),
                ..fresh.clone()
            }
        })?;
        if let Some(moved) = moved {
            return Ok(moved);
        }
        let mut payload = json!({ "unreachable": true });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
        self.event(&record.task_id, "cancel_requested", payload)?;
        let Some(cancel) = self.port.destruction.host_session_cancel() else {
            return Ok(cancel_pending(record));
        };
        if let Some(stop_requested) = &self.port.stop_requested {
            stop_requested(&record.task_id);
        }
        let stop = Arc::new(UnhandledHostSessionStop {
            port: self.port.clone(),
            task_id: record.task_id.clone(),
            previous_status: record.status,
            reason: reason.map(str::to_string),
        });
        match cancel.end(record) {
            HostSessionEnd::Ended(_) => {
                stop.ended();
                stop.settle();
                Ok(CancelOutcome::Cancelled {
                    task_id: record.task_id.clone(),
                    previous_status: record.status,
                })
            }
            HostSessionEnd::NotEnded(why) => {
                cancel.drive(record.clone(), stop);
                Ok(CancelOutcome::CancelPending {
                    task_id: record.task_id.clone(),
                    previous_status: record.status,
                    reason: why.to_string(),
                })
            }
        }
    }

    /// The cancel's `finish`: once the session's end is confirmed, cancel the record and tear the
    /// child down. A record that moved on meanwhile is left as it is.
    fn finish_unhandled_host_session_cancel(
        &self,
        task_id: &str,
        previous_status: TaskStatus,
        reason: Option<&str>,
    ) -> Result<(), SteeringError> {
        let run_stats = (self.port.run_stats_snapshot)(task_id);
        let result = self.port.store.transition(
            task_id,
            &TaskTransition::Cancel {
                timestamp: self.now_iso(),
                error_message: reason.map(str::to_string),
                run_stats,
            },
        )?;
        if !result.applied {
            return Ok(());
        }
        self.clear_persisted_queue(task_id, None)?;
        let mut payload = json!({ "previous_status": previous_status.as_str() });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
        self.event(task_id, "cancelled", payload)?;
        self.port
            .destruction
            .destroy_resident_task(task_id, DestroyCause::Cancel)?;
        Ok(())
    }
}

struct UnhandledHostSessionStop {
    port: SteeringPort,
    task_id: String,
    previous_status: TaskStatus,
    reason: Option<String>,
}

impl HostSessionCancelSink for UnhandledHostSessionStop {
    fn ended(&self) {
        let engine = SteeringEngine::new(self.port.clone());
        if let Err(error) = engine.finish_unhandled_host_session_cancel(
            &self.task_id,
            self.previous_status,
            self.reason.as_deref(),
        ) {
            utils::logger::log(
                "senpi-task host session cancel could not finish",
                Some(&json!({ "taskId": self.task_id, "error": error.to_string() })),
            );
        }
    }

    fn settle(&self) {
        if let Some(stop_settled) = &self.port.stop_settled {
            stop_settled(&self.task_id);
        }
    }
}
