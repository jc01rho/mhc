//! The carrier of woken sessions in one engine process.
//!
//! Upstream counterpart: `thread/wake/carrier.ts` (omo#13037 A3.1.3-A3.1.8, A3.2). Each carried session has its own kept
//! connection, the session's only attachment from this process. The carrier lifts the opening gate and sends the
//! engine's `wake`; when the session is finished it hands it back with a quiet `release_session { reason: "takeover" }`
//! and never sends that release while background work is pending. A person's attached client takes the session over.
//! A wait for a person that arose after admission is closed after 30 minutes. A held session is not run, its state is
//! stored, and it is not woken again until its file changes.
//!
//! Work on one durable id runs serially. Every continuation upstream runs on the event loop (a lifecycle event's
//! evaluation, the renewal timer, the sweep at the carrier's start) goes through `Schedule`, so a connection's reader
//! never waits on the carrier and a test drives the timers by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use super::carrier_connection::{
    CarrierConnect, CarrierFrameListener, CarrierHostConnection, Unsubscribe,
};
use super::closed_intake::settle_closed_intake;
use super::engine_state::{EngineView, read_engine_state};
use super::memory_headroom::WAKE_MEMORY_FLOOR_BYTES;
use super::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};
use crate::thread::constants::WAKE_LEASE_MS;
use crate::thread::intake_admission::IntakeAdmission;
use crate::thread::session_wait_types::{SessionWaitKind, SessionWaitWrite, wait_blocks_delivery};
use crate::thread::store::SqliteGatewayStore;
use crate::thread::store_api::GatewayStoreApi;
use crate::thread::store_ops::StoreError;
use crate::thread::store_woken_ops::ledger_device;
use crate::thread::woken_types::{
    WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome, WokenSessionFence, WokenSessionState,
};

/// A person's wait that arose after admission is closed after this long with only the carrier attached (A3.2.6).
pub const PERSON_WAIT_CLOSE_MS: i64 = 30 * 60_000;
/// How often a carrier renews its leases and sweeps (A3.1.3).
pub const CARRIER_RENEW_MS: i64 = 20_000;
/// How long a finished-looking session refused for a queued delivery is kept for that delivery to run (omo#13708).
pub const QUEUED_DELIVERY_GRACE_MS: i64 = 2 * 60_000;
pub const CARRIER_TICK_MS: u64 = if CARRIER_RENEW_MS < WAKE_LEASE_MS / 3 {
    CARRIER_RENEW_MS as u64
} else {
    (WAKE_LEASE_MS / 3) as u64
};

const HELD_ELSEWHERE: [&str; 2] = ["session_held", "session_path_in_use"];
const DEFINITIVE: [&str; 2] = ["invalid_path", "unknown_session"];

pub type ScheduledRun = Box<dyn FnOnce() + Send>;
pub type CancelScheduled = Box<dyn FnOnce() + Send>;
/// Runs `run` once, `ms` from now, off the caller's stack; the answer cancels a run that has not started.
pub type Schedule = Arc<dyn Fn(ScheduledRun, u64) -> CancelScheduled + Send + Sync>;
pub type EnsureHost = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
pub type WakeLimit = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type CwdSwitch = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type Switch = Arc<dyn Fn() -> bool + Send + Sync>;
pub type AdmitIntake = Arc<dyn Fn(&str) -> IntakeAdmission + Send + Sync>;
pub type MemoryProbe = Arc<dyn Fn() -> Option<u64> + Send + Sync>;
pub type MemoryFloor = Arc<dyn Fn() -> u64 + Send + Sync>;
pub type CarrierEventListener = Arc<dyn Fn(&CarrierEvent) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierWakeOutcome {
    Open {
        session_id: String,
        generation: i64,
    },
    Held,
    Waiting,
    WakeLimitReached,
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    NotWakeable {
        session_kind: WakeSessionKind,
    },
    /// The host's measured memory headroom is below the floor; the row stays requested and the sweep retries it.
    HostMemoryPressure {
        available_bytes: u64,
        floor_bytes: u64,
    },
    NotYet {
        next_attempt_at: i64,
    },
    Busy {
        code: String,
    },
    Deferred {
        attempts: i64,
    },
    Failed {
        attempts: i64,
    },
    /// No queued row of this session may be opened under the switches as they read now (A7.2.3); the row waits.
    SwitchedOff,
    /// The session holds no queued row at all, so there is nothing to open for; the record is ended (A3.1.6).
    NothingQueued,
}

