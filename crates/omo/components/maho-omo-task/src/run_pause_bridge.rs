//! `run-pause-bridge.ts`: bridge the host's native Stop/Resume onto persistent run custody.
//!
//! The host reports a hold change; this bridge validates it (a stale epoch is ignored, not applied),
//! serializes propagation and resume, restores custody BEFORE the lifecycle reconcile runs, and
//! retries the participants a failed dispatch left outside the run's custody.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use senpi_task::pause::{admission_barrier::ResumeCredential, ports::RunRef};

use crate::engine_hold_reader::{EngineHoldState, EngineRunHold, read_engine_run_hold};
use crate::engine_run_pause::EngineRunPause;

/// The hold-change event a host emits (`session_hold_changed`).
pub const SESSION_HOLD_CHANGED_EVENT: &str = "session_hold_changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunHoldChange {
    Paused,
    Resumed,
}

/// One native hold change: the projection, plus which way it moved.
#[derive(Debug, Clone, PartialEq)]
pub struct RunHoldChangeEvent {
    pub state: EngineHoldState,
    pub change: RunHoldChange,
}

/// `isRunHoldChange`: a payload that names the event and a legal direction.
pub fn is_run_hold_change(payload: &serde_json::Value) -> Option<RunHoldChange> {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some(SESSION_HOLD_CHANGED_EVENT) {
        return None;
    }
    match payload.get("change").and_then(serde_json::Value::as_str) {
        Some("paused") => Some(RunHoldChange::Paused),
        Some("resumed") => Some(RunHoldChange::Resumed),
        _ => None,
    }
}

/// What a Resume request must carry to be applied: the sequence and the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    pub pause_seq: u64,
    pub resume_id: String,
}

/// The bridge's answers, so the wiring can report them without re-deriving state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOutcome {
    Ignored,
    Paused,
    Resumed {
        installed: bool,
        failures: usize,
        /// The participants that stayed outside custody, so the wiring can name them and give them
        /// an explicit state instead of leaving them silently paused.
        failed_keys: Vec<String>,
    },
    /// The Resume named a sequence the run is no longer stopped at.
    StaleResume,
    Failed(String),
}

/// After a released Resume left participants outside the run, they are retried after each delay;
/// whatever is still lost after the last one has its run ended (#12249).
#[derive(Clone)]
pub struct LostParticipantRecovery {
    pub delays: Vec<Duration>,
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Default for LostParticipantRecovery {
    /// A restore that failed while the task host was changing generations usually succeeds a
    /// little later.
    fn default() -> Self {
        Self {
            delays: [5, 30, 120].map(Duration::from_secs).to_vec(),
            sleep: Arc::new(std::thread::sleep),
        }
    }
}

/// `EngineRunPauseBridge`: serialized Stop/Resume over one engine's custody.
pub struct EngineRunPauseBridge {
    engine: Arc<EngineRunPause>,
    /// Shared with the lost-participant recoveries, so a retry or the final end never runs beside
    /// another Stop or Resume of the same engine.
    operation: Arc<Mutex<()>>,
    last_applied: Mutex<Option<u64>>,
    lost_recovery: LostParticipantRecovery,
    recoveries: Mutex<BTreeMap<String, std::thread::JoinHandle<()>>>,
}

impl EngineRunPauseBridge {
    pub fn new(engine: Arc<EngineRunPause>) -> Self {
        Self::with_lost_recovery(engine, LostParticipantRecovery::default())
    }

