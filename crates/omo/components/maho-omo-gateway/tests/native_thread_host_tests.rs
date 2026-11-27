//! The thread component as an mhc session mounts it (`thread/native.rs`): a real session through the extension host
//! registers every thread tool, the tools reach this device's session host over the bound wire (create, list, read,
//! fork, send), and the receiver half registers with the session's `pi.session` and admits a queued delivery through it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use maho_ext_host::loader::NativeExtensionFactory;
use maho_omo_gateway::thread::adapter::{BoxFuture, EndpointLiveness, GatewayEndpointPort};
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::engine::{
    GatewayDeliverRequest, GatewayEngine, GatewayEngineOptions, GatewayResolution, GatewayTarget,
};
use maho_omo_gateway::thread::native::{NativeThreadComponent, NativeThreadHost, ThreadHostWire};
use maho_omo_gateway::thread::registration::{
    ControlEndpointDrain, RegisterControlEndpointOptions, SessionControlActionsPort,
    SessionControlRegistration,
};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::types::{GatewayDeliveryResult, GatewaySender, Provenance};
use maho_omo_gateway::{gateway_database_path, gateway_inbox_directory};
use maho_test_support::faux::load_script;
use maho_test_support::faux_session::{FauxSession, NativeSession};

const SOCKET: &str = "/run/native-thread-host.sock";
const CONTROL_SOCKET: &str = "/run/native-thread-control.sock";
const SENDER: &str = "0dd00001-0000-4000-8000-000000000004";
const BOUND: Duration = Duration::from_secs(10);

/// This device's session host, in memory: it opens (and forks) sessions, lists them, answers their messages, takes a
/// wake, and records every command it was sent.
#[derive(Default)]
struct DeviceHost {
    sessions: Mutex<Vec<Value>>,
    records: Mutex<Vec<Value>>,
}

impl DeviceHost {
    fn records(&self, kind: &str) -> Vec<Value> {
        self.records
            .lock()
            .expect("records")
            .iter()
            .filter(|record| record["type"] == kind)
            .cloned()
            .collect()
    }
}

impl RpcTransport for DeviceHost {
    fn request(
        &self,
        socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let ok = |data: Value| json!({ "type": "response", "command": kind, "success": true, "data": data });
        let frame = if socket != SOCKET {
            json!({ "type": "response", "command": kind, "success": false, "error": "no such endpoint" })
        } else {
            match kind.as_str() {
                "get_protocol_info" => ok(json!({ "capabilities": ["open_session_fork"] })),
                "list_sessions" => {
                    ok(json!({ "sessions": self.sessions.lock().expect("sessions").clone() }))
                }
                "open_session" => {
                    let mut sessions = self.sessions.lock().expect("sessions");
                    let routing_id = format!("rpc-{}", sessions.len() + 1);
                    sessions.push(json!({
                        "sessionId": routing_id,
                        "durableSessionId": command["durableSessionId"],
                        "cwd": command["cwd"],
                        "status": "open",
                    }));
                    ok(json!({ "sessionId": routing_id }))
                }
                "set_session_name" => {
                    for row in self.sessions.lock().expect("sessions").iter_mut() {
                        if row["sessionId"] == command["sessionId"] {
                            row["name"] = command["name"].clone();
                        }
                    }
                    json!({ "type": "response", "command": kind, "success": true })
                }
                "get_messages" => {
                    ok(json!({ "messages": [{ "role": "user", "content": "build the parser" }] }))
                }
                "get_state" => ok(json!({ "isStreaming": false })),
                "attach_session" | "release_attachment" => ok(json!({})),
                "wake" => ok(json!({ "admitted": [] })),
                _ => {
                    json!({ "type": "response", "command": kind, "success": false, "error": "unsupported" })
                }
            }
        };
        self.records.lock().expect("records").push(command);
        Box::pin(async move { Ok(frame) })
    }
}

/// The session's `pi.session`: it admits every delivery as a started turn and records what the component asked of it.
#[derive(Default)]
struct SessionControl {
    admitted: Mutex<Vec<Value>>,
    registered: Mutex<Vec<(String, ControlEndpointDrain)>>,
    passes: tokio::sync::Notify,
}

