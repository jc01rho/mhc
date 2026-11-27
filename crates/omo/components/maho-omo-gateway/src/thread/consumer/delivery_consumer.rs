//! The delivery consumer: what the wake runtime starts and asks it (omo#13037 design 5.4, A2.4, A2.14).
//!
//! Upstream counterpart: `thread/consumer/consumer.ts`. A handed record is resolved here first (gone, ambiguous and
//! child sessions are refused before anything is committed), then committed through the store's intake and answered
//! from the row; only after the answer does the consumer ask whether the sender is still admitted, and only a yes wakes
//! the session. A no settles the row refused. The pending list is reported after every declaration.
//!
//! The local-API client behind the port (`consumer/device-service-port.ts`) is not ported, since the device service in
//! this repository serves no `deliveries.*` protocol; the runtime takes a factory, so a host that brings one binds it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use serde_json::{Map, Value, json};

use super::port::{
    DeliveryHoldsEvent, DeliveryInboundEvent, DeliveryPendingPair, DeliveryServicePort,
    DrainedReply, PortUnsubscribe, decode_serve_ack, refusal_code,
};
use crate::thread::address_book::find_disk_sessions;
use crate::thread::session_wait_types::{
    DrainedMark, IntakePair, QueuedWaitingOn, QueuedWaitingOnKind,
};
use crate::thread::store::SqliteGatewayStore;
use crate::thread::store_api::GatewayStoreApi;
use crate::thread::store_ops::ReceiptOutcomeRequest;
use crate::thread::store_tool_receipts::ReconcileRemoteReceiptRequest;
use crate::thread::types::{
    IntakeOutcome, IntakeRecord, IntakeRequest, IntakeSender, Provenance, ReceiptOutcome,
    ReceiptRefusalReason, RefusalNotice, RefusalReason,
};
use crate::thread::wake::pending_sweep::WakeSession;
use crate::thread::wake::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};

pub const DELIVERY_PROTOCOL_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerStart {
    Serving,
    Unsupported,
    SchemaTooNew,
}

impl ConsumerStart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Unsupported => "unsupported",
            Self::SchemaTooNew => "schema_too_new",
        }
    }
}

pub trait DeliveryConsumer: Send + Sync {
    fn start(&self) -> Result<ConsumerStart, String>;
    /// From this start's acknowledgement until the consumer stops: the service hands this consumer its deliveries.
    fn serving(&self) -> bool;
    /// One `delivery.drained` report per marked pair whose intake rows are all applied.
    fn report_drained(&self) -> Result<(), String>;
    /// Waits for every step this consumer started (an answer, a wake, a held reply) to finish.
    fn idle(&self) {}
    fn stop(&self);
}

/// The threads' chain count limit for a target session's cwd; `None` when the user set none.
pub type ChainCountLimit = Arc<dyn Fn(&str) -> Option<u64> + Send + Sync>;
/// A tracked step that failed (a wake, an answer, a held reply): reported here, never dropped.
pub type ConsumerErrorSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct DeliveryConsumerOptions {
    pub store: Arc<SqliteGatewayStore>,
    pub port: Arc<dyn DeliveryServicePort>,
    pub agent_dir: String,
    pub sessions_directory: PathBuf,
    pub chain_count_limit: ChainCountLimit,
    /// Starts the session (the carrier's wake); called only after the sender passed the admission check.
    pub wake: WakeSession,
    pub on_error: Option<ConsumerErrorSink>,
}

pub type CreateDeliveryConsumer =
    Arc<dyn Fn(DeliveryConsumerOptions) -> Arc<dyn DeliveryConsumer> + Send + Sync>;

const UNKNOWN_SHAPE: [&str; 2] = ["unknown_type", "unsupported"];
/// The numbers a refusal's details carry to the sender: a chain limit's counts, the pair loop guard's burst and wait.
const REFUSAL_NUMBERS: [&str; 5] = [
    "chain_count",
    "limit",
    "burst",
    "window_ms",
    "retry_after_ms",
];

fn is_unknown_shape(code: Option<&str>) -> bool {
    code.is_some_and(|code| UNKNOWN_SHAPE.contains(&code))
}

