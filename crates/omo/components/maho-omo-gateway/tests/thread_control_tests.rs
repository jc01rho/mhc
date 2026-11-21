//! Two sessions on one device through the thread tools, over a faux host endpoint (omo v5.1.53..v5.1.79 thread
//! track): list them, steer a running turn with the default delivery, refuse a close that would drop unread
//! deliveries, then force it and read back what it dropped.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::thread::adapter::{
    AdmitResult, BoxFuture, DrainWakeEvent, EndpointLiveness, ExternalAdmissionKind,
    GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort, WakeReason,
};
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::drain::{InboxDrain, InboxDrainOptions};
use maho_omo_gateway::thread::engine::{
    GatewayDeliverRequest, GatewayEngine, GatewayEngineOptions, GatewayResolution, GatewayTarget,
};
use maho_omo_gateway::thread::errors::thread_tool_failure;
use maho_omo_gateway::thread::host_view::ThreadHost;
use maho_omo_gateway::thread::live_surface::{LiveThreadSurface, LiveThreadSurfaceOptions};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    AdmissionLedger, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance,
};

const BUILDER: &str = "0aa00001-0000-4000-8000-000000000001";
const REVIEWER: &str = "0bb00001-0000-4000-8000-000000000002";

/// One rpc host listing both sessions; `reviewer` holds one delivery it has not read.
struct FakeHost {
    cwd: String,
    records: Mutex<Vec<Value>>,
}

impl FakeHost {
    fn answer(&self, command: &Value) -> Value {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let ok = |data: Value| json!({ "type": "response", "command": kind, "success": true, "data": data });
        match kind.as_str() {
            "list_sessions" => ok(json!({ "sessions": [
                {
                    "sessionId": "rpc-1",
                    "durableSessionId": BUILDER,
                    "cwd": self.cwd,
                    "name": "builder",
                    "status": "open",
                    "activity": "running",
                    "model": { "provider": "anthropic", "id": "opus" },
                },
                { "sessionId": "rpc-2", "durableSessionId": REVIEWER, "cwd": self.cwd, "name": "reviewer", "status": "open" },
            ] })),
            "attach_session" | "release_attachment" | "set_session_name" => {
                json!({ "type": "response", "command": kind, "success": true })
            }
            "interrupt" => ok(
                json!({ "interrupted": true, "turnId": "t-4", "background": { "stopped": [], "failed": [] } }),
            ),
            "unload_session" if command.get("deliveries").is_none() => {
                ok(json!({ "outcome": "unloaded", "dropped": { "deliveries": ["d-1"] } }))
            }
            "unload_session" => ok(json!({
                "outcome": "refused",
                "reason": "turn_active",
                "pending": { "deliveries": ["d-1"] },
            })),
            "get_available_models" => ok(json!({ "models": [
                { "provider": "anthropic", "id": "opus" },
                { "provider": "openai", "id": "gpt-6" },
            ] })),
            "set_model" => ok(json!({ "provider": command["provider"], "id": command["modelId"] })),
            _ => {
                json!({ "type": "response", "command": kind, "success": false, "error": "unsupported" })
            }
        }
    }

    fn sent(&self, kind: &str) -> Vec<Value> {
        self.records
            .lock()
            .expect("lock")
            .iter()
            .filter(|record| record["type"] == kind)
            .cloned()
            .collect()
    }
}

impl RpcTransport for FakeHost {
    fn request(
        &self,
        _socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let frame = self.answer(&command);
        self.records.lock().expect("lock").push(command);
        Box::pin(async move { Ok(frame) })
    }
}

fn store(dir: &tempfile::TempDir) -> Arc<dyn GatewayStoreApi> {
    create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store")
}

