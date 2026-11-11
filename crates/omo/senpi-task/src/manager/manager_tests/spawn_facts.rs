//! `manager/manager-spawn-facts.test.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::fakes::{FakeHandle, FakeRunner, HarnessOptions, base_spec, make_manager, started};
use crate::host::HostError;
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::helpers::record_spawned_pid;
use crate::manager::types::ManagerStartSpec;
use crate::runners::RunnerOutcome;
use crate::state::TaskRecord;
use crate::store::{TaskRecordStore, parse_task_record};

fn running_record(status: &str) -> TaskRecord {
    let raw: Value = json!({
        "version": 1,
        "task_id": "st_deadbeef",
        "name": "t",
        "parent_session_id": "parent-1",
        "root_session_id": "parent-1",
        "depth": 1,
        "execution_mode": "process",
        "model": "omo-mock/mock-1",
        "status": status,
        "residency_state": "resident",
        "created_at": "2026-07-07T00:00:00.000Z",
        "updated_at": "2026-07-07T00:00:00.000Z",
        "notify_on_terminal": false,
        "notification": { "run_epoch": 0, "notified_epoch": 0 },
    });
    parse_task_record(&raw, "/tmp/record.json", &mut Vec::new()).expect("parse")
}

fn process_spec() -> ManagerStartSpec {
    ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        ..base_spec()
    }
}

#[test]
fn given_running_record_and_real_pid_when_folded_then_pid_set_on_copy() {
    let record = running_record("running");

    let updated = record_spawned_pid(&record, Some(4242));

    assert_eq!(updated.and_then(|updated| updated.pid), Some(4242));
    assert_eq!(record.pid, None);
}

#[test]
fn given_no_pid_when_folded_then_nothing_changes() {
    assert_eq!(record_spawned_pid(&running_record("running"), None), None);
}

#[test]
fn given_record_already_terminal_when_folded_then_left_untouched() {
    assert_eq!(
        record_spawned_pid(&running_record("completed"), Some(4242)),
        None
    );
}

#[test]
fn given_member_launch_env_when_process_task_starts_then_task_id_threaded_into_child_env() {
    let process = FakeRunner::new();
    let harness = make_manager(HarnessOptions {
        process: Some(process.clone()),
        ..HarnessOptions::default()
    });
    let member_env = BTreeMap::from([
        (
            "SENPI_TASK_MEMBER".to_string(),
            "11111111-1111-4111-8111-111111111111::alpha".to_string(),
        ),
        ("SENPI_TASK_TEAM_CONFIG".to_string(), "{}".to_string()),
    ]);

    let task = started(harness.manager.start(&ManagerStartSpec {
        member_env: Some(member_env.clone()),
        ..process_spec()
    }));

    let mut expected = member_env;
    expected.insert("SENPI_TASK_MEMBER_TASK_ID".to_string(), task.task_id);
    let specs = process.specs();
    assert_eq!(
        specs.first().and_then(|spec| spec.member_env.clone()),
        Some(expected)
    );
}

#[test]
fn given_process_runner_with_pid_when_process_task_starts_then_record_carries_pid() {
    let process = FakeRunner::new();
    *process.child_pid.lock().expect("pid") = Some(4242);
    let harness = make_manager(HarnessOptions {
        process: Some(process.clone()),
        ..HarnessOptions::default()
    });

    let task = started(harness.manager.start(&process_spec()));

    assert_eq!(process.specs().len(), 1);
    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.pid, Some(4242));
}

#[test]
fn given_in_process_runner_without_pid_when_task_starts_then_record_has_no_pid() {
    let harness = make_manager(HarnessOptions::default());

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::InProcess),
        ..base_spec()
    }));

    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.pid, None);
}

// ---- manager/spawn-facts-effect-seq.test.ts ----

/// omo#10708: the manager stamps a child's spawn facts onto the task record while sends may
/// already be minting input ids into that record (runners/input_effects.rs). The manager reads the
/// child's pid while it stamps, and a send lands right then, minting its id the way
/// input_effects.rs does. Each mint starts from the stored seq, so a mint lost to the stamp leaves
/// the final seq below the number of mints even when a later read heals the counter.
struct SendDuringStampHandle {
    inner: Arc<FakeHandle>,
    store: TaskRecordStore,
    minted: Arc<AtomicU64>,
}

impl ManagedChildHandle for SendDuringStampHandle {
    fn task_id(&self) -> &str {
        self.inner.task_id()
    }
    fn session_id(&self) -> Option<String> {
        self.inner.session_id()
    }
    fn pid(&self) -> Option<i64> {
        self.store
            .mutate(self.inner.task_id(), |record| {
                let seq = record.effect_seq.unwrap_or(0).max(
                    record
                        .host_session
                        .as_ref()
                        .and_then(|s| s.effect_seq)
                        .unwrap_or(0),
                ) + 1;
                self.minted.fetch_add(1, Ordering::SeqCst);
                let mut next = record.clone();
                next.effect_seq = Some(seq);
                if let Some(host) = next.host_session.as_mut() {
                    host.effect_seq = Some(seq);
                }
                next
            })
            .expect("mint");
        Some(4242)
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.inner.steer(text)
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.inner.follow_up(text)
    }
    fn abort(&self) -> Result<(), HostError> {
        self.inner.abort()
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.inner.subscribe(listener)
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        self.inner.wait_for_outcome()
    }
    fn last_assistant_text(&self) -> Option<String> {
        self.inner.last_assistant_text()
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.inner.dispose()
    }
}

#[test]
fn given_send_minting_input_id_while_manager_stamps_spawn_facts_when_stamp_lands_then_minted_effect_seq_is_kept()
 {
    let process = FakeRunner::new();
    let harness = make_manager(HarnessOptions {
        process: Some(process.clone()),
        ..HarnessOptions::default()
    });
    let minted = Arc::new(AtomicU64::new(0));
    let store = harness.store.clone();
    let observed = Arc::clone(&minted);
    *process.hook.lock().expect("hook") = Some(Arc::new(move |spec, _| {
        let handle: Arc<dyn ManagedChildHandle> = Arc::new(SendDuringStampHandle {
            inner: FakeHandle::new(&spec.task_id, None),
            store: store.clone(),
            minted: Arc::clone(&observed),
        });
        Some(Ok(handle))
    }));

    let task = started(harness.manager.start(&process_spec()));

    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    let mints = minted.load(Ordering::SeqCst);
    assert!(mints > 0);
    assert_eq!(record.effect_seq, Some(mints));
    assert_eq!(record.pid, Some(4242));
}