fn intake_record(event: &DeliveryInboundEvent) -> Option<IntakeRecord> {
    let record = event.record.as_object()?;
    let deliver_as = record.get("deliverAs")?.as_str()?.to_owned();
    if deliver_as != "followUp" && deliver_as != "steer" {
        return None;
    }
    let sender = record.get("sender")?.as_object()?;
    Some(IntakeRecord {
        session_id: event.durable_id.clone(),
        delivery_id: record.get("deliveryId")?.as_str()?.to_owned(),
        text: record.get("text")?.as_str()?.to_owned(),
        display_text: record.get("displayText")?.as_str()?.to_owned(),
        sender: IntakeSender {
            kind: sender.get("kind")?.as_str()?.to_owned(),
            session_id: sender
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            name: sender
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        deliver_as,
        chain_count: record.get("chainCount")?.as_i64()?,
    })
}

/// The answer for a record the intake refused. The store names the precise cause in `details.reason` (for example
/// `chain_count_limit`, with `chain_count` and `limit`); the sender maps that reason, so it is sent in place of the
/// coarse tool code, which stays the reason only when the store named none (the pair loop guard: `pair_loop_guard`,
/// with `burst`, `window_ms` and `retry_after_ms`).
fn intake_refusal(code: &str, details: Option<&Value>) -> Value {
    let details = details.and_then(Value::as_object);
    let reason = details
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or(code);
    let mut answer = Map::new();
    answer.insert("kind".to_owned(), json!("refused"));
    answer.insert("reason".to_owned(), json!(reason));
    if let Some(details) = details {
        for key in REFUSAL_NUMBERS {
            if let Some(number) = details.get(key).and_then(Value::as_i64) {
                answer.insert(key.to_owned(), json!(number));
            }
        }
    }
    Value::Object(answer)
}

fn receipt_outcome_of(record: &Value) -> ReceiptOutcome {
    match record
        .get("reason")
        .and_then(Value::as_str)
        .and_then(ReceiptRefusalReason::parse)
    {
        Some(reason) => ReceiptOutcome::Refused { reason },
        None => ReceiptOutcome::OutcomeUnavailable,
    }
}

/// The consumer's tracked work, so `idle` waits for it instead of polling and `stop` lets it finish.
type Follow = Arc<(Mutex<usize>, Condvar)>;

fn track(follow: &Follow, work: impl FnOnce() + Send + 'static) {
    let live = Arc::clone(follow);
    *live
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
    std::thread::spawn(move || {
        work();
        let mut count = live
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count -= 1;
        live.1.notify_all();
    });
}

fn wait_idle(follow: &Follow) {
    let mut count = follow
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *count > 0 {
        count = follow
            .1
            .wait(count)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

struct Consumer {
    store: Arc<SqliteGatewayStore>,
    port: Arc<dyn DeliveryServicePort>,
    agent_dir: String,
    sessions_directory: PathBuf,
    chain_count_limit: ChainCountLimit,
    wake: WakeSession,
    on_error: Option<ConsumerErrorSink>,
    follow: Follow,
    marks_kept: AtomicBool,
    serving: AtomicBool,
    unsubscribe: Mutex<Vec<PortUnsubscribe>>,
    me: Weak<Consumer>,
}

impl Consumer {
    fn report(&self, error: impl std::fmt::Display) {
        if let Some(sink) = &self.on_error {
            sink(error.to_string());
        }
    }

    fn answer(&self, event: &DeliveryInboundEvent, reply: &Value) -> Result<(), String> {
        match self
            .port
            .answer(&event.sender_device, &event.command_id, reply)
        {
            Ok(()) => Ok(()),
            Err(error) if refusal_code(&error) == Some("unknown_delivery") => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn outcome_notice(&self, event: &DeliveryInboundEvent) -> Result<(), String> {
        if !self
            .port
            .peer_admitted(&event.sender_device)
            .map_err(|error| error.to_string())?
        {
            return self.answer(
                event,
                &json!({ "kind": "refused", "reason": "sender_not_admitted" }),
            );
        }
        let delivery_id = event.record.get("deliveryId").and_then(Value::as_str);
        let outcome = receipt_outcome_of(&event.record);
        if let (Some(delivery_id), ReceiptOutcome::Refused { .. }) = (delivery_id, outcome) {
            let principal = format!("session:{}", event.durable_id);
            let held = self
                .store
                .remote_receipt(self.store.now(), &principal, delivery_id)
                .map_err(|error| error.to_string())?;
            let target = event
                .record
                .get("targetSessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(held) = held
                && held.result["device"].as_str() == Some(event.sender_device.as_str())
                && held.result["thread_id"].as_str() == Some(target.as_str())
                && self
                    .port
                    .peer_admitted(&event.sender_device)
                    .map_err(|error| error.to_string())?
            {
                self.store
                    .reconcile_remote_receipt(&ReconcileRemoteReceiptRequest {
                        now: self.store.now(),
                        principal,
                        idempotency_key: held.idempotency_key,
                        delivery_id: delivery_id.to_owned(),
                        target_durable_id: target,
                        device: event.sender_device.clone(),
                        outcome,
                    })
                    .map_err(|error| error.to_string())?;
            }
        }
        // A missing receipt, a duplicate and a kept notice have the same acknowledgment.
        self.answer(event, &json!({ "accepted": true }))
    }

    fn get_delivery_outcome(&self, event: &DeliveryInboundEvent) -> Result<(), String> {
        // The service attests the device principal and checks its grant again after this answer. Check here too,
        // before touching a transcript or returning any receipt state.
        if !self
            .port
            .peer_admitted(&event.sender_device)
            .map_err(|error| error.to_string())?
        {
            return self.answer(
                event,
                &json!({ "kind": "refused", "reason": "sender_not_admitted" }),
            );
        }
        let Some(delivery_id) = event.record.get("deliveryId").and_then(Value::as_str) else {
            return self.answer(
                event,
                &json!({ "outcome": { "kind": "outcome_unavailable" } }),
            );
        };
        let target = resolve_wake_target(&self.sessions_directory, &event.durable_id);
        let outcome = self
            .store
            .receipt_outcome(&ReceiptOutcomeRequest {
                now: self.store.now(),
                principal: format!("device:{}", event.sender_device),
                delivery_id: delivery_id.to_owned(),
                target_durable_id: event.durable_id.clone(),
                session_path: match &target {
                    WakeTarget::Found { session_path, .. } => Some(session_path.clone()),
                    _ => None,
                },
            })
            .map_err(|error| error.to_string())?;
        if !self
            .port
            .peer_admitted(&event.sender_device)
            .map_err(|error| error.to_string())?
        {
            return self.answer(
                event,
                &json!({ "kind": "refused", "reason": "sender_not_admitted" }),
            );
        }
        self.answer(event, &json!({ "outcome": outcome }))
    }

    fn settle_after_answer(&self, durable_id: &str, sender_device: &str, row_id: &str) {
        let admitted = match self.port.peer_admitted(sender_device) {
            Ok(admitted) => admitted,
            Err(error) => return self.report(error),
        };
        let settled = if admitted {
            (self.wake)(durable_id)
        } else {
            self.store
                .refuse_queued(self.store.now(), row_id, RefusalReason::SenderNotAdmitted)
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        if let Err(error) = settled {
            self.report(error);
        }
    }

    fn on_inbound(&self, event: &DeliveryInboundEvent) -> Result<(), String> {
        match event.record.get("type").and_then(Value::as_str) {
            Some("delivery_outcome_notice") => return self.outcome_notice(event),
            Some("get_delivery_outcome") => return self.get_delivery_outcome(event),
            _ => {}
        }
        let target = resolve_wake_target(&self.sessions_directory, &event.durable_id);
        let refusal = match &target {
            WakeTarget::Gone => Some("session_gone"),
            WakeTarget::Ambiguous { .. } => Some("ambiguous_session"),
            WakeTarget::Found { session_kind, .. } => {
                (*session_kind != WakeSessionKind::Interactive).then_some("not_wakeable")
            }
        };
        let WakeTarget::Found { cwd, .. } = &target else {
            return self.answer(
                event,
                &json!({ "kind": "refused", "reason": refusal.unwrap_or("invalid_record") }),
            );
        };
        if let Some(reason) = refusal {
            return self.answer(event, &json!({ "kind": "refused", "reason": reason }));
        }
        let Some(record) = intake_record(event) else {
            return self.answer(
                event,
                &json!({ "kind": "refused", "reason": "invalid_record" }),
            );
        };
        // A limit that cannot be read (the settings barrel failed to load) fails open: the delivery is queued as it was
        // before the limit was wired, and the failure is reported, so the sender never waits on a delivery nobody
        // answered.
        let chain_count_limit = (self.chain_count_limit)(cwd).map(|limit| limit as i64);
        let outcome = self
            .store
            .intake(&IntakeRequest {
                now: self.store.now(),
                attested_device: event.sender_device.clone(),
                record,
                provenance: Provenance {
                    workspace: "remote".to_owned(),
                    project: "remote".to_owned(),
                    lane: None,
                    session: None,
                },
                target_session_kind: "interactive".to_owned(),
                chain_count_limit,
            })
            .map_err(|error| error.to_string())?;
        let (delivery_id, row_id, ledger_id, queue_position, deduplicated) = match outcome {
            IntakeOutcome::Busy => {
                return self.answer(event, &json!({ "kind": "refused", "reason": "busy" }));
            }
            IntakeOutcome::Refused {
                code,
                details,
                message: _,
            } => {
                return self.answer(event, &intake_refusal(code.as_str(), details.as_ref()));
            }
            IntakeOutcome::Queued {
                delivery_id,
                row_id,
                ledger_id,
                queue_position,
                deduplicated,
            } => (delivery_id, row_id, ledger_id, queue_position, deduplicated),
        };
        let pair = IntakePair {
            sender_device: event.sender_device.clone(),
            session_id: event.durable_id.clone(),
        };
        if !deduplicated {
            self.store
                .intake_marks()
                .record(&pair, &event.command_id, Some(&ledger_id))
                .map_err(|error| error.to_string())?;
        }
        let stored_command = if deduplicated {
            self.store
                .intake_marks()
                .command_for(&ledger_id)
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| event.command_id.clone())
        } else {
            event.command_id.clone()
        };
        let waiting_on = self
            .store
            .session_wait()
            .waiting_on(&event.durable_id)
            .map_err(|error| error.to_string())?
            .unwrap_or(QueuedWaitingOn {
                kind: QueuedWaitingOnKind::Wake,
            });
        self.answer(
            event,
            &json!({
                "kind": "queued",
                "stored_command_id": stored_command,
                "delivery_id": delivery_id,
                "queue_position": queue_position,
                "deduplicated": deduplicated,
                "waiting_on": waiting_on,
            }),
        )?;
        if let Some(me) = self.me.upgrade() {
            let durable_id = event.durable_id.clone();
            let sender_device = event.sender_device.clone();
            track(&self.follow, move || {
                me.settle_after_answer(&durable_id, &sender_device, &row_id);
            });
        }
        Ok(())
    }

    fn report_pending(&self) -> Result<(), String> {
        if !self.marks_kept.load(Ordering::SeqCst) {
            return Ok(());
        }
        let marks = self
            .store
            .intake_marks()
            .pending()
            .map_err(|error| error.to_string())?;
        let pairs: Vec<DeliveryPendingPair> = marks
            .into_iter()
            .map(|mark| DeliveryPendingPair {
                sender_device: mark.sender_device,
                session_id: mark.session_id,
                last_command_id: mark.last_command_id,
            })
            .collect();
        if let Err(error) = self.port.pending(&pairs)
            && is_unknown_shape(refusal_code(&error))
        {
            self.marks_kept.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    fn send_refusal_notices(&self) {
        let notices = match self.store.refusal_notices(self.store.now()) {
            Ok(notices) => notices,
            Err(error) => return self.report(error),
        };
        for notice in notices {
            let admitted = match self.port.peer_admitted(&notice.device) {
                Ok(admitted) => admitted,
                Err(error) => {
                    self.report(error);
                    continue;
                }
            };
            if !admitted {
                let _ = self
                    .store
                    .claim_refusal_notice(self.store.now(), &notice, true);
                continue;
            }
            match self
                .store
                .claim_refusal_notice(self.store.now(), &notice, false)
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    self.report(error);
                    continue;
                }
            }
            match self.port.peer_admitted(&notice.device) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    self.report(error);
                    continue;
                }
            }
            if let Err(error) = self.port.notice(&notice) {
                self.report(error);
            }
        }
    }
}

impl DeliveryConsumer for Consumer {
    fn start(&self) -> Result<ConsumerStart, String> {
        if let Err(error) = self.store.identity() {
            let text = error.to_string();
            if text.contains("migration") || text.to_lowercase().contains("schema") {
                return Ok(ConsumerStart::SchemaTooNew);
            }
            return Err(text);
        }
        // The service hands its waiting records right after the acknowledgement, on the same connection, so the
        // listeners are in place before the declaration is sent.
        let inbound = {
            let me = self.me.clone();
            Arc::new(move |event: &DeliveryInboundEvent| {
                let Some(me) = me.upgrade() else {
                    return;
                };
                if let Err(error) = me.on_inbound(event) {
                    me.report(error);
                }
            })
        };
        let holds = {
            let sessions_directory = self.sessions_directory.clone();
            let port = Arc::clone(&self.port);
            let on_error = self.on_error.clone();
            Arc::new(move |event: &DeliveryHoldsEvent| {
                let holds = !find_disk_sessions(&sessions_directory, &event.durable_id, true, &[])
                    .is_empty();
                if let Err(error) = port.held(&event.ask_id, holds)
                    && let Some(sink) = &on_error
                {
                    sink(error.to_string());
                }
            })
        };
        let registered = vec![self.port.on_inbound(inbound), self.port.on_holds(holds)];
        self.unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(registered);
        let ack = match self.port.serve(&self.agent_dir, DELIVERY_PROTOCOL_VERSION) {
            Ok(ack) => ack,
            Err(error) => {
                self.stop();
                if is_unknown_shape(refusal_code(&error)) {
                    return Ok(ConsumerStart::Unsupported);
                }
                return Err(error.to_string());
            }
        };
        if decode_serve_ack(&ack).is_none() {
            self.stop();
            return Ok(ConsumerStart::Unsupported);
        }
        self.serving.store(true, Ordering::SeqCst);
        self.report_pending()?;
        Ok(ConsumerStart::Serving)
    }

    fn serving(&self) -> bool {
        self.serving.load(Ordering::SeqCst)
    }

    fn report_drained(&self) -> Result<(), String> {
        // The refusal notices are best-effort and each may wait on a slow peer, so they run beside the drained report,
        // never ahead of it: startup and the service's marks do not wait on another device.
        if let Some(me) = self.me.upgrade() {
            track(&self.follow, move || me.send_refusal_notices());
        }
        if !self.marks_kept.load(Ordering::SeqCst) {
            return Ok(());
        }
        let marked = self
            .store
            .intake_marks()
            .marked()
            .map_err(|error| error.to_string())?;
        let mut reported = BTreeSet::new();
        for pair in marked {
            let last_command_id = match self.store.intake_marks().drained(&pair) {
                Ok(DrainedMark::Drained { last_command_id }) => last_command_id,
                Ok(DrainedMark::Pending) => continue,
                Err(error) => return Err(error.to_string()),
            };
            let reply = match self.port.drained(&DeliveryPendingPair {
                sender_device: pair.sender_device.clone(),
                session_id: pair.session_id.clone(),
                last_command_id,
            }) {
                Ok(reply) => reply,
                Err(error) => {
                    if is_unknown_shape(refusal_code(&error)) {
                        self.marks_kept.store(false, Ordering::SeqCst);
                        return Ok(());
                    }
                    continue;
                }
            };
            let DrainedReply::Kept { expecting } = reply else {
                continue;
            };
            let open = self
                .store
                .intake_marks()
                .open_for_command(&pair.sender_device, &expecting)
                .map_err(|error| error.to_string())?;
            if open || !reported.insert((pair.sender_device.clone(), expecting.clone())) {
                continue;
            }
            let _ = self.port.drained(&DeliveryPendingPair {
                sender_device: pair.sender_device,
                session_id: pair.session_id,
                last_command_id: expecting,
            });
        }
        Ok(())
    }

    fn idle(&self) {
        wait_idle(&self.follow);
    }

    fn stop(&self) {
        let registered: Vec<PortUnsubscribe> = {
            let mut held = self
                .unsubscribe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *held)
        };
        for off in registered {
            off();
        }
        self.serving.store(false, Ordering::SeqCst);
    }
}

/// Builds the consumer the wake runtime starts (#13037 5.4).
pub fn create_delivery_consumer(options: DeliveryConsumerOptions) -> Arc<dyn DeliveryConsumer> {
    Arc::new_cyclic(|me| Consumer {
        store: options.store,
        port: options.port,
        agent_dir: options.agent_dir,
        sessions_directory: options.sessions_directory,
        chain_count_limit: options.chain_count_limit,
        wake: options.wake,
        on_error: options.on_error,
        follow: Arc::new((Mutex::new(0), Condvar::new())),
        marks_kept: AtomicBool::new(true),
        serving: AtomicBool::new(false),
        unsubscribe: Mutex::new(Vec::new()),
        me: Weak::clone(me),
    })
}