    pub fn with_lost_recovery(
        engine: Arc<EngineRunPause>,
        lost_recovery: LostParticipantRecovery,
    ) -> Self {
        Self {
            engine,
            operation: Arc::new(Mutex::new(())),
            last_applied: Mutex::new(None),
            lost_recovery,
            recoveries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Test observability: waits for the parent's in-flight lost-child recovery, which settles once
    /// every lost child is resumed or ended.
    pub fn lost_recovery_settled(&self, parent_session_id: &str) {
        let recovery = self
            .recoveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(parent_session_id);
        if let Some(recovery) = recovery
            && recovery.join().is_err()
        {
            maho_ext_api::diagnostic("run resume lost-participant recovery panicked");
        }
    }

    /// Backgrounded: the hold-change handler must not wait out the retry schedule.
    fn recover_lost(&self, parent_session_id: &str, pause_seq: u64) {
        let engine = self.engine.clone();
        let operation = self.operation.clone();
        let recovery = self.lost_recovery.clone();
        let parent = parent_session_id.to_string();
        let handle = std::thread::spawn(move || {
            for delay in &recovery.delays {
                (recovery.sleep)(*delay);
                if !engine.has_lost(&parent, pause_seq) {
                    return;
                }
                let failures = {
                    let _operation = operation.lock().unwrap_or_else(PoisonError::into_inner);
                    engine.retry_lost(&parent, pause_seq)
                };
                if !failures.is_empty() {
                    maho_ext_api::diagnostic(&format!(
                        "run resume retry left {} participant(s) outside the run of {parent}",
                        failures.len()
                    ));
                }
                if !engine.has_lost(&parent, pause_seq) {
                    return;
                }
            }
            let failures = {
                let _operation = operation.lock().unwrap_or_else(PoisonError::into_inner);
                engine.end_lost(&parent, pause_seq)
            };
            for failure in failures {
                maho_ext_api::diagnostic(&format!(
                    "run resume could not end lost participant {} of {parent}: {}",
                    failure.key, failure.reason
                ));
            }
        });
        self.recoveries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(parent_session_id.to_string(), handle);
    }

    pub fn close_admission(&self, parent: &str, seq: u64) {
        self.engine.barrier.close(parent, seq);
    }

    /// Apply one native hold change. A change at or below the sequence already applied is ignored:
    /// replaying an old Resume must never reopen admission.
    pub fn apply(&self, parent_session_id: &str, event: &RunHoldChangeEvent) -> BridgeOutcome {
        let hold: EngineRunHold = read_engine_run_hold(&event.state);
        let _operation = self
            .operation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match event.change {
            RunHoldChange::Paused => {
                if !hold.stopped {
                    return BridgeOutcome::Ignored;
                }
                let mut applied = self
                    .last_applied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if applied.is_some_and(|seq| seq >= hold.pause_seq) {
                    return BridgeOutcome::Ignored;
                }
                *applied = Some(hold.pause_seq);
                drop(applied);
                match self.engine.pause(parent_session_id, hold.pause_seq) {
                    Ok(()) => BridgeOutcome::Paused,
                    Err(error) => BridgeOutcome::Failed(error),
                }
            }
            RunHoldChange::Resumed => {
                let Some(mut request) = resume_request(&hold) else {
                    return BridgeOutcome::Ignored;
                };
                // The Stop's run fences are engine holds too, so a Resume can carry a sequence past
                // the Stop it releases (#12249). It releases the run's Stop at or below that
                // sequence, never a newer one.
                if let Some(stop) = self
                    .engine
                    .restore(parent_session_id)
                    .map(|record| record.pause_seq)
                    .filter(|stop| *stop <= request.pause_seq)
                {
                    request = ResumeRequest {
                        pause_seq: stop,
                        resume_id: format!("parent-resume:{stop}"),
                    };
                }
                if self
                    .last_applied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some_and(|seq| seq > request.pause_seq)
                {
                    return BridgeOutcome::StaleResume;
                }
                match self
                    .engine
                    .resume(parent_session_id, request.pause_seq, &request.resume_id)
                {
                    Ok(senpi_task::pause::run_controller::RunResumeResult::Installed {
                        failures,
                    }) => {
                        let released = self.engine.release(
                            parent_session_id,
                            request.pause_seq,
                            &request.resume_id,
                        );
                        // A Resume that installed but never released admission is retried as lost.
                        let mut failed_keys: Vec<String> =
                            failures.iter().map(|failure| failure.key.clone()).collect();
                        if !released {
                            failed_keys.extend(
                                self.engine
                                    .retry_lost(parent_session_id, request.pause_seq)
                                    .into_iter()
                                    .map(|failure| failure.key),
                            );
                        } else if self.engine.has_lost(parent_session_id, request.pause_seq) {
                            self.recover_lost(parent_session_id, request.pause_seq);
                        }
                        BridgeOutcome::Resumed {
                            installed: released,
                            failures: failed_keys.len(),
                            failed_keys,
                        }
                    }
                    Ok(senpi_task::pause::run_controller::RunResumeResult::Superseded) => {
                        BridgeOutcome::StaleResume
                    }
                    Err(error) => BridgeOutcome::Failed(error),
                }
            }
        }
    }

    /// Startup restoration: custody is rebuilt BEFORE the lifecycle reconcile reads the store.
    pub fn restore(&self, parent_session_id: &str) -> Option<u64> {
        self.engine
            .restore(parent_session_id)
            .map(|record| record.pause_seq)
    }

    /// The fenced runs of a restored Stop, so the reconcile knows which children are held.
    pub fn fenced_runs(&self, parent_session_id: &str) -> Vec<RunRef> {
        self.engine.refs(parent_session_id)
    }
}

fn resume_request(hold: &EngineRunHold) -> Option<ResumeRequest> {
    if hold.stopped {
        return None;
    }
    Some(ResumeRequest {
        pause_seq: hold.pause_seq,
        resume_id: format!("parent-resume:{}", hold.pause_seq),
    })
}

/// How the composition reads a hold change off a host event. The Rust extension event carries no
/// hold projection of its own, so the host supplies the reader; `None` means "not a hold change".
pub type RunHoldChangeReader =
    Arc<dyn Fn(&maho_ext_api::ExtensionEvent) -> Option<RunHoldChangeEvent> + Send + Sync>;

/// `wireEventBridge`: register the hold-change handler on the host's event bus.
///
/// Custody is restored BEFORE the lifecycle reconcile runs (the restore edge), then the change is
/// applied, then the reconcile proceeds: an old Resume must never reopen admission before the
/// restored Stop is in place.
pub fn wire_run_pause_bridge(
    api: &mut ExtensionApi,
    bridge: Arc<EngineRunPauseBridge>,
    session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    read_hold_change: RunHoldChangeReader,
) {
    api.on(
        EventKind::Context,
        Arc::new(move |payload, _| {
            let bridge = bridge.clone();
            let session_id = session_id.clone();
            let read_hold_change = read_hold_change.clone();
            Box::pin(async move {
                let Some(event) = read_hold_change(payload) else {
                    return Ok(EventResult::None);
                };
                if let Some(session) = session_id() {
                    let _ = bridge.restore(&session);
                    let _ = bridge.apply(&session, &event);
                }
                Ok(EventResult::None)
            })
        }),
    );
}

/// The credential a resumed run presents; named here so the wiring and the bridge agree.
pub fn resume_credential(
    barrier: &senpi_task::pause::admission_barrier::AdmissionBarrier,
    parent: &str,
    pause_seq: u64,
    resume_id: &str,
) -> Result<ResumeCredential, senpi_task::pause::admission_barrier::AdmissionBarrierError> {
    barrier.resume(parent, pause_seq, resume_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_hold_reader::{RpcPauseRecord, RpcPauseState};
    use crate::engine_run_pause::{EngineRunPauseEffects, RunPauseSource};
    use senpi_task::pause::{
        admission_barrier::AdmissionState,
        registry::{ParticipantKind, ParticipantState, PauseParticipant},
        run_controller::ResumeParticipantResult,
    };
    use serde_json::json;

    const PARENT: &str = "parent";

    struct ChildSource;
    impl RunPauseSource for ChildSource {
        fn refs(&self, _: &str) -> Vec<RunRef> {
            Vec::new()
        }
        fn snapshot(&self, _: &str) -> Vec<PauseParticipant> {
            vec![PauseParticipant {
                key: "st_b".into(),
                kind: ParticipantKind::Task,
                run_ref: RunRef::task("st_b"),
                task_id: Some("st_b".into()),
                node_id: None,
                run_epoch: Some(0),
                admission: None,
                state: ParticipantState::Pending,
                ack_seq: None,
                observed_child_seq: None,
                resume_turn_id: None,
                lost_reason: None,
                at: String::new(),
            }]
        }
    }

    type ResumeFn = dyn Fn() -> Result<ResumeParticipantResult, String> + Send + Sync;
    type EndedLog = Arc<Mutex<Vec<(String, String)>>>;

    /// One held child `st_b` whose resume answers `resume`, and whose run end is recorded.
    fn bridge_over(
        resume: Arc<ResumeFn>,
        recovery: LostParticipantRecovery,
    ) -> (
        tempfile::TempDir,
        Arc<EngineRunPause>,
        EngineRunPauseBridge,
        EndedLog,
    ) {
        let dir = tempfile::tempdir().expect("state");
        let ended: EndedLog = Arc::new(Mutex::new(Vec::new()));
        let recorded = ended.clone();
        let effects = EngineRunPauseEffects {
            sources: Arc::new(Mutex::new(vec![
                Arc::new(ChildSource) as Arc<dyn RunPauseSource>
            ])),
            task_records: Arc::new(Vec::new),
            pause_task: Arc::new(|_, _| {
                Ok(Some(senpi_task::manager::run_pause::EngineInheritReceipt {
                    child_seq: 0,
                }))
            }),
            resume_task: Arc::new(move |_, _, _, _, _| resume()),
            observe_task_hold: Arc::new(|_, _| None),
            hold_task_input: Arc::new(|_, _| Err("unused".into())),
            fence_run: Arc::new(|_| Ok(())),
            release_fences: Arc::new(|_| Ok(())),
            cancel_task: Arc::new(|_| Ok(())),
            end_lost_task: Arc::new(move |task, _, reason| {
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((task.to_string(), reason.to_string()));
                Ok(true)
            }),
        };
        let engine = Arc::new(EngineRunPause::new(dir.path(), Arc::new(effects)));
        let bridge = EngineRunPauseBridge::with_lost_recovery(engine.clone(), recovery);
        (dir, engine, bridge, ended)
    }

    fn hold(pause_seq: u64, stopped: bool) -> EngineHoldState {
        EngineHoldState::Rpc(RpcPauseState {
            pause_seq,
            records: if stopped {
                vec![RpcPauseRecord {
                    kind: "session_hold".into(),
                    key: "session_hold".into(),
                    run_ref: None,
                    parent_session_id: None,
                }]
            } else {
                Vec::new()
            },
        })
    }

    fn stop_then_resume(bridge: &EngineRunPauseBridge, stop: u64, resume: u64) -> BridgeOutcome {
        assert_eq!(
            bridge.apply(
                PARENT,
                &RunHoldChangeEvent {
                    state: hold(stop, true),
                    change: RunHoldChange::Paused,
                },
            ),
            BridgeOutcome::Paused
        );
        bridge.apply(
            PARENT,
            &RunHoldChangeEvent {
                state: hold(resume, false),
                change: RunHoldChange::Resumed,
            },
        )
    }

    fn restore_failure() -> String {
        "paused child st_b restore failed (respawn_failed): rpc respawn failed".into()
    }

    /// #12249: the engine's run fences are holds, so each bumps its pause sequence past the
    /// Stop's, and the Resume carries the bumped sequence. It must still reach the paused child.
    #[test]
    fn a_resume_whose_engine_seq_counts_the_stops_run_fences_resumes_the_paused_child() {
        let (_dir, engine, bridge, _) = bridge_over(
            Arc::new(|| Ok(ResumeParticipantResult::Resumed { turn_id: None })),
            LostParticipantRecovery::default(),
        );
        let outcome = stop_then_resume(&bridge, 7, 8);

        assert_eq!(
            outcome,
            BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            }
        );
        let record = engine.restore(PARENT).expect("custody");
        assert_eq!(record.pause_seq, 7);
        assert_eq!(record.participants["st_b"].state, ParticipantState::Resumed);
        assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
    }

    #[test]
    fn a_child_a_released_resume_could_not_restore_is_retried_on_the_schedule_then_its_run_ends() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = attempts.clone();
        let slept = Arc::new(Mutex::new(Vec::<Duration>::new()));
        let sleeps = slept.clone();
        let (_dir, engine, bridge, ended) = bridge_over(
            Arc::new(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(restore_failure())
            }),
            LostParticipantRecovery {
                delays: vec![Duration::from_millis(5), Duration::from_millis(30)],
                sleep: Arc::new(move |delay| {
                    sleeps
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(delay);
                }),
            },
        );

        stop_then_resume(&bridge, 7, 7);
        bridge.lost_recovery_settled(PARENT);

        let ended = ended.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].0, "st_b");
        assert!(ended[0].1.contains("respawn_failed"), "{}", ended[0].1);
        assert_eq!(
            *slept.lock().unwrap_or_else(PoisonError::into_inner),
            vec![Duration::from_millis(5), Duration::from_millis(30)]
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(
            !engine
                .restore(PARENT)
                .expect("custody")
                .participants
                .contains_key("st_b"),
            "an ended run leaves custody"
        );
        assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
    }

