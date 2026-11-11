//! `tools/output/polling-guard.test.ts`

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use pretty_assertions::assert_eq;

use crate::manager::types::{ListScope, ListedTask};
use crate::state::{TaskRecord, TaskStatus};
use crate::tools::control::tool_result::ToolResultContent;
use crate::tools::control::types::{SessionIdCarrier, SessionIdSource};
use crate::tools::output::output::{
    TaskOutputInput, TaskOutputMode, TaskOutputTool, create_task_output_tool,
};
use crate::tools::output::records_fakes::{RecordOverrides, make_record};
use crate::tools::output::types::{
    OutputManager, TaskOutputDeps, TaskOutputDetails, TaskOutputToolResult, TranscriptReadResult,
    TranscriptReaderInput, TranscriptSource,
};

/// A manager over records the test can replace between reads.
struct SharedManager {
    records: Arc<Mutex<Vec<TaskRecord>>>,
}

impl OutputManager for SharedManager {
    fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|record| record.task_id == task_id)
            .cloned()
    }

    fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|record| match scope {
                ListScope::ParentSession(session_id) => &record.parent_session_id == session_id,
                #[allow(unreachable_patterns)]
                _ => true,
            })
            .map(|record| ListedTask {
                record: record.clone(),
                queue_position: None,
            })
            .collect()
    }
}

struct Session(String);

impl SessionIdSource for Session {
    fn get_session_id(&self) -> String {
        self.0.clone()
    }
}

struct Context(Session);

impl SessionIdCarrier for Context {
    fn session_manager(&self) -> &dyn SessionIdSource {
        &self.0
    }
}

fn context(session_id: &str) -> Context {
    Context(Session(session_id.to_string()))
}

fn output_tool(records: Vec<TaskRecord>) -> (TaskOutputTool, Arc<Mutex<Vec<TaskRecord>>>) {
    let shared = Arc::new(Mutex::new(records));
    let tool = create_task_output_tool(TaskOutputDeps {
        manager: Arc::new(SharedManager {
            records: shared.clone(),
        }),
        state_dir: "/tmp/state".to_string(),
        transcript_reader: Some(Arc::new(
            |_input: &TranscriptReaderInput<'_>| -> io::Result<TranscriptReadResult> {
                Ok(TranscriptReadResult {
                    entries: Vec::new(),
                    source: TranscriptSource::None,
                    truncated: None,
                })
            },
        )),
        resolve_caller_session_id: None,
        now: None,
    });
    (tool, shared)
}

fn by_id(task_id: &str) -> TaskOutputInput {
    TaskOutputInput {
        task_id: Some(task_id.to_string()),
        ..TaskOutputInput::default()
    }
}

fn by_name(name: &str) -> TaskOutputInput {
    TaskOutputInput {
        name: Some(name.to_string()),
        ..TaskOutputInput::default()
    }
}

fn execute(tool: &TaskOutputTool, params: &TaskOutputInput, session: &str) -> TaskOutputToolResult {
    tool.execute("call", params, &context(session))
        .expect("task_output")
}

fn worker(task_id: &str, overrides: RecordOverrides) -> TaskRecord {
    make_record(RecordOverrides {
        task_id: Some(task_id.to_string()),
        name: Some("worker".to_string()),
        ..overrides
    })
}

fn no_progress_reason(result: &TaskOutputToolResult) -> String {
    match &result.details {
        TaskOutputDetails::NoProgress { reason, .. } => reason.clone(),
        other => panic!("expected no_progress, got {other:?}"),
    }
}

fn status_snapshot_final(result: &TaskOutputToolResult) -> Option<String> {
    match &result.details {
        TaskOutputDetails::Status { snapshot } => snapshot.final_response.clone(),
        other => panic!("expected status, got {other:?}"),
    }
}

