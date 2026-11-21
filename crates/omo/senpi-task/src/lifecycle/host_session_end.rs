//! Port of `lifecycle/host-session-end.ts` (omo#11765, omo#11783): THE one way the lifecycle decides
//! whether a daemon-hosted child it holds no handle for has ended.
//!
//! "Not listed on the recorded socket" is not an end: after a generation handoff the successor serves
//! that socket while the draining predecessor may still run the child. An end is confirmed only by a
//! close the host answered, or - with a generation view - by the three-part proof that the generation
//! the record names is gone: a serving listing without the path, no handover-parked entry, and a
//! second listing taken after that read (the successor's reopen watch opens a parked session before
//! it clears the entry).
//!
//! Single flight per task: every entrant (a cancel, its retry driver, a pending-cancel finish, the
//! destruction port) gets the in-flight pass, and a confirmed end stays until the destruction that
//! consumes it clears it. A pass belongs to one run of the task: no later run joins or doubles an
//! earlier run's check, and an earlier run's end never answers for a later one.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use super::context::LifecycleContext;
use super::destroy::spawn_spec_cwd;
use super::host_session::{
    HostSessionEnd, HostSessionEndVia, HostSessionLiveness, host_session_identity,
};
use super::host_session_close::{HostSessionCloseOutcome, close_host_session};
use crate::state::{HostSessionIdentity, TaskRecord};
use crate::store::PersistedTaskEvent;

/// The run and session a pass judged: run epoch, socket, instance id, session path, routing id.
type FlightOwner = (i64, String, String, String, String);

struct Flight {
    owner: FlightOwner,
    outcome: Mutex<Option<HostSessionEnd>>,
    settled: Condvar,
}

impl Flight {
    fn new(owner: FlightOwner) -> Self {
        Self {
            owner,
            outcome: Mutex::new(None),
            settled: Condvar::new(),
        }
    }

    fn done(&self) -> bool {
        lock(&self.outcome).is_some_and(HostSessionEnd::is_ended)
    }

    fn wait(&self) -> HostSessionEnd {
        let mut outcome = lock(&self.outcome);
        loop {
            if let Some(end) = *outcome {
                return end;
            }
            outcome = self
                .settled
                .wait(outcome)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn settle(&self, end: HostSessionEnd) {
        *lock(&self.outcome) = Some(end);
        self.settled.notify_all();
    }
}

/// The per-lifecycle flight table (TS: a `WeakMap` keyed by the lifecycle's store).
#[derive(Default)]
pub struct HostSessionEnds {
    flights: Mutex<HashMap<String, Arc<Flight>>>,
}

enum Entry {
    Join(Arc<Flight>),
    EarlierRunInFlight,
    Run(Arc<Flight>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn end_host_session_child(context: &LifecycleContext, record: &TaskRecord) -> HostSessionEnd {
    let Some(host_session) = host_session_identity(record) else {
        return HostSessionEnd::NotEnded("not_host_session");
    };
    let owner = owner_of(record, host_session);
    let entry = {
        let mut flights = lock(&context.host_session_ends.flights);
        let existing = flights.get(&record.task_id).cloned();
        match existing {
            Some(existing) if existing.owner == owner => Entry::Join(existing),
            Some(existing) if !existing.done() => Entry::EarlierRunInFlight,
            _ => {
                let created = Arc::new(Flight::new(owner));
                flights.insert(record.task_id.clone(), Arc::clone(&created));
                Entry::Run(created)
            }
        }
    };
    let flight = match entry {
        Entry::Join(existing) => return existing.wait(),
        Entry::EarlierRunInFlight => return HostSessionEnd::NotEnded("earlier_run_in_flight"),
        Entry::Run(created) => created,
    };
    let end = run_pass(context, record, host_session);
    flight.settle(end);
    if !end.is_ended() {
        let mut flights = lock(&context.host_session_ends.flights);
        if flights
            .get(&record.task_id)
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            flights.remove(&record.task_id);
        }
    }
    end
}

/// Drops a confirmed end once the destruction that consumed it ran; a pass still running stays.
pub fn clear_host_session_end(context: &LifecycleContext, task_id: &str) {
    let mut flights = lock(&context.host_session_ends.flights);
    if flights.get(task_id).is_some_and(|flight| flight.done()) {
        flights.remove(task_id);
    }
}

fn owner_of(record: &TaskRecord, host_session: &HostSessionIdentity) -> FlightOwner {
    (
        record.notification.run_epoch,
        host_session.socket.clone(),
        host_session.instance_id.clone(),
        host_session.session_path.clone(),
        host_session.routing_id.clone(),
    )
}

fn run_pass(
    context: &LifecycleContext,
    record: &TaskRecord,
    host_session: &HostSessionIdentity,
) -> HostSessionEnd {
    let close = || {
        let cwd = spawn_spec_cwd(record);
        match close_host_session(context, &record.task_id, host_session, cwd.as_deref()) {
            HostSessionCloseOutcome::Closed => {
                HostSessionEnd::Ended(HostSessionEndVia::CloseSession)
            }
            HostSessionCloseOutcome::Refused => HostSessionEnd::NotEnded("close_refused"),
        }
    };
    match listing(context, host_session) {
        HostSessionLiveness::Live => return close(),
        HostSessionLiveness::Unknown => return HostSessionEnd::NotEnded("host_unknown"),
        HostSessionLiveness::Gone => {}
    }
    let Some(view) = context.host_generation_view.as_ref() else {
        return HostSessionEnd::Ended(HostSessionEndVia::NotListed);
    };
    if view.parked_entry_present(host_session) {
        return close();
    }
    if view.generation_present(host_session) {
        return HostSessionEnd::NotEnded("generation_cannot_cancel");
    }
    match listing(context, host_session) {
        HostSessionLiveness::Live => close(),
        HostSessionLiveness::Unknown => HostSessionEnd::NotEnded("host_unknown"),
        HostSessionLiveness::Gone => {
            record_generation_exited(context, &record.task_id, host_session);
            HostSessionEnd::Ended(HostSessionEndVia::GenerationExited)
        }
    }
}

fn listing(context: &LifecycleContext, host_session: &HostSessionIdentity) -> HostSessionLiveness {
    context
        .host_session_probe
        .refresh(Some(&host_session.socket));
    context.host_session_probe.session_liveness(host_session)
}

fn record_generation_exited(
    context: &LifecycleContext,
    task_id: &str,
    host_session: &HostSessionIdentity,
) {
    let event = PersistedTaskEvent {
        event_type: "host_session_closed".to_string(),
        payload: serde_json::json!({
            "session_path": host_session.session_path,
            "socket": host_session.socket,
            "via": HostSessionEndVia::GenerationExited.as_str(),
            "instance_id": host_session.instance_id,
        }),
    };
    if let Err(error) = context.store.append_event(task_id, &event) {
        utils::logger::log(
            "senpi-task could not record a proven host session end",
            Some(&serde_json::json!({ "taskId": task_id, "error": error.to_string() })),
        );
    }
}
