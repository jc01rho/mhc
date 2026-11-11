//! `run-pause-bridge.ts`: bridge the host's native Stop/Resume onto persistent run custody.
//!
//! The host reports a hold change; this bridge validates it (a stale epoch is ignored, not applied),
//! serializes propagation and resume, restores custody BEFORE the lifecycle reconcile runs, and
//! retries the participants a failed dispatch left outside the run's custody.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use senpi_task::pause::{
    admission_barrier::{AdmissionState, ResumeCredential},
    ports::RunRef,
    registry::PausePhase,
};

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
                self.resume_locked(parent_session_id, &request)
            }
        }
    }

    /// `releaseStale` (#12343): a Resume this bridge never acted on (a lost hold-change or held-wake
    /// event) leaves the run's Stop applied while the engine holds no Stop at all, so nothing would
    /// ever release it. When the engine's live ledger, read only once the recorded Stop could be
    /// stale, is at or past that Stop and holds no user Stop, the Stop was released. Only a Stop
    /// left `Applied` with no Stop or Resume in flight is stale: one in flight is left to the path
    /// that owns it and never joined, because a child's resume turn can take as long as its own
    /// pre-prompt work.
    pub fn release_stale(
        &self,
        parent_session_id: &str,
        live: impl FnOnce() -> Option<EngineHoldState>,
    ) -> BridgeOutcome {
        let _operation = match self.operation.try_lock() {
            Ok(operation) => operation,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return BridgeOutcome::Ignored,
        };
        let Some(record) = self.engine.restore(parent_session_id) else {
            return BridgeOutcome::Ignored;
        };
        // SAFETY-DIVERGENCE(omo run-pause-bridge.ts releaseStale): upstream's restore() rebuilds the
        // admission barrier from the registry at startup; mhc's does not, so a Stop recorded by an
        // earlier process meets an open barrier here, and the credentialed Resume would only write a
        // `resuming` ledger and dispatch guidance before failing. Only a Stop this process's barrier
        // still holds is released.
        if record.phase != PausePhase::Applied
            || self.engine.admission_state(parent_session_id)
                != (AdmissionState::Closed {
                    pause_seq: record.pause_seq,
                })
        {
            return BridgeOutcome::Ignored;
        }
        let Some(state) = live() else {
            return BridgeOutcome::Ignored;
        };
        let hold = read_engine_run_hold(&state);
        if hold.pause_seq < record.pause_seq || hold.stopped {
            return BridgeOutcome::Ignored;
        }
        self.resume_locked(
            parent_session_id,
            &ResumeRequest {
                pause_seq: record.pause_seq,
                resume_id: format!("parent-resume:{}", record.pause_seq),
            },
        )
    }

    /// The Resume of one recorded Stop; the caller holds the operation lock.
    fn resume_locked(&self, parent_session_id: &str, request: &ResumeRequest) -> BridgeOutcome {
        if self
            .last_applied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|seq| seq > request.pause_seq)
        {
            return BridgeOutcome::StaleResume;
        }
        // A Stop another path already released is a replay: the next turn's stale release runs
        // before the held wakes of the same input settle (#12343). Admission is open and the
        // recovery owns any lost participant, so a second Resume could only fail at the barrier.
        if self
            .engine
            .restore(parent_session_id)
            .is_some_and(|record| {
                record.pause_seq == request.pause_seq && record.phase == PausePhase::Resumed
            })
        {
            return BridgeOutcome::Ignored;
        }
        match self
            .engine
            .resume(parent_session_id, request.pause_seq, &request.resume_id)
        {
            Ok(senpi_task::pause::run_controller::RunResumeResult::Installed { failures }) => {
                let released =
                    self.engine
                        .release(parent_session_id, request.pause_seq, &request.resume_id);
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        let (engine, bridge, ended) = bridge_in(dir.path(), resume, recovery);
        (dir, engine, bridge, ended)
    }

    /// `bridge_over` in a given state dir, so a second engine can play the restarted process.
    fn bridge_in(
        dir: &std::path::Path,
        resume: Arc<ResumeFn>,
        recovery: LostParticipantRecovery,
    ) -> (Arc<EngineRunPause>, EngineRunPauseBridge, EndedLog) {
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
        let engine = Arc::new(EngineRunPause::new(dir, Arc::new(effects)));
        let bridge = EngineRunPauseBridge::with_lost_recovery(engine.clone(), recovery);
        (engine, bridge, ended)
    }

    /// A child that resumes at once, counting each resume.
    fn counting_resume() -> (Arc<AtomicUsize>, Arc<ResumeFn>) {
        let resumes = Arc::new(AtomicUsize::new(0));
        let counted = resumes.clone();
        (
            resumes,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(ResumeParticipantResult::Resumed { turn_id: None })
            }),
        )
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

    fn stop_at(bridge: &EngineRunPauseBridge, stop: u64) {
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
    }

    fn stop_then_resume(bridge: &EngineRunPauseBridge, stop: u64, resume: u64) -> BridgeOutcome {
        stop_at(bridge, stop);
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

    /// #12343: a Resume the bridge never acted on leaves the Stop applied with no engine Stop left
    /// to release, so every later spawn reads "held by your Stop". The next turn releases it, and
    /// only once the engine's live ledger proves the recorded Stop was released: at its sequence
    /// as much as past it.
    #[test]
    fn a_stop_whose_resume_the_bridge_missed_is_released_once_the_engine_holds_no_stop_at_or_past_its_seq()
     {
        for released_seq in [7, 8] {
            let (resumes, resume) = counting_resume();
            let (_dir, engine, bridge, _) = bridge_over(resume, LostParticipantRecovery::default());
            stop_at(&bridge, 7);

            // The Stop is still live, an engine ledger older than the recorded Stop proves nothing,
            // and neither does a ledger the host cannot report.
            for live in [Some(hold(7, true)), Some(hold(6, false)), None] {
                assert_eq!(
                    bridge.release_stale(PARENT, || live),
                    BridgeOutcome::Ignored
                );
            }
            assert_eq!(resumes.load(Ordering::SeqCst), 0);
            assert_eq!(
                engine.restore(PARENT).expect("custody").phase,
                PausePhase::Applied
            );
            assert_eq!(
                engine.admission_state(PARENT),
                AdmissionState::Closed { pause_seq: 7 }
            );

            // The engine released the Stop but the bridge never saw the Resume.
            assert_eq!(
                bridge.release_stale(PARENT, || Some(hold(released_seq, false))),
                BridgeOutcome::Resumed {
                    installed: true,
                    failures: 0,
                    failed_keys: Vec::new(),
                },
                "released at {released_seq}"
            );
            let record = engine.restore(PARENT).expect("custody");
            assert_eq!(record.phase, PausePhase::Resumed);
            assert_eq!(record.participants["st_b"].state, ParticipantState::Resumed);
            assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
            assert_eq!(resumes.load(Ordering::SeqCst), 1);
        }
    }

    /// A Resume can already be in flight when the turn starts. The stale release must not join it
    /// (a child's resume turn can take as long as the child's own pre-prompt work), so it returns
    /// at once and starts no second child resume.
    #[test]
    fn release_stale_returns_at_once_while_a_resume_is_in_flight_and_starts_no_second_child_resume()
    {
        let resumes = Arc::new(AtomicUsize::new(0));
        let counted = resumes.clone();
        let (reached, reached_seen) = std::sync::mpsc::sync_channel::<()>(1);
        let (answer, answered) = std::sync::mpsc::channel::<()>();
        let answered = Mutex::new(answered);
        let (_dir, engine, bridge, _) = bridge_over(
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = reached.try_send(());
                // The child's resume does not answer until the test releases it.
                answered
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv()
                    .map_err(|error| error.to_string())?;
                Ok(ResumeParticipantResult::Resumed { turn_id: None })
            }),
            LostParticipantRecovery::default(),
        );
        let bridge = Arc::new(bridge);
        stop_at(&bridge, 7);
        let resuming = {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                bridge.apply(
                    PARENT,
                    &RunHoldChangeEvent {
                        state: hold(8, false),
                        change: RunHoldChange::Resumed,
                    },
                )
            })
        };
        reached_seen
            .recv_timeout(Duration::from_secs(10))
            .expect("the Resume reached the child");

        // Circuit breaker only: a release that joined the in-flight Resume never returns here.
        let (returned, outcome) = std::sync::mpsc::channel();
        {
            let bridge = bridge.clone();
            std::thread::spawn(move || {
                let _ = returned.send(bridge.release_stale(PARENT, || Some(hold(8, false))));
            });
        }
        assert_eq!(
            outcome.recv_timeout(Duration::from_secs(10)),
            Ok(BridgeOutcome::Ignored)
        );
        assert_eq!(resumes.load(Ordering::SeqCst), 1);

        answer.send(()).expect("answer the child");
        assert_eq!(
            resuming.join().expect("the Resume thread"),
            BridgeOutcome::Resumed {
                installed: true,
                failures: 0,
                failed_keys: Vec::new(),
            }
        );
        assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
        assert_eq!(resumes.load(Ordering::SeqCst), 1);
    }

    /// The held wakes of the input that released a Stop settle after its turn, so the turn's stale
    /// release runs first; the held-wake Resume that follows (`resume_current` names the restored
    /// Stop's own sequence) replays a released Stop and resumes no child twice.
    #[test]
    fn the_held_wake_resume_after_a_stale_release_is_a_replay_that_resumes_no_child_twice() {
        let (resumes, resume) = counting_resume();
        let (_dir, engine, bridge, _) = bridge_over(resume, LostParticipantRecovery::default());
        stop_at(&bridge, 7);
        assert!(matches!(
            bridge.release_stale(PARENT, || Some(hold(7, false))),
            BridgeOutcome::Resumed {
                installed: true,
                ..
            }
        ));

        assert_eq!(
            bridge.apply(
                PARENT,
                &RunHoldChangeEvent {
                    state: hold(7, false),
                    change: RunHoldChange::Resumed,
                },
            ),
            BridgeOutcome::Ignored
        );
        assert_eq!(resumes.load(Ordering::SeqCst), 1);
        assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
    }

    /// SAFETY-DIVERGENCE(omo run-pause-bridge.ts releaseStale): a Stop an earlier process recorded
    /// meets this process's fresh, open barrier, so a turn start never runs the Resume that could
    /// only write a `resuming` ledger and fail.
    #[test]
    fn a_stop_an_earlier_process_recorded_is_not_released_through_an_open_barrier() {
        let dir = tempfile::tempdir().expect("state");
        let (resumes, resume) = counting_resume();
        let (_, before, _) = bridge_in(
            dir.path(),
            resume.clone(),
            LostParticipantRecovery::default(),
        );
        stop_at(&before, 7);

        let (engine, after, _) = bridge_in(dir.path(), resume, LostParticipantRecovery::default());
        assert_eq!(engine.admission_state(PARENT), AdmissionState::Open);
        assert_eq!(
            after.release_stale(PARENT, || Some(hold(8, false))),
            BridgeOutcome::Ignored
        );

        let record = engine.restore(PARENT).expect("custody");
        assert_eq!(record.phase, PausePhase::Applied);
        assert_eq!(record.resume, None);
        assert_eq!(resumes.load(Ordering::SeqCst), 0);
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
