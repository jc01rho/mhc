//! Sender half of a delivery.
//!
//! Upstream counterpart: `thread/gateway/engine.ts`: resolve, one `BEGIN IMMEDIATE` transaction that
//! writes the row, its receipt, causal edge, budgets and the target's inbox marker, then a
//! best-effort `wake` of a routable endpoint. The result is read back from the target's row, which
//! only the target's own drain moves out of `queued`.

use std::sync::Arc;

use serde_json::Value;

use super::adapter::{BoxFuture, EndpointLiveness, GatewayEndpointPort, GatewayEndpointRef};
use super::constants::TARGET_MAX_BYTES;
use super::decision::{DeliveryDecision, RefusalKind, TargetState, decide_delivery};
use super::errors::ThreadToolFailure;
use super::provenance_resolver::{ProvenanceRuntime, resolve_provenance};
use super::result::{engine_failure, result_from_row};
use super::store_api::GatewayStoreApi;
use super::store_ops::{ReceiptOutcomeRequest, hash_args};
use super::types::{
    DeliveryEnvelope, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode,
    GatewayDeliveryResult, GatewaySender, Provenance, ReceiptRef,
};

/// The endpoint serving a thread right now, or null when nothing answers for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayTarget {
    pub durable_id: String,
    pub endpoint: Option<GatewayEndpointRef>,
    pub liveness: EndpointLiveness,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GatewayResolution {
    Ok(GatewayTarget),
    Error(ThreadToolFailure),
}

/// Resolves an address through the caller's live-and-disk address book.
pub type GatewayResolve = Arc<dyn Fn(&str, bool) -> BoxFuture<GatewayResolution> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDeliverRequest {
    pub sender: GatewaySender,
    pub target: String,
    pub text: String,
    pub mode: Option<GatewayDeliveryMode>,
    pub expected_turn_id: Option<i64>,
    pub all_scope: bool,
    pub idempotency_key: Option<String>,
    /// Replaces the sender's earlier undelivered delivery to this target under the same key (omo#14428).
    pub collapse_key: Option<String>,
    pub root_id: Option<String>,
}

impl GatewayDeliverRequest {
    pub fn new(sender: GatewaySender, target: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            sender,
            target: target.into(),
            text: text.into(),
            mode: None,
            expected_turn_id: None,
            all_scope: false,
            idempotency_key: None,
            collapse_key: None,
            root_id: None,
        }
    }
}

pub struct GatewayEngineOptions {
    /// Sender-owned context, kept outside every delivery/tool request.
    pub sender_provenance: Option<Arc<dyn Fn() -> Provenance + Send + Sync>>,
    pub store: Arc<dyn GatewayStoreApi>,
    pub endpoints: GatewayEndpointPort,
    pub resolve: GatewayResolve,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

#[derive(Clone)]
pub struct GatewayEngine {
    options: Arc<GatewayEngineOptions>,
}

struct SenderFacts {
    provenance: Provenance,
    principal: String,
    rate_principal: String,
    node: String,
    turn: Option<String>,
    cause: Option<String>,
    actor: String,
    origin: EnvelopeOrigin,
}

fn sender_facts(sender: &GatewaySender, resolved: Provenance) -> SenderFacts {
    let session_label = match sender {
        GatewaySender::Session { name, .. } => name
            .as_ref()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        _ => None,
    };
    let provenance = Provenance {
        session: session_label.or(resolved.session.clone()),
        ..resolved
    };
    match sender {
        GatewaySender::Session {
            durable_id,
            name,
            turn_id,
            cause_delivery_id,
        } => SenderFacts {
            provenance,
            principal: format!("session:{durable_id}"),
            rate_principal: format!("session:{durable_id}"),
            node: durable_id.clone(),
            turn: turn_id.clone(),
            cause: cause_delivery_id.clone(),
            actor: name.clone().unwrap_or_else(|| durable_id.clone()),
            origin: EnvelopeOrigin::Session {
                session: durable_id.clone(),
            },
        },
        GatewaySender::Cli { uid, user } => {
            let principal = format!("cli:{uid}");
            SenderFacts {
                provenance,
                principal: principal.clone(),
                rate_principal: principal.clone(),
                node: principal,
                turn: None,
                cause: None,
                actor: user.clone(),
                origin: EnvelopeOrigin::Session {
                    session: user.clone(),
                },
            }
        }
    }
}

/// The arguments a delivery receipt is keyed by: a retry with the same ones replays, different ones conflict. The hash
/// keeps the `binding: null` and `author: null` every send carried before the binding removal (omo#14312), so a receipt
/// written by the previous build still replays its unchanged retry instead of answering idempotency_conflict (omo#14623
/// review M1).
pub fn delivery_args_hash(
    target: &str,
    text: &str,
    mode: GatewayDeliveryMode,
    expected_turn_id: Option<i64>,
    collapse_key: Option<&str>,
) -> String {
    let mut fields = serde_json::Map::new();
    fields.insert("target".into(), Value::String(target.to_owned()));
    fields.insert("text".into(), Value::String(text.to_owned()));
    fields.insert("mode".into(), Value::String(mode.as_str().to_owned()));
    fields.insert(
        "expected_turn_id".into(),
        expected_turn_id.map_or(Value::Null, Value::from),
    );
    fields.insert("binding".into(), Value::Null);
    fields.insert("author".into(), Value::Null);
    if let Some(collapse_key) = collapse_key {
        fields.insert(
            "collapse_key".into(),
            Value::String(collapse_key.to_owned()),
        );
    }
    hash_args(&Value::Object(fields))
}

fn fail(code: &str, message: impl Into<String>, details: Option<Value>) -> GatewayDeliveryResult {
    GatewayDeliveryResult::Error {
        error: engine_failure(code, message, details),
    }
}

impl GatewayEngine {
    pub fn new(options: GatewayEngineOptions) -> Self {
        Self {
            options: Arc::new(options),
        }
    }

