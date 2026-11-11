//! `manager/run-pause.ts`: the per-task half of a run's pause custody.
//!
//! A paused child is held by its own engine under exactly one run owner (the parent session, the
//! parent's pause sequence, and the run). This module owns the task-record side of that custody:
//! recording the owner on the record so a restarted host re-holds the child under the same run,
//! refusing a resume whose admission was superseded, and exposing the ports a child's engine
//! implements (`ChildPausePort`).
//!
//! The controller is deliberately synchronous: every port it drives is a request/response call into
//! the child's engine, and the Rust task crate has no async store. Callers that run inside an async
//! task use `spawn_blocking`, exactly as the workflow tool does for the scheduler.

pub type WaitingState = Mutex<BTreeMap<String, (i64, Vec<tokio::sync::oneshot::Sender<()>>)>>;
pub type SettlementsState = Mutex<BTreeMap<(String, i64), Vec<tokio::sync::oneshot::Sender<()>>>>;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::manager::child_handle::ManagedChildHandle;
use crate::pause::admission_barrier::{AdmissionBarrier, AdmissionEntry, ResumeCredential};
use crate::pause::ports::RunPauseOwner;
use crate::state::{RecordedRunPause, RunPauseKind, TaskRecord, TaskStatus};
use crate::store::{StoreError, TaskRecordStore};

/// What a child's engine answers when asked to hold its next run under `owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineInheritReceipt {
    pub child_seq: u64,
}

pub use crate::pause::ports::{EngineHoldInputResult, ObservedHold};

/// A resumed child's turn: the id the engine opened, which settles with that turn's outcome.
pub struct TaskResumedTurn {
    pub turn_id: String,
    pub outcome: crate::manager::child_handle::ExactTurnOutcome,
}

/// The input a paused child holds for the resumed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldTaskInput {
    pub input_id: String,
    pub revision: u64,
    pub text: String,
}

pub enum ResumeTurnResult {
    Resumed {
        turn_id: String,
        outcome: crate::manager::child_handle::ExactTurnOutcome,
    },
    OtherHoldLive {
        child_seq: Option<u64>,
    },
}

/// `RunPauseAdmissionHeld`: the run's Resume was superseded by a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the run Resume was superseded by a hold")]
pub struct RunPauseAdmissionHeld;

/// The child engine's own pause controls (senpi#3811). A child on an older engine exposes none.
pub trait TaskChildPausePort: Send + Sync {
    /// `None` is the engine's `refused: "wrong_parent"` answer.
    fn pause_for_run(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String>;
    /// `None` when the child's engine reports no pause view.
    fn observe_hold(
        &self,
        _child: &Arc<dyn ManagedChildHandle>,
        _parent_session_id: &str,
    ) -> Option<ObservedHold> {
        None
    }
    fn resume_turn(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        request: &ResumeTurnRequest,
    ) -> Result<ResumeTurnResult, String>;
    fn hold_input(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        input: &HeldTaskInput,
        owner: &RunPauseOwner,
    ) -> Result<EngineHoldInputResult, String>;
}

/// A resume request, from the parent's Resume down to the child's engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTurnRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}

/// A paused child that is being resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTaskRequest {
    pub allocation: Option<super::concurrency::QueuedTaskAdmission>,
    pub owner: RunPauseOwner,
    pub resume_credential: Option<ResumeCredential>,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}

