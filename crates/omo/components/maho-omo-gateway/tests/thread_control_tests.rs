//! Two sessions on one device through the thread tools, over a faux host endpoint (omo v5.1.53..v5.1.79 thread
//! track): list them, steer a running turn with the default delivery, refuse a close that would drop unread
//! deliveries, then force it and read back what it dropped.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::gateway_database_path;
use maho_omo_gateway::thread::adapter::{
    AdmitResult, BoxFuture, DrainWakeEvent, EndpointLiveness, ExternalAdmissionKind,
    GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort, WakeReason,
};
use maho_omo_gateway::thread::address_book::session_directory_prefix;
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
use maho_omo_gateway::thread::live_surface::{
    HostEndpointReport, LiveThreadSurface, LiveThreadSurfaceOptions,
};
use maho_omo_gateway::thread::schema::GATEWAY_MIGRATIONS;
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    AdmissionLedger, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode,
    GatewayDeliveryResult, GatewayEndpointKind, GatewaySender, Provenance, ReceiptRef,
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

struct StatefulControlHost {
    cwd: String,
    records: Mutex<Vec<Value>>,
    model: Mutex<(String, String)>,
    thinking: Mutex<String>,
    reject_level: Option<String>,
}

impl StatefulControlHost {
    fn answer(&self, command: &Value) -> Value {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let ok = |data: Value| json!({ "type": "response", "command": kind, "success": true, "data": data });
        match kind.as_str() {
            "list_sessions" => ok(json!({ "sessions": [
                { "sessionId": "rpc-1", "durableSessionId": BUILDER, "cwd": self.cwd, "name": "builder", "status": "open" },
                { "sessionId": "rpc-2", "durableSessionId": REVIEWER, "cwd": self.cwd, "name": "reviewer", "status": "open" },
            ] })),
            "attach_session" | "release_attachment" => {
                json!({ "type": "response", "command": kind, "success": true })
            }
            "get_protocol_info" => ok(json!({
                "capabilities": ["unload_session", "unload_refuse_deliveries", "unload_graceful"],
            })),
            "get_available_models" => ok(json!({ "models": [
                { "provider": "anthropic", "id": "claude-opus" },
                { "provider": "openai", "id": "gpt-6" },
            ] })),
            "get_state" => {
                let (provider, id) = self.model.lock().expect("model").clone();
                let level = self.thinking.lock().expect("thinking").clone();
                ok(json!({
                    "model": { "provider": provider, "id": id },
                    "thinkingLevel": level,
                }))
            }
            "set_model" => {
                let provider = command["provider"].as_str().unwrap_or_default().to_owned();
                let id = command["modelId"].as_str().unwrap_or_default().to_owned();
                *self.model.lock().expect("model") = (provider.clone(), id.clone());
                ok(json!({ "provider": provider, "id": id }))
            }
            "get_available_thinking_levels" => ok(json!({ "levels": ["off", "medium", "max"] })),
            "set_thinking_level" => {
                let level = command["level"].as_str().unwrap_or_default().to_owned();
                if self.reject_level.as_deref() == Some(level.as_str()) {
                    json!({
                        "type": "response",
                        "command": kind,
                        "success": false,
                        "error": format!("Thinking level {level} is not supported by the active model."),
                    })
                } else {
                    *self.thinking.lock().expect("thinking") = level;
                    json!({ "type": "response", "command": kind, "success": true })
                }
            }
            "unload_session" if command.get("graceful") == Some(&json!(true)) => ok(json!({
                "outcome": "unloaded",
                "dropped": { "deliveries": ["d-queued"], "user_messages": ["draft"] },
                "graceful": { "turn": 4, "waited_ms": 900, "stopped": { "wake_sources": ["terminal"] } },
            })),
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

impl RpcTransport for StatefulControlHost {
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

#[test]
fn graceful_close_and_thinking_suffix_rollback_run_through_live_surface() {
    use maho_omo_gateway::thread::control::locate::{
        LocatedControl, ThreadCall, book_of, locate_control,
    };
    use maho_omo_gateway::thread::control::operations::{
        close_options, close_thread_with_options, set_model_on,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let host = Arc::new(StatefulControlHost {
        cwd: cwd.clone(),
        records: Mutex::new(Vec::new()),
        model: Mutex::new(("anthropic".into(), "claude-opus".into())),
        thinking: Mutex::new("off".into()),
        reject_level: Some("xhigh".into()),
    });
    let surface: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: "/run/maho-test/rpc-stateful.sock".into(),
        transport: host.clone(),
        status_all: None,
        now: None,
    }));
    let now = "2026-11-22T00:00:00Z".to_owned();
    let view = block_on(surface.list_view()).expect("list_view");
    let book = book_of(&view, &[], &now);
    let call = ThreadCall {
        host: surface,
        view,
        book,
        caller_id: BUILDER.into(),
        caller_workspace_root: cwd,
        sessions_directory: None,
        remote: None,
        now,
    };
    let LocatedControl::Control(control) = block_on(locate_control(&call, "reviewer", false))
    else {
        panic!("expected live control");
    };

    let switched = block_on(set_model_on(control.as_ref(), "gpt:max", None)).expect("switch");
    assert_eq!(switched["kind"], "ok", "{switched}");
    assert_eq!(
        switched["model"],
        json!({ "provider": "openai", "id": "gpt-6" })
    );
    assert_eq!(*host.thinking.lock().expect("thinking"), "max");

    let rejected = block_on(set_model_on(control.as_ref(), "opus:xhigh", None)).expect("rejected");
    assert_eq!(rejected["kind"], "error", "{rejected}");
    assert_eq!(rejected["error"]["code"], "thinking_level_unsupported");
    assert_eq!(
        *host.model.lock().expect("model"),
        ("openai".to_owned(), "gpt-6".to_owned())
    );
    assert_eq!(*host.thinking.lock().expect("thinking"), "max");

    let closed = block_on(close_thread_with_options(
        control.as_ref(),
        close_options(None, Some(true), Some(5_000)),
    ))
    .expect("graceful close");
    assert_eq!(closed["kind"], "ok", "{closed}");
    assert_eq!(closed["closed"], true);
    assert_eq!(closed["dropped_deliveries"], json!(["d-queued"]));
    assert_eq!(closed["graceful"]["turn"], 4);
    assert_eq!(closed["graceful"]["waited_ms"], 900);
    let unloads = host.sent("unload_session");
    assert_eq!(unloads.len(), 1);
    assert_eq!(unloads[0]["graceful"], json!(true));
    assert_eq!(unloads[0]["waitMs"], json!(5_000));
    assert!(
        unloads[0]["effectId"]
            .as_str()
            .is_some_and(|id| id.starts_with("omo-thread-close:"))
    );
}

struct MultiEndpointListTransport {
    cwd: String,
}

impl RpcTransport for MultiEndpointListTransport {
    fn request(
        &self,
        socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let reply = match (socket, kind.as_str()) {
            ("/run/maho-test/rpc.sock", "get_protocol_info") => Ok(json!({
                "type": "response",
                "command": "get_protocol_info",
                "success": true,
                "data": { "serverVersion": "5.1.79", "engineVersion": "2026.11.19-3" }
            })),
            ("/run/maho-test/rpc.sock", "list_sessions") => Ok(json!({
                "type": "response",
                "command": "list_sessions",
                "success": true,
                "data": {
                    "sessions": [{
                        "sessionId": "rpc-1",
                        "durableSessionId": BUILDER,
                        "cwd": self.cwd,
                        "status": "open",
                        "activity": "running",
                        "waiting": {
                            "kind": "rate_limit",
                            "until": "2026-11-19T05:00:45.000Z",
                            "attempt": 2,
                            "maxAttempts": 5,
                            "delayMs": 45000
                        }
                    }]
                }
            })),
            ("/run/maho-test/i-shard.sock", "get_protocol_info") => Ok(json!({
                "type": "response",
                "command": "get_protocol_info",
                "success": true,
                "data": { "serverVersion": "5.1.70" }
            })),
            ("/run/maho-test/i-shard.sock", "list_sessions") => Ok(json!({
                "type": "response",
                "command": "list_sessions",
                "success": true,
                "data": {
                    "sessions": [{
                        "sessionId": "rpc-2",
                        "durableSessionId": REVIEWER,
                        "cwd": self.cwd,
                        "name": "reviewer",
                        "status": "open",
                        "activity": "idle"
                    }]
                }
            })),
            ("/run/maho-test/t-slow.sock", "list_sessions") => {
                Err("thread RPC request timed out".to_owned())
            }
            ("/run/maho-test/t-dead.sock", "list_sessions") => {
                Err("host_unavailable:/run/maho-test/t-dead.sock".to_owned())
            }
            _ => Ok(json!({
                "type": "response",
                "command": kind,
                "success": false,
                "error": "unsupported"
            })),
        };
        Box::pin(async move { reply })
    }
}

#[test]
fn thread_list_reports_engine_version_waiting_on_unnamed_archived_incomplete_and_backlog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace-app");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let cwd = workspace.to_string_lossy().into_owned();
    let sessions_dir = dir.path().join("sessions");
    let project_dir = sessions_dir.join(format!("{}-", session_directory_prefix(&cwd)));
    std::fs::create_dir_all(&project_dir).expect("project dir");

    let slow_id = "0cc00001-0000-4000-8000-000000000003";
    let slow_path = project_dir.join(format!("2026-11-19T01-00-00-000Z_{slow_id}.jsonl"));
    std::fs::write(
        &slow_path,
        format!(
            "{}\n{}\n",
            json!({ "type": "session", "version": 3, "id": slow_id, "cwd": cwd, "timestamp": "2026-11-19T01:00:00.000Z" }),
            json!({ "type": "message", "timestamp": "2026-11-19T01:01:00.000Z", "message": { "role": "user", "content": "investigate timeout" } }),
        ),
    )
    .expect("slow file");

    let archived_id = "0dd00001-0000-4000-8000-000000000004";
    let archived_path = project_dir.join(format!("2026-11-19T02-00-00-000Z_{archived_id}.jsonl"));
    std::fs::write(
        &archived_path,
        format!(
            "{}\n{}\n",
            json!({ "type": "session", "version": 3, "id": archived_id, "cwd": cwd, "timestamp": "2026-11-19T02:00:00.000Z" }),
            json!({ "type": "custom", "customType": "session_archived", "data": { "archived": true }, "timestamp": "2026-11-19T02:01:00.000Z" }),
        ),
    )
    .expect("archived file");

    let slow_path_str = slow_path.to_string_lossy().into_owned();
    let reports = vec![
        HostEndpointReport {
            socket: Some("/run/maho-test/rpc.sock".into()),
            reachable: true,
            session_paths: Vec::new(),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
        },
        HostEndpointReport {
            socket: Some("/run/maho-test/i-shard.sock".into()),
            reachable: true,
            session_paths: Vec::new(),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
        },
        HostEndpointReport {
            socket: Some("/run/maho-test/t-slow.sock".into()),
            reachable: false,
            session_paths: vec![slow_path_str.clone()],
            endpoint_kind: Some(GatewayEndpointKind::Tui),
            alive: Some(false),
            reason: Some("live_unresponsive".into()),
        },
        HostEndpointReport {
            socket: Some("/run/maho-test/t-dead.sock".into()),
            reachable: false,
            session_paths: Vec::new(),
            endpoint_kind: Some(GatewayEndpointKind::Tui),
            alive: Some(false),
            reason: Some("dead".into()),
        },
    ];
    let surface: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: "/run/maho-test/rpc.sock".into(),
        transport: Arc::new(MultiEndpointListTransport { cwd: cwd.clone() }),
        status_all: Some(Arc::new(move || Some(reports.clone()))),
        now: None,
    }));

    let store_dir = tempfile::tempdir().expect("store dir");
    let mut store_opts = GatewayStoreOptions::new(store_dir.path().to_string_lossy().into_owned());
    store_opts.now = Some(Arc::new(|| 1_000));
    let gw_store = create_gateway_store(store_opts).expect("store");
    let enqueued = gw_store
        .enqueue(&EnqueueRequest {
            provenance: Provenance {
                workspace: "unclassified".into(),
                project: "test".into(),
                lane: None,
                session: None,
            },
            now: 1_000,
            delivery_id: "d-backlog-1".into(),
            target_durable_id: REVIEWER.into(),
            sender_principal: format!("session:{BUILDER}"),
            rate_principal: None,
            sender_node: BUILDER.into(),
            sender_turn: None,
            cause_delivery_id: None,
            claimed_root_id: None,
            origin: EnvelopeOrigin::Session {
                session: BUILDER.into(),
            },
            actor: BUILDER.into(),
            body: "ping".into(),
            mode: GatewayDeliveryMode::FollowUp,
            expected_turn_id: None,
            binding: None,
            receipt: ReceiptRef {
                idempotency_key: "d-backlog-1".into(),
                args_hash: "d-backlog-1".into(),
            },
            endpoint_kind: None,
            collapse_key: None,
        })
        .expect("enqueue");
    assert!(matches!(enqueued, EnqueueOutcome::Inserted { .. }));

    let cwd_for_root = cwd.clone();
    let comp = create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store: gw_store,
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
        host: Some(surface),
        caller_workspace_root: Arc::new(move || cwd_for_root.clone()),
        sessions_directory: Some(sessions_dir),
        remote: None,
    }));

    let listed = run(&comp, "thread_list", json!({}));
    assert_eq!(listed["kind"], "ok", "{listed}");
    assert_eq!(listed["listing"], "incomplete");
    assert_eq!(listed["unanswered_endpoints"], 1);
    assert!(listed.get("backlog_error").is_none());

    let threads = listed["threads"].as_array().expect("threads");
    let find_row = |id: &str| {
        threads
            .iter()
            .find(|row| row["thread"] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let builder = find_row(BUILDER);
    assert_eq!(builder["name"], "workspace-app");
    assert_eq!(builder["unnamed"], true);
    assert_eq!(builder["engine_version"], "2026.11.19-3");
    assert_eq!(builder["waiting_on"]["kind"], "retry");
    assert_eq!(builder["waiting_on"]["reason"], "rate_limit");
    assert_eq!(builder["waiting_on"]["attempt"], 2);
    assert_eq!(builder["waiting_on"]["max_attempts"], 5);

    let reviewer = find_row(REVIEWER);
    assert_eq!(reviewer["name"], "reviewer");
    assert!(reviewer.get("unnamed").is_none());
    assert_eq!(reviewer["engine_version"], "5.1.70");
    assert_eq!(reviewer["backlog"]["held"], 1);
    assert_eq!(reviewer["backlog"]["waiting_in_gateway"], 1);
    assert_eq!(reviewer["backlog"]["oldest_at"], "1970-01-01T00:00:01.000Z");

    let slow = find_row(slow_id);
    assert_eq!(slow["state"], "inactive");
    assert_eq!(slow["name"], "investigate timeout");
    assert_eq!(slow["unnamed"], true);
    assert_eq!(slow["error_note"], "live_unresponsive");
    assert!(find_row(archived_id).is_null());

    let archived = run(&comp, "thread_list", json!({ "state": ["archived"] }));
    let archived_rows = archived["threads"].as_array().expect("archived");
    assert_eq!(archived_rows.len(), 1);
    assert_eq!(archived_rows[0]["thread"], archived_id);
    assert_eq!(archived_rows[0]["state"], "archived");
    assert_eq!(archived_rows[0]["name"], "workspace-app");
    assert_eq!(archived_rows[0]["unnamed"], true);

    // A newer gateway store version preserves the listing and names backlog_error.
    let other =
        rusqlite::Connection::open(gateway_database_path(store_dir.path())).expect("newer binary");
    other
        .execute_batch(&format!(
            "DELETE FROM gateway_meta WHERE key = 'reader_floor'; PRAGMA user_version = {}",
            GATEWAY_MIGRATIONS.len() + 1
        ))
        .expect("bump schema");
    let broken = run(&comp, "thread_list", json!({}));
    assert_eq!(broken["kind"], "ok", "{broken}");
    assert!(
        broken["backlog_error"]
            .as_str()
            .is_some_and(|message| message.contains("newer"))
    );
    assert_eq!(broken["threads"].as_array().expect("threads").len(), 3);
}