    /// The seams this engine was built with, for a caller that builds a second engine on the same ones.
    pub fn options(&self) -> &GatewayEngineOptions {
        &self.options
    }

    pub fn deliver(&self, request: GatewayDeliverRequest) -> BoxFuture<GatewayDeliveryResult> {
        let engine = self.clone();
        Box::pin(async move { engine.deliver_inner(request).await })
    }

    fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => self.options.store.now(),
        }
    }

    fn sender_provenance(&self) -> Provenance {
        match &self.options.sender_provenance {
            Some(provenance) => provenance(),
            None => resolve_provenance(&ProvenanceRuntime::default()),
        }
    }

    async fn deliver_inner(&self, request: GatewayDeliverRequest) -> GatewayDeliveryResult {
        let mode = request.mode.unwrap_or(GatewayDeliveryMode::Auto);
        if mode == GatewayDeliveryMode::Steer && request.expected_turn_id.is_none() {
            return fail(
                "invalid_arguments",
                "A steer needs expected_turn_id (the target's current turn_epoch).",
                None,
            );
        }
        let bytes = request.text.len();
        if bytes > TARGET_MAX_BYTES {
            return fail(
                "message_too_large",
                format!("The message is {bytes} bytes, above the {TARGET_MAX_BYTES}-byte limit."),
                None,
            );
        }
        let resolved = (self.options.resolve)(&request.target, request.all_scope).await;
        let target = match resolved {
            GatewayResolution::Error(error) => return GatewayDeliveryResult::Error { error },
            GatewayResolution::Ok(target) => target,
        };
        let endpoint = if target.liveness == EndpointLiveness::Routable {
            target.endpoint.clone()
        } else {
            None
        };
        if endpoint.is_none()
            && matches!(
                decide_delivery(TargetState::Offline, mode, request.expected_turn_id, None),
                DeliveryDecision::Refuse { .. }
            )
        {
            return fail(
                "turn_conflict",
                "The target has no live turn to steer.",
                Some(serde_json::json!({ "target": target.durable_id })),
            );
        }
        let facts = sender_facts(&request.sender, self.sender_provenance());
        let delivery_id = super::store_ops::new_id("delivery");
        let idempotency_key = request
            .idempotency_key
            .clone()
            .unwrap_or_else(|| format!("auto:{delivery_id}"));
        let enqueue = EnqueueRequest {
            provenance: facts.provenance.clone(),
            now: self.now(),
            delivery_id: delivery_id.clone(),
            target_durable_id: target.durable_id.clone(),
            sender_principal: facts.principal.clone(),
            rate_principal: Some(facts.rate_principal.clone()),
            sender_node: facts.node.clone(),
            sender_turn: facts.turn.clone(),
            cause_delivery_id: facts.cause.clone(),
            claimed_root_id: request.root_id.clone(),
            origin: facts.origin.clone(),
            actor: facts.actor.clone(),
            body: request.text.clone(),
            mode,
            expected_turn_id: request.expected_turn_id,
            receipt: ReceiptRef {
                idempotency_key: idempotency_key.clone(),
                args_hash: delivery_args_hash(
                    &target.durable_id,
                    &request.text,
                    mode,
                    request.expected_turn_id,
                    request.collapse_key.as_deref(),
                ),
            },
            endpoint_kind: endpoint.as_ref().map(|endpoint| endpoint.kind),
            collapse_key: request.collapse_key.clone(),
        };
        let outcome = match self.options.store.enqueue(&enqueue) {
            Ok(outcome) => outcome,
            Err(error) => return fail("internal_error", error.message(), None),
        };
        let settle = |result: GatewayDeliveryResult| {
            let serialized = serde_json::to_value(&result).unwrap_or(Value::Null);
            let _ = self.options.store.complete_receipt(
                self.now(),
                &facts.principal,
                &idempotency_key,
                &serialized,
            );
            result
        };
        match outcome {
            EnqueueOutcome::Busy => fail(
                "overloaded",
                "The gateway store is locked by a suspended writer; nothing was sent.",
                Some(serde_json::json!({ "budget": "store_lock" })),
            ),
            EnqueueOutcome::Refused {
                code,
                message,
                details,
            } => fail(code.as_str(), message, details),
            EnqueueOutcome::Replay { result } => {
                match serde_json::from_value::<GatewayDeliveryResult>(result) {
                    Ok(GatewayDeliveryResult::Ok {
                        thread_id,
                        delivery_id,
                        message_seq,
                        delivery,
                        effective_mode,
                        endpoint_kind,
                        collapsed,
                        ..
                    }) => {
                        let receipt_outcome =
                            match self.options.store.receipt_outcome(&ReceiptOutcomeRequest {
                                now: self.now(),
                                principal: facts.principal.clone(),
                                delivery_id: delivery_id.clone(),
                                target_durable_id: thread_id.clone(),
                                session_path: None,
                            }) {
                                Ok(receipt_outcome) => receipt_outcome,
                                Err(error) => return fail("internal_error", error.message(), None),
                            };
                        GatewayDeliveryResult::Ok {
                            thread_id,
                            delivery_id,
                            message_seq,
                            delivery,
                            effective_mode,
                            endpoint_kind,
                            deduplicated: true,
                            collapsed,
                            outcome: Some(receipt_outcome),
                        }
                    }
                    Ok(other) => other,
                    Err(error) => fail("internal_error", error.to_string(), None),
                }
            }
            EnqueueOutcome::Collapsed {
                row,
                queue_position,
            } => {
                // The target already holds this sender's earlier message under the collapse key: this text was not
                // delivered, and the answer names the earlier delivery (omo#14431).
                let absorbed = result_from_row(
                    &row,
                    queue_position,
                    endpoint.as_ref().map(|endpoint| endpoint.kind),
                    true,
                    false,
                );
                settle(match absorbed {
                    GatewayDeliveryResult::Ok {
                        thread_id,
                        delivery_id,
                        message_seq,
                        delivery,
                        effective_mode,
                        endpoint_kind,
                        deduplicated,
                        ..
                    } => GatewayDeliveryResult::Ok {
                        thread_id,
                        delivery_id,
                        message_seq,
                        delivery,
                        effective_mode,
                        endpoint_kind,
                        deduplicated,
                        collapsed: true,
                        outcome: None,
                    },
                    other => other,
                })
            }
            EnqueueOutcome::Inserted {
                row,
                queue_position,
            } => {
                let Some(endpoint) = endpoint else {
                    return settle(result_from_row(&row, queue_position, None, false, true));
                };
                let reply =
                    (self.options.endpoints.wake)(endpoint.clone(), vec![delivery_id.clone()])
                        .await;
                let reached = reply.unroutable.is_none();
                match self.options.store.delivery_view(&delivery_id) {
                    Ok(Some(view)) => settle(result_from_row(
                        &view.row,
                        view.queue_position,
                        Some(endpoint.kind),
                        false,
                        !reached,
                    )),
                    Ok(None) => fail(
                        "internal_error",
                        format!("delivery {delivery_id} vanished after commit"),
                        None,
                    ),
                    Err(error) => fail("internal_error", error.message(), None),
                }
            }
        }
    }
}