#[test]
fn given_one_status_peek_when_the_same_caller_reads_the_unchanged_task_again_then_the_full_snapshot_is_not_replayed()
 {
    // given
    let parent_a = make_record(RecordOverrides {
        task_id: Some("st_parent_a".to_string()),
        name: Some("worker".to_string()),
        parent_session_id: Some("session-a".to_string()),
        ..RecordOverrides::default()
    });
    let parent_b = make_record(RecordOverrides {
        task_id: Some("st_parent_b".to_string()),
        name: Some("worker".to_string()),
        parent_session_id: Some("session-b".to_string()),
        ..RecordOverrides::default()
    });
    let (tool, records) = output_tool(vec![parent_a.clone(), parent_b.clone()]);

    // when
    let first = execute(&tool, &by_name("worker"), "session-a");
    let unchanged = execute(&tool, &by_id(&parent_a.task_id), "session-a");
    let diagnostic = execute(
        &tool,
        &TaskOutputInput {
            mode: Some(TaskOutputMode::Tail),
            ..by_name("worker")
        },
        "session-b",
    );
    let other_caller = execute(&tool, &by_name("worker"), "session-b");
    let mut progressed_record = parent_a.clone();
    progressed_record.status = TaskStatus::Completed;
    progressed_record.updated_at = "2024-12-03T14:01:00.000Z".to_string();
    progressed_record.final_response = Some("done".to_string());
    *records.lock().unwrap_or_else(PoisonError::into_inner) = vec![progressed_record, parent_b];
    let progressed = execute(&tool, &by_id(&parent_a.task_id), "session-a");

    // then
    assert_eq!(first.details.kind(), "status");
    match &unchanged.details {
        TaskOutputDetails::NoProgress {
            task_id, status, ..
        } => {
            assert_eq!(task_id, &parent_a.task_id);
            assert_eq!(*status, TaskStatus::Running);
        }
        other => panic!("expected no_progress, got {other:?}"),
    }
    let unchanged_json = serde_json::to_value(&unchanged.details).expect("json");
    assert_eq!(unchanged_json["kind"], "no_progress");
    assert_eq!(unchanged_json["status"], "running");
    assert!(unchanged_json.get("snapshot").is_none());
    assert_eq!(diagnostic.details.kind(), "transcript");
    assert_eq!(other_caller.details.kind(), "status");
    assert_eq!(status_snapshot_final(&progressed).as_deref(), Some("done"));
}

#[test]
fn given_an_unchanged_status_timestamp_when_only_the_child_pid_changes_then_the_next_status_peek_returns_a_fresh_snapshot()
 {
    let initial = worker(
        "st_pid",
        RecordOverrides {
            pid: Some(111),
            ..RecordOverrides::default()
        },
    );
    let (tool, records) = output_tool(vec![initial.clone()]);
    let session = initial.parent_session_id.clone();

    let first = execute(&tool, &by_id("st_pid"), &session);
    records.lock().unwrap_or_else(PoisonError::into_inner)[0].pid = Some(222);
    let after_pid = execute(&tool, &by_id("st_pid"), &session);
    let unchanged = execute(&tool, &by_id("st_pid"), &session);

    assert_eq!(first.details.kind(), "status");
    match &after_pid.details {
        TaskOutputDetails::Status { snapshot } => assert_eq!(snapshot.pid, Some(222)),
        other => panic!("expected status, got {other:?}"),
    }
    assert_eq!(unchanged.details.kind(), "no_progress");
}

#[test]
fn given_a_cached_status_read_when_the_session_is_compacted_then_the_next_peek_reissues_the_snapshot()
 {
    let record = worker(
        "st_compact",
        RecordOverrides {
            status: Some(TaskStatus::Completed),
            final_response: Some("done".to_string()),
            ..RecordOverrides::default()
        },
    );
    let session = record.parent_session_id.clone();
    let (tool, _records) = output_tool(vec![record]);

    assert_eq!(
        execute(&tool, &by_id("st_compact"), &session)
            .details
            .kind(),
        "status"
    );
    assert_eq!(
        execute(&tool, &by_id("st_compact"), &session)
            .details
            .kind(),
        "no_progress"
    );
    tool.forget_session(&session);
    let after_compact = execute(&tool, &by_id("st_compact"), &session);
    assert_eq!(
        status_snapshot_final(&after_compact).as_deref(),
        Some("done")
    );
}