    #[test]
    fn a_retry_that_reaches_the_lost_child_resumes_it_and_never_ends_its_run() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = attempts.clone();
        let (_dir, engine, bridge, ended) = bridge_over(
            Arc::new(move || {
                if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Err(restore_failure())
                } else {
                    Ok(ResumeParticipantResult::Resumed {
                        turn_id: Some("turn-b".into()),
                    })
                }
            }),
            LostParticipantRecovery {
                delays: vec![Duration::from_millis(5), Duration::from_millis(30)],
                sleep: Arc::new(|_| {}),
            },
        );

        stop_then_resume(&bridge, 7, 7);
        // The recovery settles only after it either resumed every lost child or ended them.
        bridge.lost_recovery_settled(PARENT);

        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            engine.restore(PARENT).expect("custody").participants["st_b"].state,
            ParticipantState::Resumed
        );
        assert!(
            ended
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
    }

    #[test]
    fn only_the_hold_change_event_with_a_legal_direction_is_recognised() {
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "paused"})),
            Some(RunHoldChange::Paused)
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "resumed"})),
            Some(RunHoldChange::Resumed)
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "session_hold_changed", "change": "bogus"})),
            None
        );
        assert_eq!(
            is_run_hold_change(&json!({"type": "other", "change": "paused"})),
            None
        );
    }
}