/// The envelope a stored row carries, for callers that need the provenance back.
pub fn envelope_of(row: &super::types::DeliveryRow) -> DeliveryEnvelope {
    DeliveryEnvelope {
        provenance: row.envelope.provenance.clone().unwrap_or(Provenance {
            workspace: "unclassified".into(),
            project: "unknown".into(),
            lane: None,
            session: None,
        }),
        origin: row.envelope.origin.clone(),
        actor: row.envelope.actor.clone(),
        via: row.envelope.via.clone(),
        root_id: row.envelope.root_id.clone(),
        hop: row.envelope.hop,
        delivery_id: row.envelope.delivery_id.clone(),
    }
}

/// `resolveFromEntries`: the address book's entries behind the engine's resolve seam.
pub fn resolve_from_entries(
    entries: Arc<dyn Fn() -> Vec<GatewayAddressEntry> + Send + Sync>,
    caller_workspace_root: Arc<dyn Fn() -> String + Send + Sync>,
) -> GatewayResolve {
    Arc::new(move |address: &str, _all_scope: bool| {
        let address = address.to_owned();
        let known = entries();
        let _ = caller_workspace_root();
        Box::pin(async move {
            let found = known
                .into_iter()
                .find(|entry| entry.thread_id == address || entry.name == address);
            match found {
                Some(entry) => GatewayResolution::Ok(GatewayTarget {
                    durable_id: entry.thread_id,
                    endpoint: entry.endpoint,
                    liveness: entry.liveness,
                }),
                None => GatewayResolution::Error(engine_failure(
                    "not_found",
                    format!("No thread matches {address}."),
                    None,
                )),
            }
        }) as BoxFuture<GatewayResolution>
    })
}

