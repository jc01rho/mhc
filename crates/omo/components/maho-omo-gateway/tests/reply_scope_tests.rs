//! A reply to a consumed delivery's sender (omo `reply-scope.test.ts`, `remote/reply-target-enforce.test.ts`;
//! decision 0027 section 6, amendment A4.3.1 of #13037): the reply allowance answers a send to that sender without
//! `all_scope`, and the send delivers only when the address resolves to that sender.
//!
//! These cases drive the real `thread_send` tool over a faux host whose address book lists the sender in a workspace
//! other than the caller's, so a scoped refusal is exactly what the allowance widens.

use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::thread::adapter::{
    AdmitResult, BoxFuture, EndpointLiveness, ExternalAdmissionKind, GatewayEndpointPort,
    RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::engine::{GatewayResolution, GatewayTarget};
use maho_omo_gateway::thread::errors::thread_tool_failure;
use maho_omo_gateway::thread::host_view::ThreadHost;
use maho_omo_gateway::thread::live_surface::{LiveThreadSurface, LiveThreadSurfaceOptions};
use maho_omo_gateway::thread::reply_permission::ReplySender;
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::types::{AdmissionLedger, Provenance};

const SENDER: &str = "01a1f000-0000-7000-8000-00000000000a";
const OTHER: &str = "01a1f000-0000-7000-8000-00000000000b";

/// A host whose one endpoint lists the sender and an unrelated thread, both in another workspace.
struct ForeignHost {
    sessions: Vec<Value>,
}

impl RpcTransport for ForeignHost {
    fn request(
        &self,
        _socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let frame = if kind == "list_sessions" {
            json!({ "type": "response", "command": kind, "success": true, "data": { "sessions": self.sessions } })
        } else {
            json!({ "type": "response", "command": kind, "success": true })
        };
        Box::pin(async move { Ok(frame) })
    }
}

fn session(durable_id: &str, name: &str, cwd: &str) -> Value {
    json!({
        "sessionId": format!("rpc-{name}"),
        "durableSessionId": durable_id,
        "cwd": cwd,
        "name": name,
        "status": "open",
    })
}

fn runtime() -> SessionRuntimePort {
    SessionRuntimePort {
        phase: Arc::new(|| RuntimePhase::Idle),
        admission_gate: Arc::new(|| SessionAdmissionGate {
            can_admit: true,
            hold_reason: None,
            editor_revision: 1,
            turn_epoch: 1,
        }),
        admit_external_message: Arc::new(|_| {
            Ok(AdmitResult {
                kind: ExternalAdmissionKind::Steered,
                turn_epoch: 1,
            })
        }),
        list_admitted_deliveries: Arc::new(AdmissionLedger::default),
    }
}

fn store(dir: &tempfile::TempDir) -> Arc<dyn maho_omo_gateway::thread::store_api::GatewayStoreApi> {
    create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store")
}

/// A receiver in its own workspace whose address book lists `sessions`; `senders` is the consumed set.
fn receiver(
    dir: &tempfile::TempDir,
    caller_cwd: String,
    sessions: Vec<Value>,
    senders: Arc<Mutex<Vec<ReplySender>>>,
) -> GatewayThreadComponent {
    let known = sessions.clone();
    let scope_root = caller_cwd.clone();
    let host: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: "/run/maho-reply/rpc.sock".into(),
        transport: Arc::new(ForeignHost { sessions }),
        status_all: None,
        now: None,
        enumerations: None,
        registry: None,
    }));
    let targets = {
        let senders = senders.clone();
        Arc::new(move || {
            senders
                .lock()
                .map(|senders| {
                    senders
                        .iter()
                        .map(|sender| sender.session_id.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
    };
    let reply_senders = Arc::new(move || {
        senders
            .lock()
            .map(|senders| senders.clone())
            .unwrap_or_default()
    });
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store: store(dir),
        runtime: runtime(),
        durable_id: "receiver".into(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| "receiver".into()),
        caller_name: None,
        caller_provenance: Arc::new(|| Provenance {
            workspace: "unclassified".into(),
            project: "test".into(),
            lane: None,
            session: None,
        }),
        resolve: Arc::new(move |address, all_scope| {
            let address = address.to_owned();
            let known = known.clone();
            let caller_cwd = scope_root.clone();
            Box::pin(async move {
                for row in &known {
                    let id = row["durableSessionId"].as_str().unwrap_or_default();
                    let name = row["name"].as_str().unwrap_or_default();
                    if id == address || name == address {
                        // The scoped ladder refuses a thread outside the caller's workspace, as the real one does.
                        if !all_scope && row["cwd"].as_str().unwrap_or_default() != caller_cwd {
                            return GatewayResolution::Error(thread_tool_failure(
                                "scope_denied",
                                format!("Thread {id} exists outside this workspace."),
                                "retry with all_scope",
                                None,
                            ));
                        }
                        return GatewayResolution::Ok(GatewayTarget {
                            durable_id: id.to_owned(),
                            endpoint: None,
                            liveness: EndpointLiveness::Dead,
                        });
                    }
                }
                GatewayResolution::Error(thread_tool_failure("not_found", "unused", "none", None))
            })
        }),
        endpoints: GatewayEndpointPort::inert(),
        notify: None,
        log: None,
        host: Some(host),
        caller_workspace_root: Arc::new(move || caller_cwd.clone()),
        sessions_directory: None,
        remote: None,
        caller_reply_targets: targets,
        caller_reply_senders: reply_senders,
    }))
}

fn run(component: &GatewayThreadComponent, name: &str, args: Value) -> Value {
    // A fresh call id per call: it is the idempotency key, and a reused one conflicts on different arguments.
    let call_id = format!(
        "call-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    );
    block_on(component.execute(name, &call_id, args).expect("tool"))
}

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().expect("workspace")
}

#[test]
fn a_reply_to_a_consumed_sender_by_id_is_delivered_without_all_scope() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    let senders = Arc::new(Mutex::new(vec![ReplySender {
        session_id: SENDER.into(),
        name: Some("sender lane".into()),
        device: None,
    }]));
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![session(SENDER, "sender lane", &foreign)],
        senders,
    );
    let sent = run(
        &component,
        "thread_send",
        json!({ "thread": SENDER, "message": "reply" }),
    );
    assert_eq!(sent["kind"], "ok", "{sent}");
    assert_eq!(sent["thread"], SENDER);
}