impl SessionControlActionsPort for SessionControl {
    fn admission_gate(&self) -> Value {
        json!({ "can_admit": true, "editor_revision": 0, "turn_epoch": 0 })
    }

    fn admit_external_message(&self, input: Value) -> Result<Value, String> {
        self.admitted.lock().expect("admitted").push(input);
        Ok(json!({ "kind": "started", "turn_epoch": 0 }))
    }

    fn list_admitted_deliveries(&self) -> Value {
        // Every drain pass starts by reading the ledger: a test awaits this signal instead of a clock.
        self.passes.notify_one();
        json!({ "pending": [], "emitted": [] })
    }

    fn persist_header_now(&self) -> BoxFuture<Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn register_control_endpoint(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<SessionControlRegistration> {
        self.registered
            .lock()
            .expect("registered")
            .push((options.inbox_dir, options.drain));
        Box::pin(async {
            SessionControlRegistration::Registered {
                socket: CONTROL_SOCKET.to_owned(),
                dispose: Arc::new(|| Box::pin(async { Ok(()) })),
            }
        })
    }
}

async fn boot(host: &Arc<DeviceHost>, control: &Arc<SessionControl>) -> NativeSession {
    let provided = control.clone();
    let component = NativeThreadComponent {
        host: NativeThreadHost {
            wire: Some(ThreadHostWire {
                socket: SOCKET.to_owned(),
                transport: host.clone(),
            }),
            session_control: Some(Arc::new(move || {
                let control: Arc<dyn SessionControlActionsPort> = provided.clone();
                Some(control)
            })),
        },
    };
    let session = FauxSession::new(load_script("hello").expect("shared faux script"))
        .with_native_extension(NativeExtensionFactory {
            path: "<thread>".into(),
            source_info: Default::default(),
            extension: Box::new(component),
        });
    tokio::time::timeout(BOUND, session.run_native_handle())
        .await
        .expect("the session boots in time")
        .expect("native session")
}

async fn call(live: &NativeSession, name: &str, args: Value) -> Value {
    let result = tokio::time::timeout(BOUND, live.execute_tool(name, args))
        .await
        .expect("the tool answers in time")
        .unwrap_or_else(|error| panic!("{name} did not run: {error}"));
    result.details["result"].clone()
}

#[tokio::test]
async fn a_session_registers_every_thread_tool_and_reaches_its_device_host_over_the_bound_wire() {
    let host = Arc::new(DeviceHost::default());
    let control = Arc::new(SessionControl::default());
    let live = boot(&host, &control).await;

    let registered: Vec<String> = live
        .agent_session()
        .get_all_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    for name in [
        "thread_send",
        "thread_handoff",
        "thread_list",
        "thread_read",
        "thread_interrupt",
        "thread_close",
        "thread_rename",
        "thread_set_model",
        "thread_set_reasoning",
        "thread_archive",
        "thread_create",
    ] {
        assert!(
            registered.iter().any(|tool| tool == name),
            "{name} is not registered: {registered:?}"
        );
    }

    let created = call(&live, "thread_create", json!({ "name": "child" })).await;
    assert_eq!(created["kind"], "ok", "{created}");
    let child = created["thread"]
        .as_str()
        .expect("the created thread")
        .to_owned();
    let opened = host.records("open_session");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["durableSessionId"], child);
    assert_eq!(opened[0]["retain_on_disconnect"], true);

    let listed = call(&live, "thread_list", json!({})).await;
    assert_eq!(listed["kind"], "ok", "{listed}");
    let row = listed["threads"]
        .as_array()
        .expect("threads")
        .iter()
        .find(|row| row["thread"] == child)
        .cloned()
        .unwrap_or_else(|| panic!("the created thread is listed: {listed}"));
    assert_eq!(row["name"], "child");

    let read = call(&live, "thread_read", json!({ "thread": child })).await;
    assert_eq!(read["kind"], "ok", "{read}");
    assert!(
        host.records("get_messages")
            .iter()
            .any(|record| record["sessionId"] == "rpc-1")
    );