/// One address-book entry as the engine reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAddressEntry {
    pub thread_id: String,
    pub name: String,
    pub endpoint: Option<GatewayEndpointRef>,
    pub liveness: EndpointLiveness,
}

/// The refusal reason a decision carries, for a caller that needs it alone.
pub fn refusal_reason(decision: &DeliveryDecision) -> Option<RefusalKind> {
    match decision {
        DeliveryDecision::Refuse { reason } => Some(*reason),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::types::GatewayDeliveryDelivery;
    use futures::executor::block_on;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn a_delivery_receipt_hash_changes_with_its_arguments() {
        let left = delivery_args_hash("receiver", "hello", GatewayDeliveryMode::Auto, None, None);
        let right = delivery_args_hash("receiver", "hello", GatewayDeliveryMode::Auto, None, None);
        let other = delivery_args_hash(
            "receiver",
            "hello",
            GatewayDeliveryMode::FollowUp,
            None,
            None,
        );
        let keyed = delivery_args_hash(
            "receiver",
            "hello",
            GatewayDeliveryMode::Auto,
            None,
            Some("alert"),
        );
        assert_eq!(left, right);
        assert_ne!(left, other);
        assert_ne!(left, keyed);
    }

    #[test]
    fn a_steer_without_an_expected_turn_is_refused_before_resolution() {
        let store: Arc<dyn crate::thread::store_api::GatewayStoreApi> =
            crate::thread::store::create_gateway_store(
                crate::thread::store::GatewayStoreOptions::new(
                    std::env::temp_dir()
                        .join("maho-engine-test")
                        .to_string_lossy()
                        .into_owned(),
                ),
            )
            .expect("store");
        let engine = GatewayEngine::new(GatewayEngineOptions {
            sender_provenance: None,
            store,
            endpoints: GatewayEndpointPort::inert(),
            resolve: Arc::new(|_, _| {
                Box::pin(async {
                    GatewayResolution::Error(engine_failure("not_found", "unused", None))
                })
            }),
            now: Some(Arc::new(|| 1)),
        });
        let mut request = GatewayDeliverRequest::new(
            GatewaySender::Session {
                durable_id: "sender".into(),
                name: None,
                turn_id: None,
                cause_delivery_id: None,
            },
            "receiver",
            "hi",
        );
        request.mode = Some(GatewayDeliveryMode::Steer);
        match block_on(engine.deliver(request)) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "invalid_arguments")
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// An engine whose every target is a known session with no live endpoint: each send is queued offline.
    fn offline_engine(dir: &tempfile::TempDir, clock: Arc<AtomicI64>) -> GatewayEngine {
        let store: Arc<dyn crate::thread::store_api::GatewayStoreApi> =
            crate::thread::store::create_gateway_store(
                crate::thread::store::GatewayStoreOptions::new(
                    dir.path().to_string_lossy().into_owned(),
                ),
            )
            .expect("store");
        GatewayEngine::new(GatewayEngineOptions {
            sender_provenance: Some(Arc::new(|| Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            })),
            store,
            endpoints: GatewayEndpointPort::inert(),
            resolve: Arc::new(|target: &str, _all_scope: bool| {
                let durable_id = target.to_owned();
                Box::pin(async move {
                    GatewayResolution::Ok(GatewayTarget {
                        durable_id,
                        endpoint: None,
                        liveness: EndpointLiveness::Dead,
                    })
                })
            }),
            now: Some(Arc::new(move || clock.load(Ordering::Relaxed))),
        })
    }

    fn send(
        from: &str,
        to: &str,
        text: &str,
        turn: Option<&str>,
        key: Option<String>,
    ) -> GatewayDeliverRequest {
        let mut request = GatewayDeliverRequest::new(
            GatewaySender::Session {
                durable_id: from.into(),
                name: None,
                turn_id: turn.map(str::to_owned),
                cause_delivery_id: None,
            },
            to,
            text,
        );
        request.idempotency_key = key;
        request
    }

    #[test]
    fn one_sender_turn_reaches_forty_sessions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = offline_engine(&dir, Arc::new(AtomicI64::new(1_000_000)));
        for index in 1..=40 {
            let target = format!("G{index}");
            match block_on(engine.deliver(send("F", &target, "fan", Some("turn-1"), None))) {
                GatewayDeliveryResult::Ok { delivery, .. } => assert!(
                    matches!(delivery, GatewayDeliveryDelivery::QueuedOffline { .. }),
                    "{target}: {delivery:?}"
                ),
                other => panic!("{target}: expected delivered, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_ninth_burst_send_to_one_session_is_refused_pair_loop_guard_with_the_measured_burst() {
        let dir = tempfile::tempdir().expect("temp dir");
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let engine = offline_engine(&dir, clock.clone());
        for index in 1..=8 {
            let burst = send("A", "B", "burst", None, Some(format!("b{index}")));
            let result = block_on(engine.deliver(burst));
            assert!(
                matches!(result, GatewayDeliveryResult::Ok { .. }),
                "send {index}: {result:?}"
            );
        }
        let ninth = send("A", "B", "burst 9", None, Some("b9".into()));
        let error = match block_on(engine.deliver(ninth)) {
            GatewayDeliveryResult::Error { error } => error,
            other => panic!("expected the 9th send refused, got {other:?}"),
        };
        assert_eq!(error.code.as_str(), "pair_loop_guard");
        assert_eq!(
            error.details,
            Some(
                json!({ "guard": "pair_loop", "burst": 8, "window_ms": 40_000, "retry_after_ms": 5_000 })
            )
        );
        assert!(error.message.contains("session:A"), "{}", error.message);
        clock.fetch_add(5_000, Ordering::Relaxed);
        let retried = block_on(engine.deliver(send("A", "B", "burst 9", None, Some("b9".into()))));
        assert!(
            matches!(retried, GatewayDeliveryResult::Ok { .. }),
            "{retried:?}"
        );
    }

    fn keyed_send(collapse_key: &str, idempotency_key: &str) -> GatewayDeliverRequest {
        let mut request = send(
            "notifier",
            "lead",
            "alert",
            None,
            Some(idempotency_key.into()),
        );
        request.mode = Some(GatewayDeliveryMode::FollowUp);
        request.collapse_key = Some(collapse_key.into());
        request
    }

    /// The delivery id, `deduplicated` and `collapsed` of a delivered answer.
    fn delivered(result: GatewayDeliveryResult) -> (String, bool, bool) {
        match result {
            GatewayDeliveryResult::Ok {
                delivery_id,
                deduplicated,
                collapsed,
                ..
            } => (delivery_id, deduplicated, collapsed),
            other => panic!("expected delivered, got {other:?}"),
        }
    }

    #[test]
    fn a_repeat_the_target_already_took_answers_collapsed_with_the_earlier_delivery() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = offline_engine(&dir, Arc::new(AtomicI64::new(1_000_000)));
        let (first, _, first_collapsed) =
            delivered(block_on(engine.deliver(keyed_send("alert", "k-1"))));
        assert!(!first_collapsed);
        let store = engine.options().store.clone();
        let claimed = store
            .claim(&crate::thread::types::ClaimRequest {
                now: 1_000_000,
                delivery_id: first.clone(),
                self_identity: store.identity().expect("identity"),
                lane: crate::thread::types::EffectiveLane::FollowUp,
            })
            .expect("claim");
        assert!(
            matches!(claimed, crate::thread::types::ClaimOutcome::Claimed { .. }),
            "{claimed:?}"
        );

        let absorbed = delivered(block_on(engine.deliver(keyed_send("alert", "k-2"))));
        assert_eq!(absorbed, (first.clone(), true, true));
        let retried = delivered(block_on(engine.deliver(keyed_send("alert", "k-2"))));
        assert_eq!(retried, (first, true, true));
        assert_eq!(store.list(Some("lead"), None).expect("list").len(), 1);
    }

    #[test]
    fn a_send_a_newer_repeat_superseded_replays_its_refusal_on_retry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = offline_engine(&dir, Arc::new(AtomicI64::new(1_000_000)));
        delivered(block_on(engine.deliver(keyed_send("alert", "k-1"))));
        delivered(block_on(engine.deliver(keyed_send("alert", "k-2"))));
        match block_on(engine.deliver(keyed_send("alert", "k-1"))) {
            GatewayDeliveryResult::Error { error } => {
                assert_eq!(error.code.as_str(), "not_resumable")
            }
            other => panic!("expected the refusal replayed, got {other:?}"),
        }
    }

    #[test]
    fn a_send_to_a_target_its_engine_holds_answers_queued_waiting_on_the_hold() {
        use super::super::adapter::{
            AdmitResult, ExternalAdmissionKind, GatewayEndpointPort, GatewayEndpointRef,
            GatewayWakeReply, RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
        };
        use super::super::drain::{InboxDrain, InboxDrainOptions};
        use super::super::store::{GatewayStoreOptions, create_gateway_store};
        use super::super::types::{AdmissionLedger, GatewayEndpointKind};

        let dir = tempfile::tempdir().expect("temp dir");
        let store = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store");
        let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
            store: store.clone(),
            runtime: SessionRuntimePort {
                phase: Arc::new(|| RuntimePhase::Idle),
                admission_gate: Arc::new(|| SessionAdmissionGate {
                    can_admit: true,
                    hold_reason: None,
                    editor_revision: 1,
                    turn_epoch: 0,
                }),
                admit_external_message: Arc::new(|_| {
                    Ok(AdmitResult {
                        kind: ExternalAdmissionKind::Held,
                        turn_epoch: 0,
                    })
                }),
                list_admitted_deliveries: Arc::new(AdmissionLedger::default),
            },
            durable_id: "held-target".into(),
            session_path: Arc::new(|| None),
            now: None,
            log: None,
            notify: None,
            on_admitted: None,
            admit_intake: None,
            wake_gated: None,
        }));
        let drain_wake = drain.clone();
        let engine = GatewayEngine::new(GatewayEngineOptions {
            sender_provenance: Some(Arc::new(|| Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            })),
            store,
            endpoints: GatewayEndpointPort {
                wake: Arc::new(move |_, ids| {
                    let drain_wake = drain_wake.clone();
                    Box::pin(async move {
                        let res = drain_wake
                            .drain(&super::super::adapter::DrainWakeEvent {
                                reason: super::super::adapter::WakeReason::Inbox,
                                delivery_ids: Some(ids),
                            })
                            .expect("drain");
                        GatewayWakeReply {
                            admitted: res.admitted,
                            unroutable: None,
                        }
                    })
                }),
                release_session: None,
                classify_liveness: None,
                respond_ui: None,
            },
            resolve: Arc::new(|_, _| {
                Box::pin(async {
                    GatewayResolution::Ok(GatewayTarget {
                        durable_id: "held-target".into(),
                        endpoint: Some(GatewayEndpointRef {
                            kind: GatewayEndpointKind::RpcHost,
                            socket: "/run/test.sock".into(),
                            routing_id: Some("r-1".into()),
                            durable_session_id: "held-target".into(),
                        }),
                        liveness: EndpointLiveness::Routable,
                    })
                })
            }),
            now: None,
        });

        let answer = block_on(engine.deliver(GatewayDeliverRequest::new(
            GatewaySender::Session {
                durable_id: "sender".into(),
                name: None,
                turn_id: None,
                cause_delivery_id: None,
            },
            "held-target",
            "are you there?",
        )));
        match answer {
            GatewayDeliveryResult::Ok {
                delivery:
                    GatewayDeliveryDelivery::Queued {
                        queue_position: 1,
                        waiting_on: Some(wait),
                    },
                ..
            } => assert_eq!(wait.kind, "hold"),
            other => panic!("expected queued hold, got {other:?}"),
        }
    }
}
