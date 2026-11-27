//! The tool-receipt wrapper (omo#14623 review H1 and M4, #14060, #14569): a retried call replays its answer instead of
//! acting twice, a receipt the store could not record answers `idempotency_uncertain` on the retry, an admission the store
//! refused runs nothing, and a create left uncertain keeps its first cause for the run that recovers it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::thread::adapter::{
    BoxFuture, GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::control::create::Retrying;
use maho_omo_gateway::thread::engine::GatewayResolution;
use maho_omo_gateway::thread::errors::thread_tool_failure;
use maho_omo_gateway::thread::host_view::ThreadHost;
use maho_omo_gateway::thread::live_surface::{LiveThreadSurface, LiveThreadSurfaceOptions};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::store_ops::StoreError;
use maho_omo_gateway::thread::store_tool_receipts::{
    ToolReceiptBegin, ToolReceiptBeginRequest, ToolReceiptScope, ToolReceiptSettleRequest,
};
use maho_omo_gateway::thread::tool_receipts::{ToolReceiptStore, ToolReceipts};
use maho_omo_gateway::thread::types::{AdmissionLedger, Provenance};

const CALLER: &str = "0aa00001-0000-4000-8000-000000000001";
const PEER: &str = "0bb00001-0000-4000-8000-000000000002";
const SOCKET: &str = "/run/maho-test/peers.sock";

struct PeerHost {
    cwd: String,
    records: Mutex<Vec<Value>>,
    names: AtomicUsize,
}

impl PeerHost {
    fn answer(&self, command: &Value) -> Value {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let ok = |data: Value| json!({ "type": "response", "command": kind, "success": true, "data": data });
        match kind.as_str() {
            "list_sessions" => ok(json!({ "sessions": [
                { "sessionId": "rpc-1", "durableSessionId": PEER, "cwd": self.cwd, "name": "peer", "status": "open" },
            ] })),
            "attach_session" | "release_attachment" => {
                json!({ "type": "response", "command": kind, "success": true })
            }
            "set_session_name" => {
                self.names.fetch_add(1, Ordering::SeqCst);
                json!({ "type": "response", "command": kind, "success": true })
            }
            _ => {
                json!({ "type": "response", "command": kind, "success": false, "error": "unsupported" })
            }
        }
    }
}

impl RpcTransport for PeerHost {
    fn request(
        &self,
        _socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let frame = self.answer(&command);
        self.records.lock().expect("records").push(command);
        Box::pin(async move { Ok(frame) })
    }
}

fn component(dir: &tempfile::TempDir, host: Arc<PeerHost>, cwd: &str) -> GatewayThreadComponent {
    let surface: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: SOCKET.into(),
        transport: host,
        status_all: None,
        now: None,
    }));
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store: create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store"),
        runtime: SessionRuntimePort {
            phase: Arc::new(|| RuntimePhase::Idle),
            admission_gate: Arc::new(|| SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(|_| Err("not in this test".to_owned())),
            list_admitted_deliveries: Arc::new(AdmissionLedger::default),
        },
        durable_id: CALLER.into(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| CALLER.to_owned()),
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
        notify: None,
        log: None,
        host: Some(surface),
        caller_workspace_root: Arc::new(move || cwd.to_owned()),
        sessions_directory: None,
        remote: None,
    }))
}

fn call(component: &GatewayThreadComponent, name: &str, args: Value) -> Value {
    let call_id = format!("call-{}", super::NEXT_CALL.fetch_add(1, Ordering::SeqCst));
    block_on(
        component
            .execute(name, &call_id, args)
            .expect("the tool is registered"),
    )
}

struct Peers {
    _dir: tempfile::TempDir,
    host: Arc<PeerHost>,
    component: GatewayThreadComponent,
}

fn peers() -> Peers {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let host = Arc::new(PeerHost {
        cwd: cwd.clone(),
        records: Mutex::new(Vec::new()),
        names: AtomicUsize::new(0),
    });
    let component = component(&dir, host.clone(), &cwd);
    Peers {
        _dir: dir,
        host,
        component,
    }
}

#[test]
fn a_retried_call_replays_its_answer_and_the_side_effect_runs_once() {
    let peers = peers();
    let args = json!({ "thread": "peer", "name": "Renamed", "idempotency_key": "rename-1" });
    let first = call(&peers.component, "thread_rename", args.clone());
    assert_eq!(first["kind"], "ok", "{first}");
    let retried = call(&peers.component, "thread_rename", args);
    assert_eq!(retried, first);
    assert_eq!(peers.host.names.load(Ordering::SeqCst), 1);
}