#[test]
fn given_a_huge_final_response_when_status_is_polled_twice_then_the_cached_fingerprint_stays_bounded()
 {
    let huge = "x".repeat(256 * 1024);
    let record = worker(
        "st_huge",
        RecordOverrides {
            status: Some(TaskStatus::Completed),
            final_response: Some(huge),
            ..RecordOverrides::default()
        },
    );
    let session = record.parent_session_id.clone();
    let (tool, _records) = output_tool(vec![record]);

    let first = execute(&tool, &by_id("st_huge"), &session);
    let second = execute(&tool, &by_id("st_huge"), &session);

    assert_eq!(first.details.kind(), "status");
    assert_eq!(second.details.kind(), "no_progress");
    assert!(serde_json::to_string(&first).expect("json").len() > 256 * 1024);
    assert!(serde_json::to_string(&second).expect("json").len() < 4096);
}

#[test]
fn given_an_unchanged_cancelled_or_interrupted_task_when_status_is_polled_again_then_it_does_not_promise_a_completion_notification()
 {
    for status in [TaskStatus::Cancelled, TaskStatus::Interrupted] {
        let task_id = format!("st_{}", status.as_str());
        let record = worker(
            &task_id,
            RecordOverrides {
                status: Some(status),
                ..RecordOverrides::default()
            },
        );
        let session = record.parent_session_id.clone();
        let (tool, _records) = output_tool(vec![record]);

        assert_eq!(
            execute(&tool, &by_id(&task_id), &session).details.kind(),
            "status"
        );
        let reason = no_progress_reason(&execute(&tool, &by_id(&task_id), &session));
        assert!(reason.contains(&format!("Task {task_id} is {}.", status.as_str())));
        assert!(!reason.contains("Await its completion notification"));
    }
}

#[test]
fn given_an_unchanged_task_without_terminal_notifications_when_status_is_polled_again_then_it_does_not_promise_a_completion_notification()
 {
    for status in [
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Error,
        TaskStatus::Lost,
        TaskStatus::Cancelled,
        TaskStatus::Interrupted,
    ] {
        let task_id = format!("st_unnotified_{}", status.as_str());
        let record = worker(
            &task_id,
            RecordOverrides {
                status: Some(status),
                ..RecordOverrides::default()
            },
        );
        let session = record.parent_session_id.clone();
        let (tool, _records) = output_tool(vec![record]);

        execute(&tool, &by_id(&task_id), &session);
        let reason = no_progress_reason(&execute(&tool, &by_id(&task_id), &session));
        assert!(reason.contains("will not send a completion notification"));
        assert!(!reason.contains("Await its completion notification"));
    }
}

#[test]
fn given_an_unchanged_task_with_terminal_notifications_when_status_is_polled_again_then_it_directs_the_caller_to_await_the_notification()
 {
    let record = worker(
        "st_notified",
        RecordOverrides {
            status: Some(TaskStatus::Completed),
            notify_on_terminal: Some(true),
            ..RecordOverrides::default()
        },
    );
    let session = record.parent_session_id.clone();
    let (tool, _records) = output_tool(vec![record]);

    execute(&tool, &by_id("st_notified"), &session);
    let result = execute(&tool, &by_id("st_notified"), &session);
    // The model-facing text is the exact reason.
    assert_eq!(
        result.content,
        vec![ToolResultContent::Text {
            text: "Task st_notified has not changed since the last status read. Await its completion notification; use mode:\"tail\" only for explicit transcript diagnosis."
                .to_string()
        }]
    );
    assert!(no_progress_reason(&result).contains("Await its completion notification"));
}

#[test]
fn given_an_unchanged_legacy_terminal_task_with_a_failed_delivery_when_status_is_polled_again_then_it_directs_the_caller_to_await_the_retry()
 {
    let record = worker(
        "st_legacy_failed_delivery",
        RecordOverrides {
            status: Some(TaskStatus::Completed),
            notify_on_terminal: Some(false),
            run_epoch: Some(2),
            notified_epoch: Some(0),
            notification_failed_epoch: Some(2),
            ..RecordOverrides::default()
        },
    );
    let session = record.parent_session_id.clone();
    let (tool, _records) = output_tool(vec![record]);

    execute(&tool, &by_id("st_legacy_failed_delivery"), &session);
    let reason = no_progress_reason(&execute(
        &tool,
        &by_id("st_legacy_failed_delivery"),
        &session,
    ));
    assert!(reason.contains("Await its completion notification"));
    assert!(!reason.contains("will not send a completion notification"));
}

