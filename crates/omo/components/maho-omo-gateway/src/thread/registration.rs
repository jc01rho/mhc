//! The receiver half's entry point: the session's own control surface (senpi `pi.session`) as the thread component
//! reads it, and the control endpoint it registers so that another session's delivery reaches this one.
//!
//! Upstream counterpart: `thread/gateway/registration.ts`. As upstream does, the surface is typed structurally here, so
//! the component depends on no engine crate: the host binds it (`native::NativeThreadHost::session_control`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use super::adapter::{
    AdmitExternalMessageInput, AdmitResult, BoxFuture, DrainWakeEvent, RuntimePhase,
    SessionAdmissionGate, SessionRuntimePort, WakeReason, WhenIdle,
};
use super::component::GatewayThreadComponent;
use super::store_api::GatewayStoreApi;
use super::types::{AdmissionLedger, GatewayDeliveryMode, GatewayEndpointKind};

/// One `session_control_wake` pass's drain: the pass's `SessionControlDrainResult`, `None` for `admitted: []`.
pub type ControlEndpointDrain =
    Arc<dyn Fn(Value) -> BoxFuture<Result<Option<Value>, String>> + Send + Sync>;
/// Releases a registered endpoint.
pub type DisposeControlEndpoint = Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>;

/// senpi `RegisterControlEndpointOptions`.
pub struct RegisterControlEndpointOptions {
    /// The directory whose entries wake the drain (`reason: "inbox"`).
    pub inbox_dir: String,
    pub drain: ControlEndpointDrain,
    /// Asked at clean exit: whether the gateway still names this session.
    pub is_session_referenced: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// senpi `SessionControlRegistration`.
pub enum SessionControlRegistration {
    Registered {
        socket: String,
        dispose: DisposeControlEndpoint,
    },
    Unsupported {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

/// The session's `pi.session` (registration.ts `SessionControlActionsPort`), in the engine's own JSON.
pub trait SessionControlActionsPort: Send + Sync {
    fn admission_gate(&self) -> Value;
    fn admit_external_message(&self, input: Value) -> Result<Value, String>;
    fn list_admitted_deliveries(&self) -> Value;
    fn persist_header_now(&self) -> BoxFuture<Result<(), String>>;
    fn register_control_endpoint(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<SessionControlRegistration>;
}

/// senpi's ask_user tool per model family and the flag that makes it wait for the answer (component.ts
/// `ASK_USER_WAIT_FLAGS`): such a call holds the run inside the tool until the user answers, so a steer has no turn to
/// enter.
pub fn waits_for_answer(tool_name: &str, args: &Value) -> bool {
    let flag = match tool_name {
        "ask_user_question" => "waitForAnswer",
        "request_user_input" => "wait_for_answer",
        _ => return false,
    };
    args.get(flag).and_then(Value::as_bool) == Some(true)
}

/// What the session is doing, as the drain's runtime port reads it.
pub struct SessionActivity {
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub is_compacting: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The tool calls of the questions the session waits on ([`waits_for_answer`]).
    pub open_questions: Arc<Mutex<BTreeSet<String>>>,
}

/// The drain's view of the session (registration.ts `runtime`): its phase, and admission through `pi.session`.
pub fn session_runtime_port(
    control: Arc<dyn SessionControlActionsPort>,
    activity: SessionActivity,
) -> SessionRuntimePort {
    let gate = control.clone();
    let admit = control.clone();
    let ledger = control;
    SessionRuntimePort {
        phase: Arc::new(move || {
            if (activity.is_compacting)() {
                RuntimePhase::Compacting
            } else if (activity.is_idle)() {
                RuntimePhase::Idle
            } else if activity
                .open_questions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
            {
                RuntimePhase::MidTurn
            } else {
                RuntimePhase::WaitingQuestion
            }
        }),
        admission_gate: Arc::new(move || admission_gate_of(gate.admission_gate())),
        admit_external_message: Arc::new(move |input: AdmitExternalMessageInput| {
            let answer = admit.admit_external_message(admission_input(&input))?;
            serde_json::from_value::<AdmitResult>(answer.clone()).map_err(|error| {
                format!(
                    "the session answered an admission this build cannot read ({error}): {answer}"
                )
            })
        }),
        list_admitted_deliveries: Arc::new(move || {
            serde_json::from_value::<AdmissionLedger>(ledger.list_admitted_deliveries())
                .unwrap_or_default()
        }),
    }
}

/// The engine's gate; one this build cannot read admits nothing, keeping the engine's hold reason when it gave one.
fn admission_gate_of(gate: Value) -> SessionAdmissionGate {
    serde_json::from_value::<SessionAdmissionGate>(gate.clone()).unwrap_or_else(|_| {
        SessionAdmissionGate {
            can_admit: false,
            hold_reason: Some(
                gate.get("hold_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unreadable_gate")
                    .to_owned(),
            ),
            editor_revision: 0,
            turn_epoch: 0,
        }
    })
}

/// senpi's `admitExternalMessage` input (adapter.ts `AdmitExternalMessageInput`): `deliverAs` and `whenIdle` keep the
/// engine's names.
pub fn admission_input(input: &AdmitExternalMessageInput) -> Value {
    let deliver_as = match input.deliver_as {
        GatewayDeliveryMode::Steer => "steer",
        GatewayDeliveryMode::FollowUp | GatewayDeliveryMode::Auto => "followUp",
    };
    let mut wire = json!({
        "delivery_id": input.delivery_id,
        "text": input.text,
        "deliverAs": deliver_as,
    });
    if let Some(turn) = input.expected_turn_id {
        wire["expected_turn_id"] = json!(turn);
    }
    if let Some(when_idle) = input.when_idle {
        wire["whenIdle"] = json!(match when_idle {
            WhenIdle::Start => "start",
            WhenIdle::Queue => "queue",
        });
    }
    if let Some(sender) = &input.sender {
        wire["sender"] = json!(sender);
    }
    if let Some(display_text) = &input.display_text {
        wire["display_text"] = json!(display_text);
    }
    wire
}

/// senpi's wake as the drain reads it (registration.ts `drainEvent`): a reason the drain does not take is `inbox`.
fn drain_event(event: &Value) -> DrainWakeEvent {
    let reason = event
        .get("reason")
        .cloned()
        .and_then(|reason| serde_json::from_value::<WakeReason>(reason).ok());
    DrainWakeEvent {
        reason: match reason {
            Some(WakeReason::Start) | None => WakeReason::Inbox,
            Some(reason) => reason,
        },
        delivery_ids: event
            .get("delivery_ids")
            .cloned()
            .and_then(|ids| serde_json::from_value::<Vec<String>>(ids).ok()),
    }
}

/// endpoint-registry.ts `endpointKindOf(socket, [])`: a terminal's control socket is named `t-<16 hex digits>.sock`,
/// every other endpoint is a host's.
pub fn endpoint_kind_of(socket: &str) -> GatewayEndpointKind {
    let name = Path::new(socket)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(socket);
    let terminal = name
        .strip_prefix("t-")
        .and_then(|rest| rest.strip_suffix(".sock"))
        .is_some_and(|hash| {
            hash.len() == 16
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
    if terminal {
        GatewayEndpointKind::Tui
    } else {
        GatewayEndpointKind::RpcHost
    }
}

/// A registered control endpoint, released when its session ends.
pub struct ControlEndpoint {
    incarnation: String,
    dispose: DisposeControlEndpoint,
}

impl ControlEndpoint {
    /// Disposes the endpoint first (senpi's clean exit asks whether the session is referenced then) and clears the
    /// published endpoint last.
    pub async fn release(self, component: &GatewayThreadComponent) -> Result<(), String> {
        let disposed = (self.dispose)().await;
        component
            .stop(&self.incarnation)
            .map_err(|error| error.to_string())?;
        disposed
    }
}

/// Registers `component`'s drain as the session's control endpoint (registration.ts `register`). The header is persisted
/// first, so the durable id is on disk before the endpoint is visible; the incarnation is recorded once the engine
/// registered the endpoint, and no pass runs before it is (an earlier wake answers `admitted: []`, and the first pass
/// after the record takes what it would have). `None` when the session's mode serves no endpoint (print mode), or the
/// registration failed, which `log` reports.
pub async fn register_control_endpoint(
    control: Arc<dyn SessionControlActionsPort>,
    component: Arc<GatewayThreadComponent>,
    agent_dir: &Path,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Option<ControlEndpoint> {
    let durable_id = component.durable_id.clone();
    if let Err(reason) = control.persist_header_now().await {
        log(&format!(
            "thread gateway: the session header could not be persisted, so the session is not registered: {reason}"
        ));
        return None;
    }
    let recorded = Arc::new(AtomicBool::new(false));
    let drain = {
        let recorded = recorded.clone();
        let drain = component.drain.clone();
        Arc::new(
            move |event: Value| -> BoxFuture<Result<Option<Value>, String>> {
                let ready = recorded.load(Ordering::SeqCst);
                let drain = drain.clone();
                Box::pin(async move {
                    if !ready {
                        return Ok(None);
                    }
                    drain
                        .drain(&drain_event(&event))
                        .map(|pass| serde_json::to_value(pass.session_control_result()).ok())
                        .map_err(|error| error.to_string())
                })
            },
        )
    };
    let referenced = component.drain.clone();
    let registration = control
        .register_control_endpoint(RegisterControlEndpointOptions {
            inbox_dir: crate::paths::gateway_inbox_directory(agent_dir, &durable_id)
                .to_string_lossy()
                .into_owned(),
            drain,
            is_session_referenced: Arc::new(move || referenced.is_session_referenced()),
        })
        .await;
    let (socket, dispose) = match registration {
        SessionControlRegistration::Registered { socket, dispose } => (socket, dispose),
        SessionControlRegistration::Unsupported { .. } => return None,
        SessionControlRegistration::Failed { reason } => {
            log(&format!(
                "thread gateway: the control endpoint for {durable_id} failed to register: {reason}"
            ));
            return None;
        }
    };
    let incarnation = uuid::Uuid::new_v4().to_string();
    if let Err(error) = GatewayStoreApi::register_incarnation(
        component.store.as_ref(),
        &durable_id,
        &incarnation,
        Some(&socket),
        Some(endpoint_kind_of(&socket).as_str()),
    ) {
        log(&format!(
            "thread gateway: the session incarnation for {durable_id} could not be recorded, so the session is not registered: {error}"
        ));
        if let Err(disposal) = dispose().await {
            log(&format!(
                "thread gateway: the control endpoint for {durable_id} was not disposed: {disposal}"
            ));
        }
        return None;
    }
    recorded.store(true, Ordering::SeqCst);
    if let Err(error) = component.drain.drain(&DrainWakeEvent {
        reason: WakeReason::Start,
        delivery_ids: None,
    }) {
        log(&format!(
            "thread gateway: the first drain pass for {durable_id} failed: {error}"
        ));
    }
    Some(ControlEndpoint {
        incarnation,
        dispose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::adapter::DeliverySender;

    #[test]
    fn an_admission_reaches_the_engine_under_its_own_field_names() {
        let input = AdmitExternalMessageInput {
            delivery_id: "d-1".into(),
            text: "hello".into(),
            deliver_as: GatewayDeliveryMode::FollowUp,
            expected_turn_id: Some(3),
            when_idle: Some(WhenIdle::Queue),
            sender: Some(DeliverySender::Agent {
                session_id: "s-1".into(),
                name: None,
            }),
            display_text: None,
        };
        assert_eq!(
            admission_input(&input),
            json!({
                "delivery_id": "d-1",
                "text": "hello",
                "deliverAs": "followUp",
                "expected_turn_id": 3,
                "whenIdle": "queue",
                "sender": { "kind": "agent", "session_id": "s-1" },
            })
        );
    }

    #[test]
    fn a_wake_reason_the_drain_does_not_take_reads_as_inbox() {
        let wake = drain_event(&json!({ "reason": "command", "delivery_ids": ["d-1"] }));
        assert_eq!(wake.reason, WakeReason::Command);
        assert_eq!(wake.delivery_ids, Some(vec!["d-1".to_owned()]));
        assert_eq!(
            drain_event(&json!({ "reason": "start" })).reason,
            WakeReason::Inbox
        );
        assert_eq!(
            drain_event(&json!({ "reason": "sigcont" })).reason,
            WakeReason::Inbox
        );
    }

    #[test]
    fn a_terminal_socket_is_told_from_a_host_socket_by_its_name() {
        assert_eq!(
            endpoint_kind_of("/home/u/.omo/agent/rpc/tui/t-0123456789abcdef.sock"),
            GatewayEndpointKind::Tui
        );
        assert_eq!(
            endpoint_kind_of("/home/u/.omo/agent/rpc/rpc.sock"),
            GatewayEndpointKind::RpcHost
        );
        assert_eq!(
            endpoint_kind_of("/run/t-0123456789ABCDEF.sock"),
            GatewayEndpointKind::RpcHost
        );
    }

    #[test]
    fn an_unreadable_gate_admits_nothing_and_keeps_the_hold_reason() {
        let gate =
            admission_gate_of(json!({ "can_admit": false, "hold_reason": "session disposed" }));
        assert!(!gate.can_admit);
        assert_eq!(gate.hold_reason.as_deref(), Some("session disposed"));
        let read =
            admission_gate_of(json!({ "can_admit": true, "editor_revision": 2, "turn_epoch": 5 }));
        assert!(read.can_admit);
        assert_eq!(read.turn_epoch, 5);
    }

    #[test]
    fn only_a_question_that_waits_for_its_answer_holds_the_run() {
        assert!(waits_for_answer(
            "ask_user_question",
            &json!({ "waitForAnswer": true })
        ));
        assert!(waits_for_answer(
            "request_user_input",
            &json!({ "wait_for_answer": true })
        ));
        assert!(!waits_for_answer("ask_user_question", &json!({})));
        assert!(!waits_for_answer("bash", &json!({ "waitForAnswer": true })));
    }
}