#[test]
fn the_same_key_with_different_arguments_conflicts_and_runs_nothing() {
    let peers = peers();
    call(
        &peers.component,
        "thread_rename",
        json!({ "thread": "peer", "name": "First", "idempotency_key": "rename-2" }),
    );
    let conflicted = call(
        &peers.component,
        "thread_rename",
        json!({ "thread": "peer", "name": "Second", "idempotency_key": "rename-2" }),
    );
    assert_eq!(
        conflicted["error"]["code"], "idempotency_conflict",
        "{conflicted}"
    );
    assert_eq!(peers.host.names.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refused_call_releases_its_key_for_a_fixed_retry() {
    let peers = peers();
    let refused = call(
        &peers.component,
        "thread_rename",
        json!({ "thread": "peer", "name": "x".repeat(201), "idempotency_key": "rename-3" }),
    );
    assert_eq!(refused["error"]["code"], "invalid_arguments", "{refused}");
    let retried = call(
        &peers.component,
        "thread_rename",
        json!({ "thread": "peer", "name": "Renamed", "idempotency_key": "rename-3" }),
    );
    assert_eq!(retried["kind"], "ok", "{retried}");
    assert_eq!(peers.host.names.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Copy, Default)]
enum Admission {
    #[default]
    Real,
    BeforeWrite,
    AfterCommit,
}

struct Flaky {
    real: Arc<dyn GatewayStoreApi>,
    admissions: Mutex<VecDeque<Admission>>,
    fail_settle: AtomicBool,
}

impl Flaky {
    fn over(real: &Arc<dyn GatewayStoreApi>) -> Arc<Self> {
        Arc::new(Self {
            real: real.clone(),
            admissions: Mutex::new(VecDeque::new()),
            fail_settle: AtomicBool::new(false),
        })
    }

    fn script(&self, admission: Admission) {
        self.admissions
            .lock()
            .expect("admissions")
            .push_back(admission);
    }
}

impl ToolReceiptStore for Flaky {
    fn receipt_now(&self) -> i64 {
        self.real.now()
    }

    fn begin(&self, request: &ToolReceiptBeginRequest) -> Result<ToolReceiptBegin, StoreError> {
        let scripted = self
            .admissions
            .lock()
            .expect("admissions")
            .pop_front()
            .unwrap_or_default();
        match scripted {
            Admission::Real => self.real.tool_receipt_begin(request),
            Admission::BeforeWrite => Err(StoreError::LockWait(
                "gateway store lock wait exceeded: tool_receipt_begin waited 30000 ms".into(),
            )),
            Admission::AfterCommit => {
                self.real.tool_receipt_begin(request)?;
                Err(StoreError::Message(
                    "the gateway store worker exited (1)".into(),
                ))
            }
        }
    }

    fn settle(&self, request: &ToolReceiptSettleRequest) -> Result<bool, StoreError> {
        if self.fail_settle.swap(false, Ordering::SeqCst) {
            return Err(StoreError::LockWait(
                "gateway store lock wait exceeded: tool_receipt_settle waited 30000 ms".into(),
            ));
        }
        self.real.tool_receipt_settle(request)
    }

    fn release(&self, scope: &ToolReceiptScope) -> Result<bool, StoreError> {
        self.real.tool_receipt_release(scope)
    }
}

fn rename_args(key: &str) -> Value {
    json!({ "thread": "peer", "name": "Renamed", "idempotency_key": key })
}

fn renamed() -> Value {
    json!({ "kind": "ok", "thread": "peer", "name": "Renamed" })
}

fn body(runs: &Arc<AtomicUsize>, answer: Value) -> BoxFuture<Value> {
    let runs = runs.clone();
    Box::pin(async move {
        runs.fetch_add(1, Ordering::SeqCst);
        answer
    })
}

#[test]
fn a_receipt_the_store_could_not_settle_answers_uncertain_on_the_retry_and_runs_the_side_effect_once()
 {
    let (_dir, real) = store();
    let flaky = Flaky::over(&real);
    flaky.fail_settle.store(true, Ordering::SeqCst);
    let receipts = ToolReceipts::new(flaky.clone() as Arc<dyn ToolReceiptStore>);
    let runs = Arc::new(AtomicUsize::new(0));
    let first = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-unsettled"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(first, renamed());
    let retried = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-unsettled"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(
        retried["error"]["code"], "idempotency_uncertain",
        "{retried}"
    );
    assert!(
        retried["error"]["details"]["error_note"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be recorded"),
        "{retried}"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn an_admission_the_store_refused_before_writing_answers_the_lock_and_the_next_call_runs_once() {
    let (_dir, real) = store();
    let flaky = Flaky::over(&real);
    flaky.script(Admission::BeforeWrite);
    let receipts = ToolReceipts::new(flaky.clone() as Arc<dyn ToolReceiptStore>);
    let runs = Arc::new(AtomicUsize::new(0));
    let refused = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-locked"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(refused["error"]["code"], "overloaded", "{refused}");
    let retried = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-locked"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(retried, renamed());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn a_receipt_committed_before_its_reply_was_lost_is_recovered_once_and_replayed_after() {
    let (_dir, real) = store();
    let flaky = Flaky::over(&real);
    flaky.script(Admission::AfterCommit);
    let receipts = ToolReceipts::new(flaky.clone() as Arc<dyn ToolReceiptStore>);
    let runs = Arc::new(AtomicUsize::new(0));
    let lost = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-lost"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(lost["error"]["code"], "internal_error", "{lost}");
    let recovered = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-lost"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(recovered, renamed());
    let replayed = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("rename-lost"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(replayed, renamed());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn a_full_recovery_bound_refuses_a_new_key_until_one_recovered_key_frees_its_slot() {
    let (_dir, real) = store();
    let flaky = Flaky::over(&real);
    let receipts = ToolReceipts::with_recovery_bound(flaky.clone() as Arc<dyn ToolReceiptStore>, 2);
    let runs = Arc::new(AtomicUsize::new(0));
    for key in ["recover-a", "recover-b"] {
        flaky.script(Admission::BeforeWrite);
        let refused =
            block_on(
                receipts.execute("thread_rename", "call-1", CALLER, rename_args(key), {
                    let runs = runs.clone();
                    move |_| body(&runs, renamed())
                }),
            );
        assert_eq!(refused["error"]["code"], "overloaded", "{refused}");
    }
    let bounded = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("fresh-before"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(bounded["error"]["code"], "overloaded", "{bounded}");
    assert_eq!(bounded["error"]["details"]["budget"], "receipt_recovery");
    assert_eq!(bounded["error"]["details"]["max_keys"], 2);
    let recovered = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("recover-a"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(recovered, renamed());
    let fresh = block_on(receipts.execute(
        "thread_rename",
        "call-1",
        CALLER,
        rename_args("fresh-after"),
        {
            let runs = runs.clone();
            move |_| body(&runs, renamed())
        },
    ));
    assert_eq!(fresh, renamed());
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[test]
fn a_create_left_uncertain_keeps_its_first_cause_and_the_recovery_settles_then_replays() {
    let (_dir, real) = store();
    let flaky = Flaky::over(&real);
    let receipts = ToolReceipts::new(flaky.clone() as Arc<dyn ToolReceiptStore>);
    let runs = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let uncertain = json!({
        "kind": "error",
        "error": {
            "code": "idempotency_uncertain",
            "message": "The earlier operation may have been delivered.",
            "next_action": "Read the target transcript before deciding whether to retry.",
            "details": { "reason": "relay_host_unavailable" },
        },
    });
    let mut answers = Vec::new();
    for _ in 0..3 {
        let runs = runs.clone();
        let seen = seen.clone();
        let first = runs.load(Ordering::SeqCst) == 0;
        let answer = if first {
            uncertain.clone()
        } else {
            json!({ "kind": "ok", "thread": "dur-child", "name": "child" })
        };
        answers.push(block_on(receipts.execute(
            "thread_create",
            "call-1",
            CALLER,
            json!({ "name": "child", "idempotency_key": "create-1" }),
            move |retrying: Option<Retrying>| {
                seen.lock()
                    .expect("seen")
                    .push(retrying.and_then(|retrying| retrying.error_note));
                let runs = runs.clone();
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    answer
                })
            },
        )));
    }
    assert_eq!(
        answers[0]["error"]["code"], "idempotency_uncertain",
        "{:?}",
        answers[0]
    );
    assert_eq!(answers[1]["kind"], "ok", "{:?}", answers[1]);
    assert_eq!(answers[1]["thread"], "dur-child");
    assert_eq!(
        answers[2],
        json!({
            "kind": "ok",
            "thread": "dur-child",
            "name": "child",
            "deduplicated": true,
        })
    );
    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), 2, "the replay runs no body");
    assert_eq!(seen[0], None);
    let note = seen[1]
        .as_deref()
        .expect("the recovery keeps its first cause");
    assert!(note.contains("relay_host_unavailable"), "{note}");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

fn store() -> (tempfile::TempDir, Arc<dyn GatewayStoreApi>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn GatewayStoreApi> = create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store");
    (dir, store)
}
