//! The wake primitive, steps 1 to 5 (omo#13037 design 5.2, amendment A3.1.5).
//!
//! Upstream counterpart: `thread/wake/session-waker.ts`. It carries no message and nothing after the open: resolve the
//! durable id under `<agentDir>/sessions`, refuse anything but an interactive session, take the row out of
//! `requested`, ensure the host and send one open. Every outcome of the open other than success takes the row out of
//! `opening` in one compare-and-set. One wake per durable id runs at a time in this process; a second caller waits
//! for the first one's answer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use serde_json::Value;

use super::carrier::EnsureHost;
use super::legacy_open::{WakeOpen, WakeOpenReply, WakeOpenRequest};
use super::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};
use crate::thread::store::SqliteGatewayStore;
use crate::thread::store_ops::StoreError;
use crate::thread::woken_types::{WakeRetryOutcome, WakeStartOutcome, WokenSessionFence};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The session is open (or the open attached to it); the row stays `opening` for the carrier's next step.
    Live {
        session_id: String,
        state: Value,
        generation: i64,
    },
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    NotWakeable {
        session_kind: WakeSessionKind,
    },
    NotYet {
        next_attempt_at: i64,
    },
    Busy {
        code: String,
        retry_after_ms: Option<i64>,
    },
    Deferred {
        attempts: i64,
        next_attempt_at: i64,
    },
    Failed {
        attempts: i64,
        error: String,
    },
}

pub struct SessionWakerOptions {
    pub store: Arc<SqliteGatewayStore>,
    /// `<agentDir>/sessions`: the only place a durable id is resolved; a caller never names a path.
    pub sessions_directory: PathBuf,
    pub ensure_host: EnsureHost,
    pub open: WakeOpen,
}

pub struct SessionWaker {
    options: SessionWakerOptions,
}

pub fn create_session_waker(options: SessionWakerOptions) -> SessionWaker {
    SessionWaker { options }
}

/// A holder outside the wake's host owns the file; its own drain admits, so the wake steps aside (A3.1.5).
const HELD_ELSEWHERE: [&str; 2] = ["session_held", "session_path_in_use"];
const DEFINITIVE: [&str; 2] = ["invalid_path", "unknown_session"];

type FlightAnswer = Result<WakeOutcome, String>;

#[derive(Default)]
struct Flight {
    answer: Mutex<Option<FlightAnswer>>,
    settled: Condvar,
}

type Flights = Mutex<HashMap<String, Arc<Flight>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn in_flight() -> &'static Flights {
    static FLIGHTS: OnceLock<Flights> = OnceLock::new();
    FLIGHTS.get_or_init(Flights::default)
}

fn wake_in_progress() -> WakeOutcome {
    WakeOutcome::Busy {
        code: "wake_in_progress".to_owned(),
        retry_after_ms: None,
    }
}

fn settle_open(
    store: &SqliteGatewayStore,
    fence: &WokenSessionFence,
    reply: WakeOpenReply,
) -> Result<WakeOutcome, StoreError> {
    let error = match reply {
        WakeOpenReply::Opened {
            session_id, state, ..
        } => {
            return Ok(WakeOutcome::Live {
                session_id,
                state,
                generation: fence.generation,
            });
        }
        WakeOpenReply::Refused {
            code,
            retry_after_ms,
        } if HELD_ELSEWHERE.contains(&code.as_str()) || DEFINITIVE.contains(&code.as_str()) => {
            let _ = store.woken().end(fence)?;
            if DEFINITIVE.contains(&code.as_str()) {
                return Ok(WakeOutcome::Gone);
            }
            return Ok(WakeOutcome::Busy {
                code,
                retry_after_ms,
            });
        }
        WakeOpenReply::Refused { code, .. } => code,
        WakeOpenReply::Unreachable { message } => message,
    };
    Ok(match store.woken().retry_later(fence)? {
        WakeRetryOutcome::LostLease => wake_in_progress(),
        WakeRetryOutcome::Failed { attempts } => WakeOutcome::Failed { attempts, error },
        WakeRetryOutcome::Deferred { row } => WakeOutcome::Deferred {
            attempts: row.attempts,
            next_attempt_at: row.next_attempt_at.unwrap_or(0),
        },
    })
}

fn wake(options: &SessionWakerOptions, durable_id: &str) -> Result<WakeOutcome, StoreError> {
    let (session_path, cwd) = match resolve_wake_target(&options.sessions_directory, durable_id) {
        WakeTarget::Gone => return Ok(WakeOutcome::Gone),
        WakeTarget::Ambiguous { session_paths } => {
            return Ok(WakeOutcome::Ambiguous { session_paths });
        }
        WakeTarget::Found { session_kind, .. } if session_kind != WakeSessionKind::Interactive => {
            return Ok(WakeOutcome::NotWakeable { session_kind });
        }
        WakeTarget::Found {
            session_path, cwd, ..
        } => (session_path, cwd),
    };
    options.store.woken().request(durable_id, &session_path)?;
    let row = match options.store.woken().start(durable_id)? {
        WakeStartOutcome::Started { row } => row,
        WakeStartOutcome::NotYet { next_attempt_at } => {
            return Ok(WakeOutcome::NotYet { next_attempt_at });
        }
        WakeStartOutcome::InProgress { .. } | WakeStartOutcome::NotFound => {
            return Ok(wake_in_progress());
        }
    };
    let fence = WokenSessionFence::new(durable_id, row.generation);
    let reply = match (options.ensure_host)() {
        Err(message) => WakeOpenReply::Unreachable { message },
        Ok(()) => (options.open)(&WakeOpenRequest { session_path, cwd }),
    };
    settle_open(&options.store, &fence, reply)
}

impl SessionWaker {
    pub fn ensure_session_running(&self, durable_id: &str) -> Result<WakeOutcome, StoreError> {
        let key = format!(
            "{}\u{0}{durable_id}",
            self.options.sessions_directory.display()
        );
        let (flight, leader) = {
            let mut flights = lock(in_flight());
            match flights.get(&key) {
                Some(flight) => (flight.clone(), false),
                None => {
                    let flight = Arc::new(Flight::default());
                    flights.insert(key.clone(), flight.clone());
                    (flight, true)
                }
            }
        };
        if !leader {
            let mut answer = lock(&flight.answer);
            while answer.is_none() {
                answer = flight
                    .settled
                    .wait(answer)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            return match answer.clone() {
                Some(Ok(outcome)) => Ok(outcome),
                Some(Err(error)) => Err(StoreError::Message(error)),
                None => Ok(wake_in_progress()),
            };
        }
        let result = wake(&self.options, durable_id);
        *lock(&flight.answer) = Some(match &result {
            Ok(outcome) => Ok(outcome.clone()),
            Err(error) => Err(error.message()),
        });
        flight.settled.notify_all();
        lock(in_flight()).remove(&key);
        result
    }
}