/// A child the run recorded as lost: it is resumed only if its engine still holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostResumeRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub resume_credential: Option<ResumeCredential>,
    pub child_seq_cutoff: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeTaskResult {
    Resumed { turn_id: String },
    OtherHoldLive { child_seq: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostResumeResult {
    Resumed { turn_id: String },
    OtherHoldLive { child_seq: Option<u64> },
    NotHeld,
}

/// The effects a task run pause needs from the composition.
pub trait TaskRunPausePorts: Send + Sync {
    fn acquire_resume_capacity(
        &self,
        _task_id: &str,
        _epoch: i64,
        _allocation: Option<&super::concurrency::QueuedTaskAdmission>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn park_capacity(&self, _task_id: &str, _epoch: i64) {}
    fn begin_native_pause(&self, _task_id: &str, _epoch: i64) {}
    fn finish_native_pause(
        &self,
        _task_id: &str,
        _epoch: i64,
        _accepted: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn store(&self) -> &TaskRecordStore;
    /// Milliseconds since the epoch, as the store's timestamps are spelled.
    fn now(&self) -> i64;
    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>>;
    fn child(&self) -> Option<Arc<dyn TaskChildPausePort>>;
    fn barrier(&self) -> Option<AdmissionBarrier>;
    /// Record the run's owner on the task record so a restarted host re-holds it under the same run.
    fn remember_owner(&self, task_id: &str, owner: &RunPauseOwner) -> Result<(), StoreError>;
    fn forget_owner(&self, task_id: &str) -> Result<(), StoreError>;
    /// `transition({ type: "pause" })`; `false` when the record changed under the caller.
    fn apply_pause(&self, task_id: &str, timestamp: &str) -> bool;
    /// `transition({ type: "unpause" })`; `false` when the record changed under the caller.
    fn apply_unpause(&self, task_id: &str, timestamp: &str) -> bool;
    /// Rebuild a paused child whose host is gone; it starts no turn.
    fn restore(&self, record: &TaskRecord) -> Option<Result<Arc<dyn ManagedChildHandle>, String>>;
    fn track(
        &self,
        task_id: &str,
        handle: Arc<dyn ManagedChildHandle>,
        model: &str,
        epoch: i64,
        turn: TaskResumedTurn,
    );
    /// A revive transition for an idle resident child joining a pause.
    fn revive(&self, record: &TaskRecord, timestamp: String) -> TaskRecord;
    /// `concurrency.releaseTask`: an ended paused child gives back every lane slot it still holds.
    fn release_task_capacity(&self, _task_id: &str) {}
}

fn now_iso(now: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The pause intents a child's own abort can settle immediately, so registration precedes the call.
#[derive(Default)]
struct PendingPauses {
    waiting: WaitingState,
    settlements: SettlementsState,
}

impl PendingPauses {
    fn begin(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lock(&self.waiting)
            .entry(task_id.to_string())
            .or_insert_with(|| (epoch, Vec::new()))
            .1
            .push(sender);
        receiver
    }

    fn acknowledge(&self, task_id: &str, epoch: i64) {
        if let Some((held_epoch, waiters)) = lock(&self.waiting).remove(task_id)
            && held_epoch == epoch
        {
            for waiter in waiters {
                let _ = waiter.send(());
            }
        }
    }

    fn clear(&self, task_id: &str) {
        lock(&self.waiting).remove(task_id);
        lock(&self.settlements).retain(|(id, _), _| id != task_id);
    }

    fn settle(&self, task_id: &str, epoch: i64) {
        for waiter in lock(&self.settlements)
            .remove(&(task_id.to_string(), epoch))
            .unwrap_or_default()
        {
            let _ = waiter.send(());
        }
    }

    fn settlement(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lock(&self.settlements)
            .entry((task_id.to_string(), epoch))
            .or_default()
            .push(sender);
        receiver
    }
}

/// A parked child gives its concurrency slot back until its Resume re-acquires one.
#[derive(Default)]
struct ResumeCapacity {
    parked: Mutex<BTreeMap<String, i64>>,
}

impl ResumeCapacity {
    fn park(&self, task_id: &str, epoch: i64) {
        lock(&self.parked).insert(task_id.to_string(), epoch);
    }

    /// A parked child needs no slot: its Resume re-acquires one before it starts a turn.
    fn acquire(&self, task_id: &str, epoch: i64) -> bool {
        let mut parked = lock(&self.parked);
        if parked.get(task_id) == Some(&epoch) {
            parked.remove(task_id);
        }
        true
    }

    fn forget(&self, task_id: &str) {
        lock(&self.parked).remove(task_id);
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The run-pause surface the task manager and the engine's pause controller drive.
pub struct TaskRunPause {
    ports: Arc<dyn TaskRunPausePorts>,
    intents: PendingPauses,
    capacity: ResumeCapacity,
    resumed: Mutex<BTreeMap<String, (String, ResumeTaskResult)>>,
    attempts: Mutex<BTreeMap<String, String>>,
    owners: Mutex<BTreeMap<String, RunPauseOwner>>,
}

impl TaskRunPause {
    pub fn new(ports: Arc<dyn TaskRunPausePorts>) -> Self {
        Self {
            ports,
            intents: PendingPauses::default(),
            capacity: ResumeCapacity::default(),
            resumed: Mutex::new(BTreeMap::new()),
            attempts: Mutex::new(BTreeMap::new()),
            owners: Mutex::new(BTreeMap::new()),
        }
    }

    fn child_port(&self) -> Result<Arc<dyn TaskChildPausePort>, String> {
        self.ports
            .child()
            .ok_or_else(|| "child pause port is not installed".to_string())
    }

    fn load(&self, task_id: &str) -> Result<TaskRecord, String> {
        match self.ports.store().load(task_id) {
            Ok(Some(record)) => Ok(record),
            Ok(None) => Err(format!("paused child {task_id} has no record")),
            Err(error) => Err(error.to_string()),
        }
    }

    fn remember(&self, task_id: &str, owner: &RunPauseOwner) -> Result<(), String> {
        lock(&self.owners).insert(task_id.to_string(), owner.clone());
        self.ports
            .remember_owner(task_id, owner)
            .map_err(|error| error.to_string())
    }

    /// `pauseTask`: hold the child under `owner`, or record the pause for a child whose host is gone.
    pub fn pause_task(
        &self,
        task_id: &str,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        let handle = self.ports.live_handle(task_id);
        let epoch = record.notification.run_epoch;
        if !matches!(record.status, TaskStatus::Running | TaskStatus::Paused) {
            return Err(format!("cannot pause {} task", record.status.as_str()));
        }
        let Some(handle) = handle else {
            // Its host is gone, so nothing runs: record the pause and the run's sequence; the restore
            // that precedes Resume re-holds the child and reads the cutoff from the child's answer.
            if record.status == TaskStatus::Running
                && !self.ports.apply_pause(task_id, &now_iso(self.ports.now()))
            {
                return Err(format!("child {task_id} changed while pausing"));
            }
            self.remember(task_id, owner)?;
            return Ok(Some(EngineInheritReceipt { child_seq: 0 }));
        };
        let settled = if record.status == TaskStatus::Paused {
            None
        } else {
            Some(self.intents.begin(task_id, epoch))
        };
        lock(&self.resumed).remove(task_id);
        // Registration above MUST precede the child call: its abort can settle immediately.
        self.ports.begin_native_pause(task_id, epoch);
        let ack = match port.pause_for_run(&handle, owner) {
            Ok(ack) => ack,
            Err(error) => {
                // The request may have failed after the child's engine took the hold (an RPC
                // timeout): the engine's own hold view, not the failed call, decides.
                match port.observe_hold(&handle, &owner.parent_session_id) {
                    Some(observed) if observed.held => Some(EngineInheritReceipt {
                        child_seq: observed.child_seq,
                    }),
                    _ => {
                        if settled.is_some() {
                            self.intents.clear(task_id);
                        }
                        self.ports.finish_native_pause(task_id, epoch, false)?;
                        return Err(error);
                    }
                }
            }
        };
        let Some(ack) = ack else {
            self.ports.finish_native_pause(task_id, epoch, false)?;
            self.intents.clear(task_id);
            return Ok(None);
        };
        self.remember(task_id, owner)?;
        self.ports.finish_native_pause(task_id, epoch, true)?;
        // A pause that landed while a Resume was in flight cancels that attempt: the child stays held.
        if lock(&self.attempts).remove(task_id).is_some() && record.status == TaskStatus::Paused {
            self.park(task_id, epoch);
        }
        self.intents.acknowledge(task_id, epoch);
        Ok(Some(ack))
    }

    /// `resumePausedTask`: release the hold and open the child's next turn.
    pub fn resume_paused_task(
        &self,
        task_id: &str,
        request: &ResumeTaskRequest,
    ) -> Result<ResumeTaskResult, String> {
        if let Some((resume_id, result)) = lock(&self.resumed).get(task_id)
            && resume_id == &request.resume_id
        {
            return Ok(result.clone());
        }
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        if record.status != TaskStatus::Paused {
            return Err(format!("cannot resume {} task", record.status.as_str()));
        }
        let mut handle = self.ports.live_handle(task_id);
        let entered = self.ports.barrier().map(|barrier| {
            barrier.enter(
                &record.root_session_id.clone(),
                crate::pause::admission_barrier::AdmissionParticipant {
                    key: task_id.to_string(),
                    run_ref: request.owner.run_ref.clone(),
                },
                request.resume_credential.clone(),
            )
        });
        let ticket = match entered {
            Some(AdmissionEntry::Held { .. }) => return Err(RunPauseAdmissionHeld.to_string()),
            Some(AdmissionEntry::Entered { ticket }) => Some(ticket),
            None => None,
        };
        struct ResumeTicket(Option<crate::pause::admission_barrier::AdmissionTicket>);
        impl Drop for ResumeTicket {
            fn drop(&mut self) {
                if let Some(ticket) = &self.0 {
                    ticket.leave();
                }
            }
        }
        let _ticket_guard = ResumeTicket(ticket.clone());
        lock(&self.attempts).insert(task_id.to_string(), request.resume_id.clone());
        let mut cutoff = request.child_seq_cutoff;
        if handle.is_none() {
            let restored = self.ports.restore(&record).ok_or_else(|| {
                format!("paused child {task_id} has no live handle and no restore port")
            })??;
            let ack = port
                .pause_for_run(&restored, &request.owner)?
                .ok_or_else(|| format!("restored child {task_id} is held by another parent"))?;
            lock(&self.owners).insert(task_id.to_string(), request.owner.clone());
            cutoff = ack.child_seq;
            handle = Some(restored);
        }
        let Some(handle) = handle else {
            return Err(format!("paused child {task_id} is not attached"));
        };
        if let Some(ticket) = &ticket
            && ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
        {
            self.park(task_id, record.notification.run_epoch);
            return Err(RunPauseAdmissionHeld.to_string());
        }
        if !self
            .capacity
            .acquire(task_id, record.notification.run_epoch)
        {
            return Err(RunPauseAdmissionHeld.to_string());
        }
        self.ports.acquire_resume_capacity(
            task_id,
            record.notification.run_epoch,
            request.allocation.as_ref(),
        )?;
        let assert_current = || -> Result<(), String> {
            let current = self.load(task_id)?;
            if lock(&self.attempts).get(task_id) != Some(&request.resume_id)
                || ticket.as_ref().is_some_and(|ticket| {
                    ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
                })
                || current.status != TaskStatus::Paused
                || current.notification.run_epoch != record.notification.run_epoch
                || !self
                    .ports
                    .live_handle(task_id)
                    .is_some_and(|live| Arc::ptr_eq(&live, &handle))
            {
                self.park(task_id, record.notification.run_epoch);
                return Err(RunPauseAdmissionHeld.to_string());
            }
            Ok(())
        };
        assert_current()?;
        let result = port.resume_turn(
            &handle,
            &ResumeTurnRequest {
                owner: request.owner.clone(),
                resume_id: request.resume_id.clone(),
                child_seq_cutoff: cutoff,
            },
        )?;
        if let Some(ticket) = &ticket
            && ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
        {
            self.park(task_id, record.notification.run_epoch);
            return Err(RunPauseAdmissionHeld.to_string());
        }
        assert_current()?;
        let outcome = match result {
            ResumeTurnResult::OtherHoldLive { child_seq } => {
                self.park(task_id, record.notification.run_epoch);
                ResumeTaskResult::OtherHoldLive { child_seq }
            }
            ResumeTurnResult::Resumed { turn_id, outcome } => {
                self.intents.clear(task_id);
                if !self
                    .ports
                    .apply_unpause(task_id, &now_iso(self.ports.now()))
                {
                    return Err("paused child changed while resuming".to_string());
                }
                lock(&self.owners).remove(task_id);
                let _ = self.ports.forget_owner(task_id);
                // The returned turn belongs to `turn_id` even when already settled; never wait here.
                self.ports.track(
                    task_id,
                    handle,
                    &record.model,
                    record.notification.run_epoch,
                    TaskResumedTurn {
                        turn_id: turn_id.clone(),
                        outcome,
                    },
                );
                ResumeTaskResult::Resumed { turn_id }
            }
        };
        lock(&self.resumed).insert(
            task_id.to_string(),
            (request.resume_id.clone(), outcome.clone()),
        );
        if let Some(ticket) = &ticket {
            ticket.leave();
        }
        Ok(outcome)
    }

    /// `observeTaskHold`: what the child's engine reports about this parent's hold.
    pub fn observe_task_hold(
        &self,
        task_id: &str,
        parent_session_id: &str,
    ) -> Option<ObservedHold> {
        let handle = self.ports.live_handle(task_id)?;
        self.ports.child()?.observe_hold(&handle, parent_session_id)
    }

    /// `resumeLostTask`: release a child the run recorded as lost but whose engine still holds it.
    pub fn resume_lost_task(
        &self,
        task_id: &str,
        request: &LostResumeRequest,
    ) -> Result<LostResumeResult, String> {
        let record = self.load(task_id)?;
        if !matches!(record.status, TaskStatus::Running | TaskStatus::Paused) {
            return Ok(LostResumeResult::NotHeld);
        }
        let cutoff = match request.child_seq_cutoff {
            Some(cutoff) => cutoff,
            None => match self.observe_task_hold(task_id, &request.owner.parent_session_id) {
                Some(observed) if observed.held => observed.child_seq,
                _ => return Ok(LostResumeResult::NotHeld),
            },
        };
        if record.status == TaskStatus::Paused {
            return Ok(
                match self.resume_paused_task(
                    task_id,
                    &ResumeTaskRequest {
                        allocation: None,
                        owner: request.owner.clone(),
                        resume_credential: request.resume_credential.clone(),
                        resume_id: request.resume_id.clone(),
                        child_seq_cutoff: cutoff,
                    },
                )? {
                    ResumeTaskResult::Resumed { turn_id } => LostResumeResult::Resumed { turn_id },
                    ResumeTaskResult::OtherHoldLive { child_seq } => {
                        LostResumeResult::OtherHoldLive { child_seq }
                    }
                },
            );
        }
        let Some(handle) = self.ports.live_handle(task_id) else {
            return Ok(LostResumeResult::NotHeld);
        };
        match self.child_port()?.resume_turn(
            &handle,
            &ResumeTurnRequest {
                owner: request.owner.clone(),
                resume_id: request.resume_id.clone(),
                child_seq_cutoff: cutoff,
            },
        )? {
            ResumeTurnResult::OtherHoldLive { child_seq } => {
                Ok(LostResumeResult::OtherHoldLive { child_seq })
            }
            ResumeTurnResult::Resumed { turn_id, outcome } => {
                self.ports.track(
                    task_id,
                    handle,
                    &record.model,
                    record.notification.run_epoch,
                    TaskResumedTurn {
                        turn_id: turn_id.clone(),
                        outcome,
                    },
                );
                Ok(LostResumeResult::Resumed { turn_id })
            }
        }
    }

    /// `holdTaskInput`: guidance names the parent that paused this child; another parent is refused.
    pub fn hold_task_input(
        &self,
        task_id: &str,
        input: &HeldTaskInput,
    ) -> Result<EngineHoldInputResult, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        if record.status != TaskStatus::Paused {
            return Err(format!(
                "cannot hold input for {} task",
                record.status.as_str()
            ));
        }
        let owner = lock(&self.owners)
            .get(task_id)
            .cloned()
            .or_else(|| saved_run_owner(&record))
            .ok_or_else(|| format!("paused child {task_id} has no recorded run owner"))?;
        let handle = match self.ports.live_handle(task_id) {
            Some(handle) => handle,
            None => self.ports.restore(&record).ok_or_else(|| {
                format!("paused child {task_id} has no live handle and no restore port")
            })??,
        };
        port.hold_input(&handle, input, &owner)
    }

    /// `joinIdlePause`: an idle resident child opens its next run paused under the run's owner.
    pub fn join_idle_pause(
        &self,
        task_id: &str,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        let handle = self
            .ports
            .live_handle(task_id)
            .ok_or_else(|| format!("paused child {task_id} is not attached"))?;
        if !matches!(
            record.status,
            TaskStatus::Completed | TaskStatus::Error | TaskStatus::Interrupted
        ) {
            return Err(format!(
                "cannot join an idle pause from {}",
                record.status.as_str()
            ));
        }
        let ack = port.pause_for_run(&handle, owner)?;
        if ack.is_none() {
            return Ok(None);
        }
        let timestamp = now_iso(self.ports.now());
        let mut opened = false;
        self.ports
            .store()
            .mutate(task_id, &mut |fresh: &TaskRecord| {
                if fresh.status != record.status
                    || fresh.notification.run_epoch != record.notification.run_epoch
                {
                    return fresh.clone();
                }
                opened = true;
                self.ports.revive(fresh, timestamp.clone())
            })
            .map_err(|error| error.to_string())?;
        if !opened {
            return Err(format!(
                "idle child {task_id} changed while joining the pause"
            ));
        }
        if !self.ports.apply_pause(task_id, &timestamp) {
            return Err(format!(
                "idle child {task_id} changed while joining the pause"
            ));
        }
        self.remember(task_id, owner)?;
        Ok(ack)
    }

    /// `endLostPausedTask`: a paused child the run's Resume could not reach, with no live handle
    /// left, ends as an error the parent is told about; its transcript stays readable through
    /// task_output (#12249). The status check, the caller's ownership check and the end are one
    /// locked read-modify-write, so a host that takes the record over in between is never ended here.
    pub fn end_lost_paused_task(
        &self,
        task_id: &str,
        reason: &str,
        may_end: impl Fn(&TaskRecord) -> bool,
    ) -> bool {
        if self.ports.live_handle(task_id).is_some() {
            return false;
        }
        let timestamp = now_iso(self.ports.now());
        let mut ended: Option<crate::state::TaskTransitionResult> = None;
        let written = self.ports.store().mutate(task_id, |fresh: &TaskRecord| {
            if fresh.status != TaskStatus::Paused || !may_end(fresh) {
                return fresh.clone();
            }
            // The Rust `Fail` transition carries no failure kind, so upstream's narrow
            // `paused -> fail(session_unavailable)` edge is taken here: the store already encodes a
            // paused child as a running one, and only this end gives it `session_unavailable`.
            let mut unpaused = fresh.clone();
            unpaused.run_pause = None;
            unpaused.status = TaskStatus::Running;
            let result = crate::state::transition_task_record(
                &unpaused,
                &crate::state::TaskTransition::Fail {
                    timestamp: timestamp.clone(),
                    error_message: format!(
                        "The run's Resume could not restore this paused child ({reason}), so its run ended. Its transcript is kept for task_output."
                    ),
                    killed: false,
                    run_stats: None,
                },
            );
            if !result.applied {
                return fresh.clone();
            }
            let mut next = result.record.clone();
            next.failure_kind = Some("session_unavailable".to_string());
            ended = Some(result);
            next
        });
        let Some(ended) = ended.filter(|_| written.is_ok()) else {
            return false;
        };
        let _ = self.ports.store().append_event(
            task_id,
            &crate::store::PersistedTaskEvent {
                event_type: ended.audit.type_name().to_string(),
                payload: serde_json::to_value(&ended.audit).unwrap_or(serde_json::Value::Null),
            },
        );
        self.forget(task_id);
        self.ports.release_task_capacity(task_id);
        true
    }

    /// `park`: a child that stays held gives its slot back and settles its pause intent.
    pub fn park(&self, task_id: &str, epoch: i64) {
        self.capacity.park(task_id, epoch);
        self.ports.park_capacity(task_id, epoch);
        self.intents.settle(task_id, epoch);
    }

    pub fn forget(&self, task_id: &str) {
        lock(&self.owners).remove(task_id);
        self.intents.clear(task_id);
        lock(&self.attempts).remove(task_id);
        self.capacity.forget(task_id);
        lock(&self.resumed).remove(task_id);
    }

    pub fn settlement(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        self.intents.settlement(task_id, epoch)
    }
}

/// The owner a restarted host saved on the record (`savedRunOwner`).
pub fn saved_run_owner(record: &TaskRecord) -> Option<RunPauseOwner> {
    let RecordedRunPause {
        parent_session_id,
        parent_pause_seq,
        run_ref,
    } = record.run_pause.clone()?;
    Some(RunPauseOwner {
        parent_session_id,
        parent_pause_seq,
        run_ref: crate::pause::ports::RunRef::new(
            match run_ref.kind {
                RunPauseKind::Dag => crate::pause::ports::RunRefKind::Dag,
                RunPauseKind::Task => crate::pause::ports::RunRefKind::Task,
                RunPauseKind::Workpool => crate::pause::ports::RunRefKind::Workpool,
                RunPauseKind::Team => crate::pause::ports::RunRefKind::Team,
            },
            run_ref.id,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumed_turn_preserves_the_exact_deferred_outcome() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let returned = ResumeTurnResult::Resumed {
            turn_id: "turn-2".into(),
            outcome: Box::new(move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::runners::RunnerOutcome::completed("exact turn")
            }),
        };
        let ResumeTurnResult::Resumed { turn_id, outcome } = returned else {
            panic!("resumed turn")
        };
        let tracked = TaskResumedTurn { turn_id, outcome };
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(tracked.turn_id, "turn-2");
        assert_eq!(
            (tracked.outcome)(),
            crate::runners::RunnerOutcome::completed("exact turn")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_parked_child_keeps_no_slot_after_its_resume() {
        let capacity = ResumeCapacity::default();
        capacity.park("t1", 3);
        assert!(capacity.acquire("t1", 3));
        // A second acquire is idempotent: nothing is parked any more.
        assert!(capacity.acquire("t1", 3));
    }

    #[test]
    fn a_pause_intent_settles_when_the_child_acknowledges() {
        let intents = PendingPauses::default();
        let mut receiver = intents.begin("t1", 7);
        intents.acknowledge("t1", 7);
        assert!(receiver.try_recv().is_ok());
    }

    /// `run-pause-end-lost.test.ts`: a paused child with a held lane slot, and an optional live handle.
    struct EndLostPorts {
        store: TaskRecordStore,
        concurrency: Mutex<crate::manager::concurrency::TaskConcurrency>,
        live: bool,
    }

    struct IdleHandle;
    impl ManagedChildHandle for IdleHandle {
        fn task_id(&self) -> &str {
            "st_0e1d0001"
        }
        fn session_id(&self) -> Option<String> {
            None
        }
        fn pid(&self) -> Option<i64> {
            None
        }
        fn steer(&self, _: &str) -> Result<(), crate::host::HostError> {
            Ok(())
        }
        fn follow_up(&self, _: &str) -> Result<(), crate::host::HostError> {
            Ok(())
        }
        fn abort(&self) -> Result<(), crate::host::HostError> {
            Ok(())
        }
        fn subscribe(
            &self,
            _: crate::manager::ManagedChildListener,
        ) -> crate::manager::Unsubscribe {
            Box::new(|| {})
        }
        fn wait_for_outcome(&self) -> crate::runners::RunnerOutcome {
            crate::runners::RunnerOutcome::Cancelled
        }
        fn last_assistant_text(&self) -> Option<String> {
            None
        }
        fn dispose(&self) -> Result<(), crate::host::HostError> {
            Ok(())
        }
    }

    impl TaskRunPausePorts for EndLostPorts {
        fn store(&self) -> &TaskRecordStore {
            &self.store
        }
        fn now(&self) -> i64 {
            chrono::DateTime::parse_from_rfc3339("2026-11-03T04:28:14.000Z")
                .expect("fixed clock")
                .timestamp_millis()
        }
        fn live_handle(&self, _: &str) -> Option<Arc<dyn ManagedChildHandle>> {
            self.live
                .then(|| Arc::new(IdleHandle) as Arc<dyn ManagedChildHandle>)
        }
        fn child(&self) -> Option<Arc<dyn TaskChildPausePort>> {
            None
        }
        fn barrier(&self) -> Option<AdmissionBarrier> {
            None
        }
        fn remember_owner(&self, _: &str, _: &RunPauseOwner) -> Result<(), StoreError> {
            Ok(())
        }
        fn forget_owner(&self, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
        fn apply_pause(&self, _: &str, _: &str) -> bool {
            false
        }
        fn apply_unpause(&self, _: &str, _: &str) -> bool {
            false
        }
        fn restore(&self, _: &TaskRecord) -> Option<Result<Arc<dyn ManagedChildHandle>, String>> {
            None
        }
        fn track(
            &self,
            _: &str,
            _: Arc<dyn ManagedChildHandle>,
            _: &str,
            _: i64,
            _: TaskResumedTurn,
        ) {
        }
        fn revive(&self, record: &TaskRecord, _: String) -> TaskRecord {
            record.clone()
        }
        fn release_task_capacity(&self, task_id: &str) {
            let _ = lock(&self.concurrency).release_task(task_id);
        }
    }

    const PAUSED_TASK: &str = "st_0e1d0001";

    fn paused_child(live: bool) -> (tempfile::TempDir, Arc<EndLostPorts>, TaskRunPause) {
        let root = tempfile::tempdir().expect("project");
        let store = TaskRecordStore::new(&crate::store::StateDirConfig {
            project_dir: root.path().into(),
            task_state_dir: None,
        });
        let mut record = crate::test_support::base_record(PAUSED_TASK, "parent-1");
        record.status = TaskStatus::Paused;
        record.final_response = None;
        record.model = "test/model".into();
        record.run_pause = Some(RecordedRunPause {
            parent_session_id: "parent-1".into(),
            parent_pause_seq: 16,
            run_ref: crate::state::RunPauseRef {
                kind: RunPauseKind::Task,
                id: PAUSED_TASK.into(),
            },
        });
        store.save(&record).expect("seed");
        let mut concurrency = crate::manager::concurrency::TaskConcurrency::new(
            crate::manager::concurrency::TaskConcurrencyConfig {
                provider_concurrency: Some(BTreeMap::from([("test".to_string(), 1)])),
                ..Default::default()
            },
        );
        assert!(concurrency.try_acquire("test/model", PAUSED_TASK, 0));
        let ports = Arc::new(EndLostPorts {
            store,
            concurrency: Mutex::new(concurrency),
            live,
        });
        let pause = TaskRunPause::new(ports.clone());
        (root, ports, pause)
    }

    #[test]
    fn an_unrestorable_paused_child_ends_as_session_unavailable_with_its_pause_cleared_and_slot_freed()
     {
        let (_root, ports, pause) = paused_child(false);

        let ended = pause.end_lost_paused_task(
            PAUSED_TASK,
            "paused child restore failed (respawn_failed): rpc respawn failed",
            |_| true,
        );

        assert!(ended);
        let record = ports
            .store
            .load(PAUSED_TASK)
            .expect("load")
            .expect("record");
        assert_eq!(record.status, TaskStatus::Error);
        assert_eq!(record.failure_kind.as_deref(), Some("session_unavailable"));
        assert!(
            record
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("respawn_failed")),
            "{:?}",
            record.error_message
        );
        assert_eq!(record.run_pause, None);
        let mut concurrency = lock(&ports.concurrency);
        assert_eq!(concurrency.lease_state(PAUSED_TASK, 0), None);
        assert!(concurrency.try_acquire("test/model", "st_0e1d0002", 0));
    }

    #[test]
    fn a_paused_child_another_host_took_over_stays_paused_under_the_fresh_ownership_check() {
        let (_root, ports, pause) = paused_child(false);
        ports
            .store
            .mutate(PAUSED_TASK, |fresh| {
                let mut next = fresh.clone();
                next.host_pid = Some(424_242);
                next
            })
            .expect("take over");

        assert!(
            !pause.end_lost_paused_task(PAUSED_TASK, "respawn_failed", |fresh| {
                fresh.host_pid != Some(424_242)
            })
        );

        let record = ports
            .store
            .load(PAUSED_TASK)
            .expect("load")
            .expect("record");
        assert_eq!(record.status, TaskStatus::Paused);
        assert_eq!(
            record.run_pause.map(|owner| owner.parent_pause_seq),
            Some(16)
        );
        assert_eq!(
            lock(&ports.concurrency).lease_state(PAUSED_TASK, 0),
            Some("held")
        );
    }

    #[test]
    fn a_paused_child_whose_handle_is_still_live_stays_paused_for_the_next_resume() {
        let (_root, ports, pause) = paused_child(true);

        assert!(!pause.end_lost_paused_task(PAUSED_TASK, "resume_for_run timed out", |_| true));

        let record = ports
            .store
            .load(PAUSED_TASK)
            .expect("load")
            .expect("record");
        assert_eq!(record.status, TaskStatus::Paused);
        assert_eq!(
            lock(&ports.concurrency).lease_state(PAUSED_TASK, 0),
            Some("held")
        );
    }

    struct RecordingParent(Arc<Mutex<Vec<crate::completion::ParentNotifierMessage>>>);
    impl crate::completion::ParentNotifier for RecordingParent {
        fn enqueue_with_callbacks(
            &self,
            message: &crate::completion::ParentNotifierMessage,
            callbacks: crate::completion::DeliveryCallbacks,
        ) -> Result<(), crate::host::HostError> {
            lock(&self.0).push(message.clone());
            callbacks.delivered();
            Ok(())
        }
    }

    /// `completion-bridge.test.ts` (#12249): in mhc the parent hears about a settled child through
    /// the completion waiter the task component holds for it, which hands the record to the
    /// completion notifier. Ending the run its Resume could not restore settles that waiter with
    /// the error, and the parent is told exactly once.
    #[test]
    fn an_ended_lost_paused_run_settles_its_waiter_and_tells_the_parent_once_that_it_failed() {
        use std::future::Future;
        let harness = crate::manager::manager_tests::fakes::make_manager(Default::default());
        let mut record = crate::test_support::base_record(PAUSED_TASK, "parent-session");
        record.status = TaskStatus::Paused;
        record.final_response = None;
        record.notify_on_terminal = true;
        harness.store.save(&record).expect("seed");
        let pause = TaskRunPause::new(Arc::new(EndLostPorts {
            store: harness.store.clone(),
            concurrency: Mutex::new(crate::manager::concurrency::TaskConcurrency::new(
                Default::default(),
            )),
            live: false,
        }));
        let (_cancel, mut cancelled) = tokio::sync::watch::channel(false);
        let mut waiting =
            std::pin::pin!(harness.manager.wait_for_async(PAUSED_TASK, &mut cancelled));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        // The first poll registers the waiter and finds the run still paused.
        assert!(waiting.as_mut().poll(&mut context).is_pending());

        assert!(harness.manager.end_lost_paused_task(
            &pause,
            PAUSED_TASK,
            "paused child restore failed (respawn_failed)"
        ));

        let std::task::Poll::Ready(settled) = waiting.as_mut().poll(&mut context) else {
            panic!("ending the run never settled its waiter");
        };
        let ended = settled.expect("the ended record");
        assert_eq!(ended.status, TaskStatus::Error);
        let messages = Arc::new(Mutex::new(Vec::new()));
        let notifier = crate::completion::create_completion_notifier(
            crate::completion::CompletionNotifierDeps::new(
                Arc::new(RecordingParent(messages.clone())),
                Arc::new(harness.store.clone()),
            ),
        );
        for _ in 0..2 {
            notifier
                .notify_terminal(&crate::completion::CompletionRequest {
                    record: ended.clone(),
                    parent_state: crate::completion::ParentState::Idle,
                    run_in_background: true,
                    tokens: None,
                })
                .expect("notify");
        }
        let messages = lock(&messages);
        assert_eq!(messages.len(), 1, "told once");
        assert_eq!(messages[0].details[0].task_id, PAUSED_TASK);
        assert_eq!(messages[0].details[0].status, TaskStatus::Error);
    }

    #[test]
    fn a_park_settles_the_pause_intent() {
        let intents = PendingPauses::default();
        let mut receiver = intents.settlement("t1", 2);
        intents.settle("t1", 2);
        assert!(receiver.try_recv().is_ok());
    }
}