#[test]
fn given_an_unchanged_opted_in_background_task_when_status_is_polled_again_before_it_is_terminal_then_it_directs_the_caller_to_await_its_future_notification()
 {
    for status in [TaskStatus::Pending, TaskStatus::Running] {
        let task_id = format!("st_live_{}", status.as_str());
        let record = worker(
            &task_id,
            RecordOverrides {
                status: Some(status),
                notify_on_terminal: Some(true),
                ..RecordOverrides::default()
            },
        );
        let session = record.parent_session_id.clone();
        let (tool, _records) = output_tool(vec![record]);

        execute(&tool, &by_id(&task_id), &session);
        let reason = no_progress_reason(&execute(&tool, &by_id(&task_id), &session));
        assert!(reason.contains("Await its completion notification"));
    }
}

#[test]
fn given_an_unchanged_task_without_a_deliverable_completion_notification_when_status_is_polled_again_then_it_does_not_promise_one()
 {
    let records = [
        worker(
            "st_cancelled_notification",
            RecordOverrides {
                status: Some(TaskStatus::Cancelled),
                notify_on_terminal: Some(true),
                ..RecordOverrides::default()
            },
        ),
        worker(
            "st_delivered_notification",
            RecordOverrides {
                status: Some(TaskStatus::Completed),
                notify_on_terminal: Some(true),
                run_epoch: Some(1),
                notified_epoch: Some(1),
                ..RecordOverrides::default()
            },
        ),
    ];

    for record in records {
        let task_id = record.task_id.clone();
        let session = record.parent_session_id.clone();
        let (tool, _records) = output_tool(vec![record]);

        execute(&tool, &by_id(&task_id), &session);
        let reason = no_progress_reason(&execute(&tool, &by_id(&task_id), &session));
        assert!(!reason.contains("Await its completion notification"));
    }
}

#[test]
fn given_a_lost_task_when_tail_is_read_twice_then_both_peeks_return_the_lost_snapshot() {
    let record = worker(
        "st_lost",
        RecordOverrides {
            status: Some(TaskStatus::Lost),
            ..RecordOverrides::default()
        },
    );
    let session = record.parent_session_id.clone();
    let (tool, _records) = output_tool(vec![record]);
    let tail = TaskOutputInput {
        mode: Some(TaskOutputMode::Tail),
        ..by_id("st_lost")
    };

    let first = execute(&tool, &tail, &session);
    let second = execute(&tool, &tail, &session);

    match &first.details {
        TaskOutputDetails::Status { snapshot } => assert_eq!(snapshot.status, TaskStatus::Lost),
        other => panic!("expected status, got {other:?}"),
    }
    assert_eq!(second.details.kind(), "status");
}

#[test]
fn given_more_callers_than_the_status_cache_limit_when_the_oldest_caller_reads_again_then_its_stale_fingerprint_was_evicted()
 {
    // given: each caller session reads the same task, re-parented so it stays in that caller's scope.
    let (tool, records) = output_tool(Vec::new());
    let shared = |session: &str| {
        let mut record = worker("st_shared", RecordOverrides::default());
        record.parent_session_id = session.to_string();
        record
    };

    // when
    *records.lock().unwrap_or_else(PoisonError::into_inner) = vec![shared("session-0")];
    let first = execute(&tool, &by_id("st_shared"), "session-0");
    let repeat = execute(&tool, &by_id("st_shared"), "session-0");
    for index in 1..=1024 {
        let session = format!("session-{index}");
        *records.lock().unwrap_or_else(PoisonError::into_inner) = vec![shared(&session)];
        execute(&tool, &by_id("st_shared"), &session);
    }
    *records.lock().unwrap_or_else(PoisonError::into_inner) = vec![shared("session-0")];
    let after_eviction = execute(&tool, &by_id("st_shared"), "session-0");

    // then
    assert_eq!(first.details.kind(), "status");
    assert_eq!(repeat.details.kind(), "no_progress");
    assert_eq!(after_eviction.details.kind(), "status");
}