    let forked = call(&live, "thread_create", json!({ "fork_from": child })).await;
    assert_eq!(forked["kind"], "ok", "{forked}");
    assert_ne!(forked["thread"], child);
    let opened = host.records("open_session");
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[1]["forkFrom"], child);

    let sent = call(
        &live,
        "thread_send",
        json!({ "thread": child, "message": "status?" }),
    )
    .await;
    assert_eq!(sent["kind"], "ok", "{sent}");
    let woken = host.records("wake");
    assert_eq!(woken.len(), 1, "{woken:?}");
    assert_eq!(woken[0]["sessionId"], "rpc-1");
    assert_eq!(woken[0]["delivery_ids"], json!([sent["delivery_id"]]));
}

#[tokio::test]
async fn the_session_registers_its_control_endpoint_and_admits_a_queued_delivery_through_pi_session()
 {
    let host = Arc::new(DeviceHost::default());
    let control = Arc::new(SessionControl::default());
    let live = boot(&host, &control).await;
    tokio::time::timeout(BOUND, control.passes.notified())
        .await
        .expect("the first drain pass runs once the endpoint is recorded");

    let session = live.agent_session();
    let durable_id = session.session_id();
    let agent_dir = PathBuf::from(session.agent_dir());
    let (inbox, drain) = control
        .registered
        .lock()
        .expect("registered")
        .first()
        .cloned()
        .expect("the session registered its control endpoint");
    assert_eq!(
        PathBuf::from(inbox),
        gateway_inbox_directory(&agent_dir, &durable_id)
    );
    let published: (String, String) = rusqlite::Connection::open(gateway_database_path(&agent_dir))
        .expect("store file")
        .query_row(
            "SELECT endpoint_socket, endpoint_kind FROM session_meta WHERE durable_id = ?1",
            rusqlite::params![durable_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the endpoint is published");
    assert_eq!(
        published,
        (CONTROL_SOCKET.to_owned(), "rpc_host".to_owned())
    );

    let target = durable_id.clone();
    let engine = GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(Arc::new(|| Provenance {
            workspace: "w".to_owned(),
            project: "p".to_owned(),
            lane: None,
            session: None,
        })),
        store: create_gateway_store(GatewayStoreOptions::new(agent_dir.to_string_lossy()))
            .expect("store"),
        endpoints: GatewayEndpointPort::inert(),
        resolve: Arc::new(move |_: &str, _all_scope: bool| {
            let durable_id = target.clone();
            Box::pin(async move {
                GatewayResolution::Ok(GatewayTarget {
                    durable_id,
                    endpoint: None,
                    liveness: EndpointLiveness::Dead,
                })
            })
        }),
        now: None,
    });
    let sent = engine
        .deliver(GatewayDeliverRequest::new(
            GatewaySender::Session {
                durable_id: SENDER.to_owned(),
                name: None,
                turn_id: None,
                cause_delivery_id: None,
            },
            durable_id.as_str(),
            "review the diff",
        ))
        .await;
    let delivery_id = match &sent {
        GatewayDeliveryResult::Ok { delivery_id, .. } => delivery_id.clone(),
        other => panic!("the delivery is queued: {other:?}"),
    };

    let pass = tokio::time::timeout(
        BOUND,
        drain(json!({
            "type": "session_control_wake",
            "reason": "command",
            "reasons": ["command"],
            "delivery_ids": [delivery_id],
        })),
    )
    .await
    .expect("the pass answers in time")
    .expect("the pass runs")
    .expect("the pass reports what it admitted");
    assert_eq!(pass["admitted"][0]["delivery_id"], delivery_id, "{pass}");
    assert_eq!(pass["admitted"][0]["kind"], "started", "{pass}");
    let admitted = control.admitted.lock().expect("admitted").clone();
    assert_eq!(admitted.len(), 1, "{admitted:?}");
    assert_eq!(admitted[0]["delivery_id"], delivery_id);
    assert!(
        matches!(
            admitted[0]["deliverAs"].as_str(),
            Some("steer" | "followUp")
        ),
        "{admitted:?}"
    );
    assert!(
        admitted[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("review the diff")),
        "{admitted:?}"
    );
}