impl CarrierWakeOutcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Held => "held",
            Self::Waiting => "waiting",
            Self::WakeLimitReached => "wake_limit_reached",
            Self::Gone => "gone",
            Self::Ambiguous { .. } => "ambiguous",
            Self::NotWakeable { .. } => "not_wakeable",
            Self::HostMemoryPressure { .. } => "host_memory_pressure",
            Self::NotYet { .. } => "not_yet",
            Self::Busy { .. } => "busy",
            Self::Deferred { .. } => "deferred",
            Self::Failed { .. } => "failed",
            Self::SwitchedOff => "switched_off",
            Self::NothingQueued => "nothing_queued",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierEvaluation {
    Released,
    /// The session is still working (upstream `{ kind: "kept", reason: "working" }`).
    Kept,
    HandedOver,
    CloseFailed {
        code: String,
    },
    Waiting {
        since: i64,
    },
    ClosedWaiting,
    Held,
    LostLease,
    NotCarried,
}

impl CarrierEvaluation {
    /// A carry that ended: the session is no longer this carrier's, so its intake marks may now be drained.
    pub fn ends_carry(&self) -> bool {
        matches!(
            self,
            Self::Released | Self::HandedOver | Self::ClosedWaiting | Self::Held
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierEvent {
    pub durable_id: String,
    pub outcome: CarrierEvaluation,
}

#[derive(Debug, thiserror::Error)]
pub enum CarrierError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Host(String),
}

pub struct CarrierOptions {
    pub store: Arc<SqliteGatewayStore>,
    pub sessions_directory: PathBuf,
    pub ensure_host: EnsureHost,
    pub connect: CarrierConnect,
    /// `session_wake.wake_limit`; 0 means no bound (A3.5.3).
    pub wake_limit: Option<WakeLimit>,
    /// `session_wake.same_device`, read at the moment of the open for the TARGET session's cwd.
    pub same_device: Option<CwdSwitch>,
    /// The gate for a row from another device: production allows it while this process's delivery consumer serves.
    pub remote_wake: Option<Switch>,
    /// Whether a row's sender is still admitted (5.6), asked at every open of a row from another device; `unknown`
    /// and `refused` fail closed.
    pub admit_intake: Option<AdmitIntake>,
    /// The host's memory headroom in bytes; no reading never defers a wake.
    pub memory_headroom: Option<MemoryProbe>,
    /// The headroom a wake needs; `WAKE_MEMORY_FLOOR_BYTES` when absent.
    pub memory_floor: Option<MemoryFloor>,
    pub schedule: Schedule,
    pub on_event: Option<CarrierEventListener>,
}

struct Carried {
    entry_id: u64,
    durable_id: String,
    connection: Arc<dyn CarrierHostConnection>,
    session_id: String,
    generation: i64,
    off: Mutex<Option<Unsubscribe>>,
    /// When the release was first refused for a delivery since the session last worked or was refused otherwise.
    delivery_refused_since: Mutex<Option<i64>>,
}

#[derive(Default)]
struct CarrierTimer {
    armed: bool,
    cancel: Option<CancelScheduled>,
}

struct CarrierInner {
    options: CarrierOptions,
    carried: Mutex<HashMap<String, Arc<Carried>>>,
    chains: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    stopped: AtomicBool,
    timer: Mutex<CarrierTimer>,
    next_entry: AtomicU64,
}

struct OpenedSession {
    connection: Arc<dyn CarrierHostConnection>,
    session_id: String,
    attached: bool,
    view: EngineView,
}

enum OpenOutcome {
    Opened(OpenedSession),
    Refused { code: String },
    Unreachable,
}

enum ReleaseReply {
    Released,
    Refused { code: String, for_delivery: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allowance {
    Open,
    SwitchedOff,
    NothingQueued,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn succeeded(frame: &Value) -> bool {
    frame.get("success") == Some(&Value::Bool(true))
}

fn code_of(frame: &Value) -> String {
    if let Some(code) = frame.get("errorCode").and_then(Value::as_str) {
        return code.to_owned();
    }
    match frame.get("error").and_then(Value::as_str) {
        Some(error) => error.split(':').next().unwrap_or(error).trim().to_owned(),
        None => "unknown".to_owned(),
    }
}

fn data_of(frame: &Value) -> Value {
    frame
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn busy_list(frame: &Value) -> Vec<String> {
    frame
        .get("errorData")
        .and_then(|data| data.get("busy"))
        .and_then(Value::as_array)
        .map(|busy| busy.iter().map(text_of).collect())
        .unwrap_or_default()
}

fn number_value(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .unwrap_or(0.0)
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(-1)
}

fn busy_in_progress() -> CarrierWakeOutcome {
    CarrierWakeOutcome::Busy {
        code: "wake_in_progress".to_owned(),
    }
}

#[derive(Clone)]
pub struct Carrier {
    inner: Arc<CarrierInner>,
}

pub fn create_carrier(options: CarrierOptions) -> Carrier {
    let inner = Arc::new(CarrierInner {
        options,
        carried: Mutex::default(),
        chains: Mutex::default(),
        stopped: AtomicBool::new(false),
        timer: Mutex::default(),
        next_entry: AtomicU64::new(1),
    });
    // A3.1.4: every carrier sweeps when it starts, so rows a dead process left requested and leases that ran out are
    // taken over without a wake of its own; with an empty table that one read arms no timer.
    let weak = Arc::downgrade(&inner);
    let _ = (inner.options.schedule)(
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.start_sweep();
            }
        }),
        0,
    );
    Carrier { inner }
}

impl Carrier {
    pub fn wake(&self, durable_id: &str) -> Result<CarrierWakeOutcome, CarrierError> {
        self.inner.serial(durable_id, || {
            // A wake leaves a row the timer must tend (a retry, a carried session's lease).
            self.inner.arm();
            self.inner.wake_now(durable_id)
        })
    }

    pub fn evaluate(&self, durable_id: &str) -> Result<CarrierEvaluation, CarrierError> {
        self.inner.evaluate(durable_id)
    }

    /// Starts requested rows that are due and takes over rows whose lease ran out (A3.1.3).
    pub fn sweep(&self) -> Result<(), CarrierError> {
        self.inner.sweep_now().map(|_| ())
    }

    pub fn stop(&self) {
        self.inner.stop();
    }
}

impl CarrierInner {
    fn serial<T>(&self, durable_id: &str, work: impl FnOnce() -> T) -> T {
        let chain = lock(&self.chains)
            .entry(durable_id.to_owned())
            .or_default()
            .clone();
        let _turn = lock(&chain);
        work()
    }

    fn carried_entry(&self, durable_id: &str) -> Option<Arc<Carried>> {
        lock(&self.carried).get(durable_id).cloned()
    }

    fn emit(&self, durable_id: &str, outcome: &CarrierEvaluation) {
        if let Some(listener) = &self.options.on_event {
            listener(&CarrierEvent {
                durable_id: durable_id.to_owned(),
                outcome: outcome.clone(),
            });
        }
    }

    fn forget(&self, entry: &Carried) {
        let off = lock(&entry.off).take();
        if let Some(off) = off {
            off();
        }
        lock(&self.carried).remove(&entry.durable_id);
        entry.connection.close();
    }

    fn request(&self, entry: &Carried, frame: Value) -> Result<Value, CarrierError> {
        entry.connection.request(frame).map_err(CarrierError::Host)
    }

    fn release_own(&self, entry: &Carried) {
        let _ = entry
            .connection
            .request(json!({ "type": "release_attachment", "sessionId": entry.session_id }));
    }

    fn record_held(
        &self,
        durable_id: &str,
        session_file: &str,
        view: &EngineView,
    ) -> Result<(), StoreError> {
        let hold_kinds = if view.hold_kinds.is_empty() {
            vec!["paused".to_owned()]
        } else {
            view.hold_kinds.clone()
        };
        self.options.store.session_wait().write(&SessionWaitWrite {
            durable_id: durable_id.to_owned(),
            wait: SessionWaitKind::Held,
            hold_kinds,
            file_size: file_size(Path::new(session_file)),
        })
    }

    /// The quiet hand-back (A3.2.4). On a held session a refusal naming a held delivery is retried once with
    /// interrupt, which drops it. A finished session refused for a delivery has one queued to run (a wake admitted
    /// while the engine was still restoring it): it is kept, not interrupted, since an interrupt parks that delivery
    /// for a person's turn, for up to QUEUED_DELIVERY_GRACE_MS; a delivery still refused after that is held, and the
    /// interrupt drops it.
    fn quiet_release(
        &self,
        entry: &Carried,
        drop_held_delivery: bool,
    ) -> Result<ReleaseReply, CarrierError> {
        let first = self.request(
            entry,
            json!({ "type": "release_session", "sessionId": entry.session_id, "reason": "takeover" }),
        )?;
        if succeeded(&first) {
            return Ok(ReleaseReply::Released);
        }
        let for_delivery = code_of(&first) == "turn_active"
            && busy_list(&first).iter().any(|busy| busy == "delivery");
        if for_delivery && !drop_held_delivery {
            return Ok(ReleaseReply::Refused {
                code: "turn_active".to_owned(),
                for_delivery: true,
            });
        }
        if for_delivery {
            let second = self.request(
                entry,
                json!({ "type": "release_session", "sessionId": entry.session_id, "reason": "takeover", "interrupt": true }),
            )?;
            if succeeded(&second) {
                return Ok(ReleaseReply::Released);
            }
            return Ok(ReleaseReply::Refused {
                code: code_of(&second),
                for_delivery: false,
            });
        }
        Ok(ReleaseReply::Refused {
            code: code_of(&first),
            for_delivery: false,
        })
    }

    fn attached_others(&self, entry: &Carried) -> Result<bool, CarrierError> {
        let listed = self.request(entry, json!({ "type": "list_sessions", "observe": true }))?;
        let data = data_of(&listed);
        let Some(sessions) = data.get("sessions").and_then(Value::as_array) else {
            return Ok(false);
        };
        let attachments = sessions
            .iter()
            .find(|session| {
                session.get("sessionId").and_then(Value::as_str) == Some(entry.session_id.as_str())
            })
            .and_then(|session| session.get("attachments"))
            .map_or(0.0, number_value);
        Ok(attachments > 1.0)
    }

    fn read_state(&self, entry: &Carried) -> Result<EngineView, CarrierError> {
        let state = self.request(
            entry,
            json!({ "type": "get_state", "sessionId": entry.session_id }),
        )?;
        Ok(read_engine_state(&data_of(&state)))
    }

    fn evaluate(&self, durable_id: &str) -> Result<CarrierEvaluation, CarrierError> {
        self.serial(durable_id, || {
            let outcome = self.evaluate_now(durable_id)?;
            self.emit(durable_id, &outcome);
            Ok(outcome)
        })
    }

    fn evaluate_now(&self, durable_id: &str) -> Result<CarrierEvaluation, CarrierError> {
        let Some(entry) = self.carried_entry(durable_id) else {
            return Ok(CarrierEvaluation::NotCarried);
        };
        let store = &self.options.store;
        let fence = WokenSessionFence::new(durable_id, entry.generation);
        let renewed = match store.woken().renew(&fence)? {
            Ok(row) => row,
            Err(_) => {
                self.release_own(&entry);
                self.forget(&entry);
                return Ok(CarrierEvaluation::LostLease);
            }
        };
        if self.attached_others(&entry)? {
            self.release_own(&entry);
            let _ = store.woken().end(&fence)?;
            store.woken().reset_count(durable_id)?;
            self.forget(&entry);
            return Ok(CarrierEvaluation::HandedOver);
        }
        let view = self.read_state(&entry)?;
        if view.held {
            self.quiet_release(&entry, true)?;
            self.record_held(durable_id, &renewed.session_file, &view)?;
            let _ = store.woken().end(&fence)?;
            self.forget(&entry);
            return Ok(CarrierEvaluation::Held);
        }
        if view.waiting_for_person {
            let recorded = match store.woken().record_waiting(&fence)? {
                Ok(row) => row,
                Err(_) => return Ok(CarrierEvaluation::LostLease),
            };
            let since = recorded.first_waiting_at.unwrap_or_else(|| store.now());
            if store.now() - since < PERSON_WAIT_CLOSE_MS {
                return Ok(CarrierEvaluation::Waiting { since });
            }
            if !self.read_state(&entry)?.waiting_for_person {
                return Ok(CarrierEvaluation::Kept);
            }
            self.request(
                &entry,
                json!({ "type": "close_session", "sessionId": entry.session_id }),
            )?;
            let _ = store.woken().end(&fence)?;
            self.forget(&entry);
            return Ok(CarrierEvaluation::ClosedWaiting);
        }
        if renewed.first_waiting_at.is_some() {
            let _ = store.woken().clear_waiting(&fence)?;
        }
        if !view.finished {
            // Working again: a later refusal for a delivery gets its own full grace (omo#13709 review M1).
            *lock(&entry.delivery_refused_since) = None;
            return Ok(CarrierEvaluation::Kept);
        }
        let refused_since = *lock(&entry.delivery_refused_since);
        let grace_over =
            refused_since.is_some_and(|since| store.now() - since >= QUEUED_DELIVERY_GRACE_MS);
        let released = self.quiet_release(&entry, grace_over)?;
        let refused_for_delivery = matches!(
            released,
            ReleaseReply::Refused {
                for_delivery: true,
                ..
            }
        );
        {
            let mut since = lock(&entry.delivery_refused_since);
            *since = if refused_for_delivery {
                Some((*since).unwrap_or_else(|| store.now()))
            } else {
                None
            };
        }
        if let ReleaseReply::Refused { code, .. } = released {
            if code == "turn_active" || code == "session_busy" {
                return Ok(CarrierEvaluation::Kept);
            }
            if code == "attached" {
                self.release_own(&entry);
                let _ = store.woken().end(&fence)?;
                store.woken().reset_count(durable_id)?;
                self.forget(&entry);
                return Ok(CarrierEvaluation::HandedOver);
            }
            let _ = store.woken().end(&fence)?;
            self.forget(&entry);
            return Ok(CarrierEvaluation::CloseFailed { code });
        }
        let _ = entry
            .connection
            .request(json!({ "type": "detach_connection" }));
        let _ = store.woken().end(&fence)?;
        self.forget(&entry);
        Ok(CarrierEvaluation::Released)
    }

    fn evaluate_later(self: &Arc<Self>, durable_id: &str) {
        let weak = Arc::downgrade(self);
        let durable_id = durable_id.to_owned();
        let _ = (self.options.schedule)(
            Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    let _ = inner.evaluate(&durable_id);
                }
            }),
            0,
        );
    }