fn runtime(phase: RuntimePhase, admitted: Arc<Mutex<Vec<Value>>>) -> SessionRuntimePort {
    SessionRuntimePort {
        phase: Arc::new(move || phase),
        admission_gate: Arc::new(|| SessionAdmissionGate {
            can_admit: true,
            hold_reason: None,
            editor_revision: 1,
            turn_epoch: 7,
        }),
        admit_external_message: Arc::new(move |input| {
            admitted
                .lock()
                .expect("lock")
                .push(serde_json::to_value(&input).expect("input"));
            Ok(AdmitResult {
                kind: ExternalAdmissionKind::Steered,
                turn_epoch: 7,
            })
        }),
        list_admitted_deliveries: Arc::new(AdmissionLedger::default),
    }
}

fn component(
    dir: &tempfile::TempDir,
    host: Option<Arc<dyn ThreadHost>>,
    cwd: String,
) -> GatewayThreadComponent {
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store: store(dir),
        runtime: runtime(RuntimePhase::Idle, Arc::new(Mutex::new(Vec::new()))),
        durable_id: BUILDER.into(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| BUILDER.to_owned()),
        caller_name: None,
        caller_provenance: Arc::new(|| Provenance {
            workspace: "unclassified".into(),
            project: "test".into(),
            lane: None,
            session: None,
        }),
        resolve: Arc::new(|_, _| {
            Box::pin(async {
                GatewayResolution::Error(thread_tool_failure("not_found", "unused", "none", None))
            })
        }),
        endpoints: GatewayEndpointPort::inert(),
        locate: Arc::new(|_| Box::pin(async { None })),
        notify: None,
        log: None,
        host,
        caller_workspace_root: Arc::new(move || cwd.clone()),
        sessions_directory: None,
        remote: None,
    }))
}

fn run(component: &GatewayThreadComponent, name: &str, args: Value) -> Value {
    block_on(component.execute(name, "call-1", args).expect("tool"))
}

fn keys(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                found.push(key.clone());
                keys(child, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, found)),
        _ => {}
    }
}

fn faux_device() -> (tempfile::TempDir, Arc<FakeHost>, GatewayThreadComponent) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let host = Arc::new(FakeHost {
        cwd: cwd.clone(),
        records: Mutex::new(Vec::new()),
    });
    let surface: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: "/run/maho-test/rpc.sock".into(),
        transport: host.clone(),
        status_all: None,
        now: None,
    }));
    let component = component(&dir, Some(surface), cwd);
    (dir, host, component)
}