#[test]
fn a_reply_to_a_consumed_sender_by_its_stated_name_is_delivered() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    let senders = Arc::new(Mutex::new(vec![ReplySender {
        session_id: SENDER.into(),
        name: Some("sender lane".into()),
        device: None,
    }]));
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![session(SENDER, "sender lane", &foreign)],
        senders,
    );
    let sent = run(
        &component,
        "thread_send",
        json!({ "thread": "sender lane", "message": "reply" }),
    );
    assert_eq!(sent["kind"], "ok", "{sent}");
    assert_eq!(sent["thread"], SENDER);
}

#[test]
fn a_reply_grant_reaches_no_other_session_in_that_workspace() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    let senders = Arc::new(Mutex::new(vec![ReplySender {
        session_id: SENDER.into(),
        name: Some("sender lane".into()),
        device: None,
    }]));
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![
            session(SENDER, "sender lane", &foreign),
            session(OTHER, "other lane", &foreign),
        ],
        senders,
    );
    for thread in [OTHER, "other lane"] {
        let sent = run(
            &component,
            "thread_send",
            json!({ "thread": thread, "message": "not a reply" }),
        );
        assert_eq!(sent["kind"], "error", "{sent}");
        assert_eq!(sent["error"]["code"], "scope_denied", "{sent}");
    }
}

#[test]
fn a_reply_by_a_name_another_session_now_carries_is_reply_target_mismatch() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    // The sender renamed itself; another session there now carries its stated name.
    let senders = Arc::new(Mutex::new(vec![ReplySender {
        session_id: SENDER.into(),
        name: Some("sender lane".into()),
        device: None,
    }]));
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![
            session(SENDER, "renamed lane", &foreign),
            session(OTHER, "sender lane", &foreign),
        ],
        senders,
    );
    let sent = run(
        &component,
        "thread_send",
        json!({ "thread": "sender lane", "message": "reply" }),
    );
    assert_eq!(sent["kind"], "error", "{sent}");
    assert_eq!(sent["error"]["code"], "reply_target_mismatch", "{sent}");
    assert_eq!(sent["error"]["details"]["thread"], OTHER);
}

#[test]
fn without_a_consumed_delivery_a_send_outside_the_workspace_is_scope_denied() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![session(SENDER, "sender lane", &foreign)],
        Arc::new(Mutex::new(Vec::new())),
    );
    let sent = run(
        &component,
        "thread_send",
        json!({ "thread": SENDER, "message": "hi" }),
    );
    assert_eq!(sent["kind"], "error", "{sent}");
    assert_eq!(sent["error"]["code"], "scope_denied", "{sent}");
}

#[test]
fn the_allowance_ends_when_the_consumed_sender_is_cleared() {
    let dir = workspace();
    let caller = workspace();
    let caller_cwd = caller.path().to_string_lossy().into_owned();
    let foreign = caller_cwd.clone() + "-other";
    let senders = Arc::new(Mutex::new(vec![ReplySender {
        session_id: SENDER.into(),
        name: Some("sender lane".into()),
        device: None,
    }]));
    let component = receiver(
        &dir,
        caller_cwd.clone(),
        vec![session(SENDER, "sender lane", &foreign)],
        senders.clone(),
    );
    let sent = run(
        &component,
        "thread_send",
        json!({ "thread": SENDER, "message": "reply" }),
    );
    assert_eq!(sent["kind"], "ok", "{sent}");
    // A new message started work after a final answer: the allowance is gone.
    senders.lock().unwrap().clear();
    for thread in [SENDER, "sender lane"] {
        let sent = run(
            &component,
            "thread_send",
            json!({ "thread": thread, "message": "later" }),
        );
        assert_eq!(sent["kind"], "error", "{sent}");
        assert_eq!(sent["error"]["code"], "scope_denied", "{sent}");
    }
}