    fn carry(
        self: &Arc<Self>,
        durable_id: &str,
        connection: Arc<dyn CarrierHostConnection>,
        session_id: String,
        generation: i64,
    ) -> Arc<Carried> {
        let entry_id = self.next_entry.fetch_add(1, Ordering::SeqCst);
        let listener: CarrierFrameListener = {
            let weak = Arc::downgrade(self);
            let durable_id = durable_id.to_owned();
            let session_id = session_id.clone();
            Arc::new(move |frame: &Value| {
                let lifecycle = frame.get("type").and_then(Value::as_str)
                    == Some("session_lifecycle")
                    && frame.get("sessionId").and_then(Value::as_str) == Some(session_id.as_str());
                if lifecycle && let Some(inner) = weak.upgrade() {
                    inner.evaluate_later(&durable_id);
                }
            })
        };
        let off = connection.on_event(listener);
        let entry = Arc::new(Carried {
            entry_id,
            durable_id: durable_id.to_owned(),
            connection: connection.clone(),
            session_id,
            generation,
            off: Mutex::new(Some(off)),
            delivery_refused_since: Mutex::new(None),
        });
        lock(&self.carried).insert(durable_id.to_owned(), entry.clone());
        let weak = Arc::downgrade(self);
        let closed_id = durable_id.to_owned();
        connection.on_closed(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                let mut carried = lock(&inner.carried);
                if carried
                    .get(&closed_id)
                    .is_some_and(|current| current.entry_id == entry_id)
                {
                    carried.remove(&closed_id);
                }
            }
        }));
        entry
    }

    fn open(&self, session_path: &str, cwd: &str) -> OpenOutcome {
        if (self.options.ensure_host)().is_err() {
            return OpenOutcome::Unreachable;
        }
        let Ok(connection) = (self.options.connect)() else {
            return OpenOutcome::Unreachable;
        };
        let reply = match connection.request(json!({
            "type": "open_session",
            "sessionPath": session_path,
            "cwd": cwd,
            "retain_on_disconnect": true,
        })) {
            Ok(reply) => reply,
            Err(_) => {
                connection.close();
                return OpenOutcome::Unreachable;
            }
        };
        if !succeeded(&reply) {
            connection.close();
            return OpenOutcome::Refused {
                code: code_of(&reply),
            };
        }
        let data = data_of(&reply);
        let session_id = data.get("sessionId").map(text_of).unwrap_or_default();
        let attached = data.get("attached") == Some(&Value::Bool(true));
        let view = read_engine_state(data.get("state").unwrap_or(&Value::Null));
        OpenOutcome::Opened(OpenedSession {
            connection,
            session_id,
            attached,
            view,
        })
    }

    /// The one rule of A7.2.3 (A7R-10): a carrier opens a session only when the session holds a queued row whose own
    /// switch allows the open. A row from another device (it carries the intake's ledger id) reads the wake the
    /// consumer was told and its sender's admission now, failing closed when the answer is unknown or refused; a
    /// same-device row reads `session_wake.same_device` for the target session's cwd. A row queued before the session
    /// was closed on request is settled `target_closed` first and opens nothing (omo#14273).
    fn open_allowance(
        &self,
        durable_id: &str,
        cwd: &str,
        closed_at: Option<i64>,
    ) -> Result<Allowance, CarrierError> {
        let same_device = self.options.same_device.as_ref().is_some_and(|on| on(cwd));
        let remote_wake = self.options.remote_wake.as_ref().is_some_and(|on| on());
        let mut queued = false;
        for row in settle_closed_intake(self.options.store.as_ref(), durable_id, closed_at)? {
            queued = true;
            let Some(ledger_id) = row.ledger_id.as_deref() else {
                if same_device {
                    return Ok(Allowance::Open);
                }
                continue;
            };
            if !remote_wake {
                continue;
            }
            let Some(device) = ledger_device(ledger_id) else {
                continue;
            };
            let Some(admit) = &self.options.admit_intake else {
                return Ok(Allowance::Open);
            };
            if admit(device) == IntakeAdmission::Admitted {
                return Ok(Allowance::Open);
            }
        }
        Ok(if queued {
            Allowance::SwitchedOff
        } else {
            Allowance::NothingQueued
        })
    }

    /// Only a record that is still `requested` is ended, compared on its generation: another carrier's record that is
    /// being opened or is open keeps its lease, so that carrier still closes the session it carries.
    fn end_nothing_queued(&self, durable_id: &str) -> Result<CarrierWakeOutcome, CarrierError> {
        let store = &self.options.store;
        let Some(record) = store
            .woken()
            .list()?
            .into_iter()
            .find(|row| row.durable_id == durable_id)
        else {
            return Ok(CarrierWakeOutcome::NothingQueued);
        };
        if record.state != WokenSessionState::Requested {
            return Ok(busy_in_progress());
        }
        Ok(
            match store
                .woken()
                .end(&WokenSessionFence::new(durable_id, record.generation))?
            {
                Ok(()) => CarrierWakeOutcome::NothingQueued,
                Err(_) => busy_in_progress(),
            },
        )
    }

    fn wake_now(self: &Arc<Self>, durable_id: &str) -> Result<CarrierWakeOutcome, CarrierError> {
        if let Some(entry) = self.carried_entry(durable_id) {
            return Ok(CarrierWakeOutcome::Open {
                session_id: entry.session_id.clone(),
                generation: entry.generation,
            });
        }
        let (session_path, cwd, closed_at) =
            match resolve_wake_target(&self.options.sessions_directory, durable_id) {
                WakeTarget::Gone => return Ok(CarrierWakeOutcome::Gone),
                WakeTarget::Ambiguous { session_paths } => {
                    return Ok(CarrierWakeOutcome::Ambiguous { session_paths });
                }
                WakeTarget::Found { session_kind, .. }
                    if session_kind != WakeSessionKind::Interactive =>
                {
                    return Ok(CarrierWakeOutcome::NotWakeable { session_kind });
                }
                WakeTarget::Found {
                    session_path,
                    cwd,
                    closed_at,
                    ..
                } => (session_path, cwd, closed_at),
            };
        let store = &self.options.store;
        if let Some(wait) = store.session_wait().read(durable_id)?
            && wait.wait == SessionWaitKind::Held
            && wait_blocks_delivery(&wait)
            && wait.file_size == file_size(Path::new(&session_path))
        {
            return Ok(CarrierWakeOutcome::Held);
        }
        match self.open_allowance(durable_id, &cwd, closed_at)? {
            Allowance::Open => {}
            Allowance::NothingQueued => return self.end_nothing_queued(durable_id),
            Allowance::SwitchedOff => {
                store.woken().request(durable_id, &session_path)?;
                return Ok(CarrierWakeOutcome::SwitchedOff);
            }
        }
        let limit = self.options.wake_limit.as_ref().map_or(0, |limit| limit());
        if limit > 0 && store.woken().wake_count(durable_id)? >= limit {
            return Ok(CarrierWakeOutcome::WakeLimitReached);
        }
        store.woken().request(durable_id, &session_path)?;
        // Read outside the store's transaction: a short host defers the wake with its row requested, and the sweep
        // starts it once the headroom is back.
        if let Some(available) = self
            .options
            .memory_headroom
            .as_ref()
            .and_then(|probe| probe())
        {
            let floor = self
                .options
                .memory_floor
                .as_ref()
                .map_or(WAKE_MEMORY_FLOOR_BYTES, |floor| floor());
            if available < floor {
                return Ok(CarrierWakeOutcome::HostMemoryPressure {
                    available_bytes: available,
                    floor_bytes: floor,
                });
            }
        }
        let row = match store.woken().start(durable_id)? {
            WakeStartOutcome::Started { row } => row,
            WakeStartOutcome::NotYet { next_attempt_at } => {
                return Ok(CarrierWakeOutcome::NotYet { next_attempt_at });
            }
            WakeStartOutcome::InProgress { .. } | WakeStartOutcome::NotFound => {
                return Ok(busy_in_progress());
            }
        };
        let fence = WokenSessionFence::new(durable_id, row.generation);
        let opened = match self.open(&session_path, &cwd) {
            OpenOutcome::Opened(opened) => opened,
            OpenOutcome::Refused { code }
                if HELD_ELSEWHERE.contains(&code.as_str())
                    || DEFINITIVE.contains(&code.as_str()) =>
            {
                let _ = store.woken().end(&fence)?;
                if DEFINITIVE.contains(&code.as_str()) {
                    return Ok(CarrierWakeOutcome::Gone);
                }
                return Ok(CarrierWakeOutcome::Busy { code });
            }
            OpenOutcome::Refused { .. } | OpenOutcome::Unreachable => {
                return Ok(match store.woken().retry_later(&fence)? {
                    WakeRetryOutcome::Failed { attempts } => {
                        CarrierWakeOutcome::Failed { attempts }
                    }
                    WakeRetryOutcome::LostLease => busy_in_progress(),
                    WakeRetryOutcome::Deferred { row } => CarrierWakeOutcome::Deferred {
                        attempts: row.attempts,
                    },
                });
            }
        };
        let OpenedSession {
            connection,
            session_id,
            attached,
            view,
        } = opened;
        let entry = self.carry(durable_id, connection, session_id, row.generation);
        if view.held {
            if attached {
                self.release_own(&entry);
            } else {
                self.quiet_release(&entry, true)?;
            }
            self.record_held(durable_id, &session_path, &view)?;
            let _ = store.woken().end(&fence)?;
            self.forget(&entry);
            return Ok(CarrierWakeOutcome::Held);
        }
        if view.waiting_for_person {
            store.session_wait().write(&SessionWaitWrite {
                durable_id: durable_id.to_owned(),
                wait: SessionWaitKind::Waiting,
                hold_kinds: Vec::new(),
                file_size: file_size(Path::new(&session_path)),
            })?;
            self.release_own(&entry);
            let _ = store.woken().end(&fence)?;
            self.forget(&entry);
            return Ok(CarrierWakeOutcome::Waiting);
        }
        store.session_wait().clear(durable_id)?;
        store
            .woken()
            .count_wake(durable_id, view.human_input_revision)?;
        if store
            .woken()
            .set_state(&fence, WokenSessionState::Open, None)?
            .is_err()
        {
            self.release_own(&entry);
            self.forget(&entry);
            return Ok(busy_in_progress());
        }
        let _ = entry
            .connection
            .request(json!({ "type": "wake", "sessionId": entry.session_id }));
        Ok(CarrierWakeOutcome::Open {
            session_id: entry.session_id.clone(),
            generation: entry.generation,
        })
    }

    fn take_over(self: &Arc<Self>, durable_id: &str) -> Result<(), CarrierError> {
        let store = &self.options.store;
        let WakeTakeOverOutcome::TakenOver { row, .. } = store.woken().take_over(durable_id)?
        else {
            return Ok(());
        };
        let fence = WokenSessionFence::new(durable_id, row.generation);
        // The same refusals a wake makes: a session that is gone, ambiguous, or no longer interactive is not reopened.
        let (session_path, cwd, closed_at) =
            match resolve_wake_target(&self.options.sessions_directory, durable_id) {
                WakeTarget::Found {
                    session_path,
                    cwd,
                    session_kind: WakeSessionKind::Interactive,
                    closed_at,
                } => (session_path, cwd, closed_at),
                WakeTarget::Gone | WakeTarget::Ambiguous { .. } | WakeTarget::Found { .. } => {
                    let _ = store.woken().end(&fence)?;
                    return Ok(());
                }
            };
        if self.open_allowance(durable_id, &cwd, closed_at)? != Allowance::Open {
            let _ = store.woken().end(&fence)?;
            return Ok(());
        }
        let OpenOutcome::Opened(opened) = self.open(&session_path, &cwd) else {
            return Ok(());
        };
        self.carry(
            durable_id,
            opened.connection,
            opened.session_id,
            row.generation,
        );
        // Already inside this session's serial turn: evaluate directly, never through `evaluate`, which would wait on
        // itself.
        let outcome = self.evaluate_now(durable_id)?;
        self.emit(durable_id, &outcome);
        Ok(())
    }

    /// Whether the timer still has work to tend: a woken row or a carried session (A3.1.4).
    fn sweep_now(self: &Arc<Self>) -> Result<bool, CarrierError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let store = &self.options.store;
        let now = store.now();
        let rows = store.woken().list()?;
        for row in &rows {
            if self.carried_entry(&row.durable_id).is_some() {
                let _ = self.evaluate(&row.durable_id);
                continue;
            }
            if row.state == WokenSessionState::Requested {
                if row.next_attempt_at.is_none_or(|at| at <= now) {
                    let _ = self.serial(&row.durable_id, || self.wake_now(&row.durable_id));
                }
                continue;
            }
            if row.lease_expires_at.is_some_and(|at| at <= now) {
                let _ = self.serial(&row.durable_id, || self.take_over(&row.durable_id));
            }
        }
        let has_rows = !rows.is_empty();
        let carrying = !lock(&self.carried).is_empty();
        Ok(has_rows || carrying)
    }

    fn start_sweep(self: &Arc<Self>) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let Ok(rows) = self.options.store.woken().list() else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        if self.sweep_now().unwrap_or(false) {
            self.arm();
        }
    }

    fn arm(self: &Arc<Self>) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut timer = lock(&self.timer);
            if timer.armed {
                return;
            }
            timer.armed = true;
        }
        let weak = Arc::downgrade(self);
        let cancel = (self.options.schedule)(
            Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.tick();
                }
            }),
            CARRIER_TICK_MS,
        );
        let mut timer = lock(&self.timer);
        if timer.armed {
            timer.cancel = Some(cancel);
        }
    }

    fn tick(self: &Arc<Self>) {
        {
            let mut timer = lock(&self.timer);
            timer.armed = false;
            timer.cancel = None;
        }
        // A sweep that failed still had work to tend: the timer stays armed for the next tick (A3.1.4).
        match self.sweep_now() {
            Ok(false) => {}
            Ok(true) | Err(_) => self.arm(),
        }
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let cancel = lock(&self.timer).cancel.take();
        if let Some(cancel) = cancel {
            cancel();
        }
        let chains: Vec<Arc<Mutex<()>>> = lock(&self.chains).values().cloned().collect();
        for chain in chains {
            let _turn = lock(&chain);
        }
        let entries: Vec<Arc<Carried>> = lock(&self.carried).values().cloned().collect();
        for entry in entries {
            self.forget(&entry);
        }
    }
}