#[test]
fn thread_list_names_each_session_by_thread_with_its_state_and_no_endpoint_internals() {
    let (_dir, _host, component) = faux_device();
    let listed = run(&component, "thread_list", json!({}));
    assert_eq!(listed["kind"], "ok", "{listed}");
    let threads = listed["threads"].as_array().expect("threads");
    let row = |thread: &str| {
        threads
            .iter()
            .find(|row| row["thread"] == thread)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(row(BUILDER)["state"], "running");
    assert_eq!(
        row(BUILDER)["model"],
        json!({ "provider": "anthropic", "id": "opus" })
    );
    assert_eq!(row(REVIEWER)["state"], "idle");
    assert_eq!(row(REVIEWER)["name"], "reviewer");
    let mut found = Vec::new();
    keys(&listed, &mut found);
    for internal in [
        "thread_id",
        "endpoint",
        "socket",
        "routing_id",
        "sessionId",
        "session_path",
    ] {
        assert!(
            !found.iter().any(|key| key.as_str() == internal),
            "{internal} leaked: {listed}"
        );
    }
}

#[test]
fn a_close_that_would_drop_unread_deliveries_is_refused_until_forced() {
    let (_dir, host, component) = faux_device();
    let refused = run(&component, "thread_close", json!({ "thread": "reviewer" }));
    assert_eq!(refused["kind"], "error", "{refused}");
    assert_eq!(refused["error"]["code"], "turn_active");
    assert_eq!(
        refused["error"]["details"]["pending_deliveries"],
        json!(["d-1"])
    );
    assert_eq!(
        refused["error"]["details"]["retry_with"],
        json!({ "force": true })
    );
    let forced = run(
        &component,
        "thread_close",
        json!({ "thread": "reviewer", "force": true }),
    );
    assert_eq!(
        forced,
        json!({
            "kind": "ok",
            "thread": REVIEWER,
            "closed": true,
            "reason": "closed",
            "dropped_deliveries": ["d-1"],
        })
    );
    let unloads = host.sent("unload_session");
    assert_eq!(unloads.len(), 3);
    assert!(unloads.iter().all(|record| record["sessionId"] == "rpc-2"));
    assert!(
        host.sent("attach_session")
            .iter()
            .all(|record| record["durableSessionId"] == REVIEWER)
    );
}

#[test]
fn controls_reach_the_thread_through_its_own_endpoint() {
    let (_dir, _host, component) = faux_device();
    let interrupted = run(
        &component,
        "thread_interrupt",
        json!({ "thread": REVIEWER }),
    );
    assert_eq!(interrupted["kind"], "ok", "{interrupted}");
    assert_eq!(interrupted["interrupted"], json!(true));
    assert_eq!(interrupted["turn_id"], "t-4");
    assert_eq!(interrupted["background"]["status"], "stopped");
    let switched = run(
        &component,
        "thread_set_model",
        json!({ "thread": "reviewer", "model": "gpt" }),
    );
    assert_eq!(
        switched["model"],
        json!({ "provider": "openai", "id": "gpt-6" }),
        "{switched}"
    );
    let taken = run(&component, "thread_rename", json!({ "name": "reviewer" }));
    assert_eq!(taken["error"]["code"], "name_conflict", "{taken}");
    let missing = run(&component, "thread_read", json!({ "thread": "nobody" }));
    assert_eq!(missing["error"]["code"], "not_found", "{missing}");
}

#[test]
fn without_a_host_every_control_answers_host_unavailable_as_data() {
    let dir = tempfile::tempdir().expect("tempdir");
    let component = component(&dir, None, String::new());
    for (tool, args) in [
        ("thread_list", json!({})),
        ("thread_close", json!({})),
        ("thread_interrupt", json!({ "thread": REVIEWER })),
    ] {
        let answer = run(&component, tool, args);
        assert_eq!(
            answer["error"]["code"], "host_unavailable",
            "{tool}: {answer}"
        );
    }
}

#[test]
fn an_auto_send_to_a_running_session_steers_the_turn_its_runtime_reports() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(&dir);
    let engine = GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(Arc::new(|| Provenance {
            workspace: "unclassified".into(),
            project: "test".into(),
            lane: None,
            session: None,
        })),
        store: store.clone(),
        endpoints: GatewayEndpointPort::inert(),
        resolve: Arc::new(|_, _| {
            Box::pin(async {
                GatewayResolution::Ok(GatewayTarget {
                    durable_id: REVIEWER.into(),
                    endpoint: None,
                    liveness: EndpointLiveness::Dead,
                })
            })
        }),
        now: None,
    });
    let sent = block_on(engine.deliver(GatewayDeliverRequest::new(
        GatewaySender::Session {
            durable_id: BUILDER.into(),
            name: None,
            turn_id: None,
            cause_delivery_id: None,
        },
        REVIEWER,
        "look at the failing test",
    )));
    assert!(matches!(sent, GatewayDeliveryResult::Ok { .. }), "{sent:?}");
    let admitted = Arc::new(Mutex::new(Vec::new()));
    let drain = InboxDrain::new(InboxDrainOptions {
        store,
        runtime: runtime(RuntimePhase::MidTurn, admitted.clone()),
        durable_id: REVIEWER.into(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
    });
    drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Start,
            delivery_ids: None,
        })
        .expect("drain");
    let inputs = admitted.lock().expect("lock");
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert_eq!(
        inputs[0]["deliver_as"],
        serde_json::to_value(GatewayDeliveryMode::Steer).expect("mode")
    );
    assert_eq!(inputs[0]["expected_turn_id"], 7);
}
