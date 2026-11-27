//! Cross-device `thread_list`, remote graceful `thread_close`, `closed_rows`, `reply_on_device` and
//! manual-clock `AttachPool` integration tests over [`RemoteThreadSurface`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::thread::adapter::{
    AdmitResult, BoxFuture, ExternalAdmissionKind, GatewayEndpointPort, RuntimePhase,
    SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::address_book::{
    AddressBookHost, HostListSessions, session_directory_prefix,
};
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::control::close_ladder::REMOTE_GRACEFUL_MAX_WAIT_MS;
use maho_omo_gateway::thread::control::relay_control::RelayControlLocator;
use maho_omo_gateway::thread::engine::GatewayResolution;
use maho_omo_gateway::thread::errors::thread_tool_failure;
use maho_omo_gateway::thread::host_view::{
    SessionCommandPort, ThreadHost, ThreadHostSession, ThreadHostView,
};
use maho_omo_gateway::thread::remote::address_book::{listing_incomplete, reply_on_device};
use maho_omo_gateway::thread::remote::attach_pool::{
    ATTACH_IDLE_MS, AttachClock, AttachPool, AttachSchedule, AttachTask, AttachTransport,
    AttachedClient, CATALOG_MAX_AGE_MS, CancelAttachTimer,
};
use maho_omo_gateway::thread::remote::closed_rows::with_closed_rows;
use maho_omo_gateway::thread::remote::ports::{
    AliasError, AliasHolder, AliasRegistry, ClientDeviceRow, RemoteClient, RemoteCommandOutcome,
    RemoteDevice, RemoteFailure, RemoteListing, RemoteLocateOutcome, RemoteLookupOutcome,
    RemoteSessionRow, RemoteSessionsPort, RemoteThreadSurface, ServiceStatus,
    SessionCommandOptions,
};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{AdmissionLedger, GatewayEndpointKind, Provenance};

const LOCAL_ID: &str = "0aa00001-0000-4000-8000-000000000001";
const B_ID: &str = "0bb00001-0000-4000-8000-000000000002";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct StubHost {
    view: Mutex<ThreadHostView>,
}

struct NoSessionCommand;

impl SessionCommandPort for NoSessionCommand {
    fn send(&self, _command: Value) -> BoxFuture<Result<Value, String>> {
        Box::pin(async { Err("unused".to_owned()) })
    }
}

impl ThreadHost for StubHost {
    fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>> {
        let view = lock(&self.view).clone();
        Box::pin(async move { Ok(view) })
    }

    fn session_port(&self, _session: &ThreadHostSession) -> Arc<dyn SessionCommandPort> {
        Arc::new(NoSessionCommand)
    }
}

struct ScriptedRemote {
    status: Mutex<Result<ServiceStatus, RemoteFailure>>,
    devices: Mutex<Result<Vec<RemoteDevice>, String>>,
    listing: Mutex<Result<RemoteListing, String>>,
    device_listings: Mutex<VecDeque<Result<RemoteListing, String>>>,
    lookup_replies: Mutex<VecDeque<RemoteLookupOutcome>>,
    command_replies: Mutex<VecDeque<RemoteCommandOutcome>>,
    sent_commands: Mutex<Vec<(String, String, Value, SessionCommandOptions)>>,
    device_listing_calls: Mutex<usize>,
    wake_enabled: Mutex<bool>,
    lookup_enabled: Mutex<bool>,
    aliases: Mutex<Result<BTreeMap<String, String>, AliasError>>,
}

impl ScriptedRemote {
    fn new(devices: Vec<RemoteDevice>, rows: Vec<RemoteSessionRow>) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(Ok(ServiceStatus {
                state: "connected".to_owned(),
                device_id: Some("here".to_owned()),
            })),
            devices: Mutex::new(Ok(devices)),
            listing: Mutex::new(Ok(RemoteListing {
                rows,
                incomplete: Vec::new(),
                devices: None,
            })),
            device_listings: Mutex::new(VecDeque::new()),
            lookup_replies: Mutex::new(VecDeque::new()),
            command_replies: Mutex::new(VecDeque::new()),
            sent_commands: Mutex::new(Vec::new()),
            device_listing_calls: Mutex::new(0),
            wake_enabled: Mutex::new(true),
            lookup_enabled: Mutex::new(true),
            aliases: Mutex::new(Ok(BTreeMap::new())),
        })
    }
}

impl RemoteClient for ScriptedRemote {
    fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
        let res = lock(&self.devices)
            .as_ref()
            .map(|devs| {
                devs.iter()
                    .map(|d| ClientDeviceRow {
                        id: d.device.clone(),
                        name: d.label.clone(),
                        online: d.online,
                        is_self: d.device == "here",
                        last_seen_ms: d.last_seen_ms,
                    })
                    .collect()
            })
            .map_err(Clone::clone);
        Box::pin(async move { res })
    }

    fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
        let res = lock(&self.status).clone();
        Box::pin(async move { res })
    }
}

impl RemoteSessionsPort for ScriptedRemote {
    fn this_device(&self) -> BoxFuture<Option<String>> {
        Box::pin(async { Some("here".to_owned()) })
    }

    fn this_device_name(&self) -> BoxFuture<Option<String>> {
        Box::pin(async { Some("This Machine".to_owned()) })
    }

    fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
        let res = lock(&self.devices).clone();
        Box::pin(async move { res })
    }

    fn sessions(&self, device: Option<String>) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
        let res = lock(&self.listing)
            .as_ref()
            .map(|listed| {
                listed
                    .rows
                    .iter()
                    .filter(|row| device.as_ref().is_none_or(|d| *d == row.device))
                    .cloned()
                    .collect()
            })
            .map_err(Clone::clone);
        Box::pin(async move { res })
    }

    fn listing(&self, device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
        if let Some(wanted) = device {
            *lock(&self.device_listing_calls) += 1;
            if let Some(next) = lock(&self.device_listings).pop_front() {
                return Box::pin(async move { next });
            }
            let res = lock(&self.listing)
                .as_ref()
                .map(|listed| RemoteListing {
                    rows: listed
                        .rows
                        .iter()
                        .filter(|row| row.device == wanted)
                        .cloned()
                        .collect(),
                    incomplete: listed
                        .incomplete
                        .iter()
                        .filter(|id| *id == &wanted)
                        .cloned()
                        .collect(),
                    devices: listed.devices.clone(),
                })
                .map_err(Clone::clone);
            return Box::pin(async move { res });
        }
        let res = lock(&self.listing).clone();
        Box::pin(async move { res })
    }

    fn locate(&self, device: String, engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
        let found = lock(&self.listing).as_ref().ok().and_then(|listed| {
            listed
                .rows
                .iter()
                .find(|row| {
                    row.device == device && row.durable_id.as_deref() == Some(engine_id.as_str())
                })
                .map(|row| row.session_id.clone())
        });
        Box::pin(async move {
            match found {
                Some(session_id) => RemoteLocateOutcome::Located { session_id },
                None => RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                },
            }
        })
    }

    fn lookup(&self, _device: String, _durable_id: String) -> BoxFuture<RemoteLookupOutcome> {
        let enabled = *lock(&self.lookup_enabled);
        if !enabled {
            return Box::pin(async { RemoteLookupOutcome::Unavailable });
        }
        let next = lock(&self.lookup_replies)
            .pop_front()
            .unwrap_or(RemoteLookupOutcome::NotHeld);
        Box::pin(async move { next })
    }

    fn can_lookup(&self) -> BoxFuture<bool> {
        let enabled = *lock(&self.lookup_enabled);
        Box::pin(async move { enabled })
    }

    fn can_wake(&self) -> BoxFuture<bool> {
        let enabled = *lock(&self.wake_enabled);
        Box::pin(async move { enabled })
    }

    fn session_command(
        &self,
        device: String,
        session_id: String,
        rpc: Value,
        options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome> {
        lock(&self.sent_commands).push((device, session_id, rpc, options));
        let next =
            lock(&self.command_replies)
                .pop_front()
                .unwrap_or_else(|| RemoteCommandOutcome::Ok {
                    response: json!({ "success": true, "data": {} }),
                    local: false,
                    located: None,
                });
        Box::pin(async move { next })
    }
}

impl AliasRegistry for ScriptedRemote {
    fn resolve(&self, _alias: String) -> BoxFuture<Result<AliasHolder, AliasError>> {
        Box::pin(async {
            Err(AliasError {
                code: "not_found".to_owned(),
                message: "none".to_owned(),
            })
        })
    }

    fn aliases_of(
        &self,
        _thread_ids: Vec<String>,
    ) -> BoxFuture<Result<BTreeMap<String, String>, AliasError>> {
        let res = lock(&self.aliases).clone();
        Box::pin(async move { res })
    }
}

fn make_component(
    dir: &tempfile::TempDir,
    sessions_dir: Option<std::path::PathBuf>,
    view: ThreadHostView,
    cwd: String,
    remote_backend: Arc<ScriptedRemote>,
) -> GatewayThreadComponent {
    let store: Arc<dyn GatewayStoreApi> = create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store");
    let surface = Arc::new(RemoteThreadSurface::new(
        remote_backend.clone(),
        remote_backend.clone(),
        Some(remote_backend),
    ));
    let locator = Arc::new(RelayControlLocator::new(surface));
    let runtime = SessionRuntimePort {
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
    };
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store,
        runtime,
        durable_id: LOCAL_ID.to_owned(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| LOCAL_ID.to_owned()),
        caller_name: None,
        caller_provenance: Arc::new(|| Provenance {
            workspace: "unclassified".to_owned(),
            project: "test".to_owned(),
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
        host: Some(Arc::new(StubHost {
            view: Mutex::new(view),
        })),
        caller_workspace_root: Arc::new(move || cwd.clone()),
        sessions_directory: sessions_dir,
        remote: Some(locator),
    }))
}

fn run(component: &GatewayThreadComponent, name: &str, args: Value) -> Value {
    let call_id = format!(
        "call-{}",
        super::NEXT_CALL.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    block_on(component.execute(name, &call_id, args).expect("tool"))
}

fn local_session(id: &str, cwd: &str) -> ThreadHostSession {
    ThreadHostSession::from_listing(
        &json!({
            "sessionId": "rpc-local",
            "durableSessionId": id,
            "cwd": cwd,
            "name": "builder",
            "status": "open",
            "activity": "idle",
        }),
        "/run/local.sock",
        GatewayEndpointKind::RpcHost,
        &[],
    )
    .expect("session")
}

#[test]
fn thread_list_merges_remote_rows_tracks_incomplete_and_dropped_and_remote_graceful_close_stamps_closed_rows()
 {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("app");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    let cwd_str = cwd.to_string_lossy().into_owned();

    let sessions_dir = dir.path().join("sessions");
    let proj_dir = sessions_dir.join(format!("{}-", session_directory_prefix(&cwd_str)));
    std::fs::create_dir_all(&proj_dir).expect("mkdir proj");
    for i in 0..202usize {
        let id = format!("0195f0c2-1a2b-7c3d-8e4f-{i:012x}");
        let path = proj_dir.join(format!("2026-11-26T00-00-00Z_{id}.jsonl"));
        let header = json!({
            "type": "session",
            "version": 3,
            "id": id,
            "timestamp": "2026-11-26T00:00:00Z",
            "cwd": cwd_str,
        });
        std::fs::write(&path, format!("{header}\n")).expect("write session");
    }

    let remote = ScriptedRemote::new(
        vec![
            RemoteDevice {
                device: "b".to_owned(),
                label: "Laptop\u{202e}".to_owned(),
                online: true,
                platform: Some("darwin".to_owned()),
                kind: Some("laptop".to_owned()),
                last_seen_ms: Some(1_700_000_000_000),
            },
            RemoteDevice {
                device: "c".to_owned(),
                label: "Box C".to_owned(),
                online: true,
                platform: Some("linux".to_owned()),
                kind: None,
                last_seen_ms: None,
            },
            RemoteDevice {
                device: "d".to_owned(),
                label: "Box D".to_owned(),
                online: false,
                platform: None,
                kind: None,
                last_seen_ms: Some(1_699_000_000_000),
            },
        ],
        vec![
            RemoteSessionRow {
                device: "b".to_owned(),
                session_id: "rk-b".to_owned(),
                durable_id: Some(B_ID.to_owned()),
                title: "Remote\nTitle".to_owned(),
                project: "/work/b".to_owned(),
                path: "/work/b\u{202e}".to_owned(),
                state: "running".to_owned(),
                updated_ms: 1_000,
                ..RemoteSessionRow::default()
            },
            RemoteSessionRow {
                device: "b".to_owned(),
                session_id: "rk-anon".to_owned(),
                durable_id: None,
                title: "".to_owned(),
                project: "/work/anon".to_owned(),
                path: "unknown".to_owned(),
                state: "idle".to_owned(),
                updated_ms: 1_000,
                ..RemoteSessionRow::default()
            },
            RemoteSessionRow {
                device: "here".to_owned(),
                session_id: "rk-dup".to_owned(),
                durable_id: Some(LOCAL_ID.to_owned()),
                title: "Shadow".to_owned(),
                project: cwd_str.clone(),
                path: cwd_str.clone(),
                state: "idle".to_owned(),
                updated_ms: 1_000,
                ..RemoteSessionRow::default()
            },
        ],
    );
    lock(&remote.listing).as_mut().expect("listing").devices = Some({
        let mut states = BTreeMap::new();
        states.insert("b".to_owned(), "answered".to_owned());
        states.insert("c".to_owned(), "timeout".to_owned());
        states
    });
    *lock(&remote.aliases) = Ok({
        let mut map = BTreeMap::new();
        map.insert(B_ID.to_owned(), "b-alias".to_owned());
        map
    });
    *lock(&remote.command_replies) = VecDeque::from([
        RemoteCommandOutcome::Ok {
            response: json!({
                "success": true,
                "data": { "capabilities": ["unload_session", "unload_graceful"] }
            }),
            local: false,
            located: None,
        },
        RemoteCommandOutcome::Ok {
            response: json!({
                "success": true,
                "data": {
                    "outcome": "unloaded",
                    "sessionPath": "/b/dur-b.jsonl",
                    "effect": { "updatedAt": "2026-11-26T00:00:02.000Z" },
                    "graceful": { "turn": 5, "waited_ms": 250, "stopped": { "wake_sources": ["terminal"] } },
                    "dropped": { "deliveries": [], "user_messages": [] }
                }
            }),
            local: false,
            located: None,
        },
        RemoteCommandOutcome::Refused {
            reason: "free\u{202e}error\n\x1b[31m".to_owned(),
            detail: None,
        },
    ]);

    let view = ThreadHostView {
        sessions: vec![local_session(LOCAL_ID, &cwd_str)],
        hosts: vec![AddressBookHost {
            socket: "/run/unanswered.sock".to_owned(),
            list_sessions: Some(HostListSessions::failed("timeout")),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: None,
            reason: Some("timeout".to_owned()),
            legacy: false,
        }],
        disk: Vec::new(),
    };
    let component = make_component(&dir, Some(sessions_dir), view, cwd_str, remote.clone());

    let listed = run(
        &component,
        "thread_list",
        json!({ "all_scope": true, "state": ["running", "idle", "inactive"] }),
    );
    assert_eq!(listed["kind"], "ok", "{listed}");
    assert_eq!(listed["listing"], "incomplete");
    assert_eq!(listed["unanswered_endpoints"], 1);
    assert_eq!(listed["inactive_dropped"], 2);

    let threads = listed["threads"].as_array().expect("threads");
    let b_row = threads.iter().find(|t| t["thread"] == B_ID).expect("b row");
    assert_eq!(b_row["alias"], "b-alias");
    assert_eq!(b_row["name"], "Remote\\u000aTitle");
    assert_eq!(b_row["cwd"], "/work/b\\u202e");
    assert_eq!(b_row["machine"], "Laptop\\u202e");

    let anon = threads
        .iter()
        .find(|t| t["thread"] == "b/rk-anon")
        .expect("anon row");
    assert_eq!(anon["name"], "anon");
    assert_eq!(anon["unnamed"], true);

    let devices = listed["devices"].as_array().expect("devices");
    assert_eq!(devices[0]["this_device"], true);
    assert_eq!(devices[0]["listing"], "incomplete");
    assert_eq!(devices[0]["dropped"], 2);
    assert_eq!(devices[1]["device"], "b");
    assert_eq!(devices[1]["listing"], "listed");
    assert_eq!(devices[1]["threads"], 2);
    assert_eq!(devices[2]["device"], "c");
    assert_eq!(devices[2]["listing"], "incomplete");
    assert_eq!(devices[3]["device"], "d");
    assert_eq!(devices[3]["listing"], "offline");

    let bad_close = run(
        &component,
        "thread_close",
        json!({ "thread": "b/dur", "wait_ms": 5000 }),
    );
    assert_eq!(bad_close["kind"], "error");
    assert_eq!(bad_close["error"]["code"], "invalid_arguments");

    let closed = run(
        &component,
        "thread_close",
        json!({ "thread": format!("b/{B_ID}"), "graceful": true, "wait_ms": 50_000 }),
    );
    assert_eq!(closed["kind"], "ok", "{closed}");
    assert_eq!(closed["closed"], true);
    assert_eq!(closed["device"], "b");
    assert_eq!(closed["graceful"]["turn"], 5);
    assert_eq!(closed["graceful"]["waited_ms"], 250);

    let sent = lock(&remote.sent_commands);
    assert_eq!(sent[1].2["waitMs"], REMOTE_GRACEFUL_MAX_WAIT_MS);
    assert_eq!(sent[1].3.wait_ms, Some(REMOTE_GRACEFUL_MAX_WAIT_MS));
    drop(sent);

    let after_close_live = run(
        &component,
        "thread_list",
        json!({ "all_scope": true, "state": ["running", "idle"] }),
    );
    let live_ids: Vec<&str> = after_close_live["threads"]
        .as_array()
        .expect("threads")
        .iter()
        .filter_map(|t| t["thread"].as_str())
        .collect();
    assert!(!live_ids.contains(&B_ID));

    let after_close_all = run(&component, "thread_list", json!({ "all_scope": true }));
    let closed_row = after_close_all["threads"]
        .as_array()
        .expect("threads")
        .iter()
        .find(|t| t["thread"] == B_ID)
        .expect("ended row stamped by closed_rows");
    assert_eq!(closed_row["status"], "resumable");
    assert_eq!(closed_row["state"], "inactive");

    let escaped_err = run(
        &component,
        "thread_interrupt",
        json!({ "thread": "b/rk-anon" }),
    );
    assert_eq!(escaped_err["kind"], "error");
    let msg = escaped_err["error"]["message"].as_str().expect("msg");
    assert!(msg.contains("\\u202e"));
    assert!(!msg.contains('\n'));
}

#[test]
fn reply_on_device_covers_second_read_hit_incomplete_wake_and_lookup_refusals() {
    let remote = ScriptedRemote::new(
        vec![RemoteDevice {
            device: "b".to_owned(),
            label: "Laptop".to_owned(),
            online: true,
            platform: None,
            kind: None,
            last_seen_ms: None,
        }],
        vec![RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "rk-b".to_owned(),
            durable_id: Some(B_ID.to_owned()),
            title: "B".to_owned(),
            project: "/work/b".to_owned(),
            path: "/work/b".to_owned(),
            state: "idle".to_owned(),
            updated_ms: 1_000,
            ..RemoteSessionRow::default()
        }],
    );
    let surface = with_closed_rows(Arc::new(RemoteThreadSurface::new(
        remote.clone(),
        remote.clone(),
        None,
    )));

    lock(&remote.device_listings).push_back(Ok(RemoteListing {
        rows: Vec::new(),
        incomplete: Vec::new(),
        devices: None,
    }));
    let second_read_hit =
        block_on(reply_on_device(Some(surface.as_ref()), "b", B_ID)).expect("2nd read hit");
    assert_eq!(second_read_hit.entry.thread_id, B_ID);
    assert_eq!(*lock(&remote.device_listing_calls), 2);

    lock(&remote.device_listings).push_back(Ok(RemoteListing {
        rows: Vec::new(),
        incomplete: Vec::new(),
        devices: Some({
            let mut m = BTreeMap::new();
            m.insert("b".to_owned(), "timeout".to_owned());
            m
        }),
    }));
    let incomplete_err =
        block_on(reply_on_device(Some(surface.as_ref()), "b", B_ID)).expect_err("incomplete");
    assert_eq!(incomplete_err.code.as_str(), "host_unavailable");
    assert_eq!(
        incomplete_err
            .details
            .as_ref()
            .and_then(|d| d["reason"].as_str()),
        Some("listing_incomplete")
    );

    *lock(&remote.wake_enabled) = false;
    let wake_err = block_on(reply_on_device(
        Some(surface.as_ref()),
        "b",
        "0cc00001-0000-4000-8000-000000000003",
    ))
    .expect_err("wake off");
    assert_eq!(wake_err.code.as_str(), "host_unavailable");
    assert_eq!(
        wake_err.details.as_ref().and_then(|d| d["reason"].as_str()),
        Some("wake_unavailable")
    );

    *lock(&remote.wake_enabled) = true;
    *lock(&remote.lookup_enabled) = false;
    let lookup_err = block_on(reply_on_device(
        Some(surface.as_ref()),
        "b",
        "0cc00001-0000-4000-8000-000000000003",
    ))
    .expect_err("lookup off");
    assert_eq!(lookup_err.code.as_str(), "host_unavailable");
    assert_eq!(
        lookup_err
            .details
            .as_ref()
            .and_then(|d| d["reason"].as_str()),
        Some("lookup_unavailable")
    );

    *lock(&remote.lookup_enabled) = true;
    let missing = block_on(reply_on_device(
        Some(surface.as_ref()),
        "b",
        "0cc00001-0000-4000-8000-000000000003",
    ))
    .expect_err("not found");
    assert_eq!(missing.code.as_str(), "not_found");
    assert_eq!(
        missing.details.as_ref().and_then(|d| d["device"].as_str()),
        Some("b")
    );

    let msg = listing_incomplete("b");
    assert!(
        msg.message
            .contains("whose threads this listing round did not include in full")
    );
}

#[test]
fn attach_pool_manual_clock_expires_catalog_at_max_age_plus_one_and_detaches_idle() {
    struct TimerEntry {
        id: u64,
        due: i64,
        cancelled: bool,
        task: Option<AttachTask>,
    }
    struct Manual {
        now: Arc<AtomicI64>,
        next_id: AtomicU64,
        timers: Arc<Mutex<Vec<TimerEntry>>>,
    }
    impl Manual {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Arc::new(AtomicI64::new(10_000)),
                next_id: AtomicU64::new(1),
                timers: Arc::new(Mutex::new(Vec::new())),
            })
        }
        fn clock(&self) -> AttachClock {
            let now = Arc::clone(&self.now);
            Arc::new(move || now.load(Ordering::SeqCst))
        }
        fn schedule(self: &Arc<Self>) -> AttachSchedule {
            let me = Arc::clone(self);
            Arc::new(
                move |task: AttachTask, delay_ms: u64| -> CancelAttachTimer {
                    let id = me.next_id.fetch_add(1, Ordering::SeqCst);
                    let due =
                        me.now.load(Ordering::SeqCst) + i64::try_from(delay_ms).unwrap_or(i64::MAX);
                    lock(&me.timers).push(TimerEntry {
                        id,
                        due,
                        cancelled: false,
                        task: Some(task),
                    });
                    let timers = Arc::clone(&me.timers);
                    Box::new(move || {
                        for row in lock(&timers).iter_mut() {
                            if row.id == id {
                                row.cancelled = true;
                                row.task = None;
                            }
                        }
                    })
                },
            )
        }
        fn advance(&self, ms: i64) {
            let target = self.now.fetch_add(ms, Ordering::SeqCst) + ms;
            loop {
                let idx = lock(&self.timers)
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| !r.cancelled && r.task.is_some() && r.due <= target)
                    .min_by_key(|(_, r)| r.due)
                    .map(|(i, _)| i);
                let Some(idx) = idx else { break };
                let task = lock(&self.timers)[idx].task.take();
                if let Some(task) = task {
                    task();
                }
            }
        }
    }
    struct Client(Arc<Mutex<usize>>, Arc<Mutex<usize>>);
    impl AttachedClient for Client {
        fn command(
            &self,
            _rpc: Value,
            _opts: SessionCommandOptions,
        ) -> BoxFuture<RemoteCommandOutcome> {
            *lock(&self.0) += 1;
            let count = *lock(&self.0);
            Box::pin(async move {
                RemoteCommandOutcome::Ok {
                    response: json!({ "success": true, "data": { "n": count } }),
                    local: false,
                    located: None,
                }
            })
        }
        fn detach(&self) -> BoxFuture<()> {
            *lock(&self.1) += 1;
            Box::pin(async {})
        }
    }
    struct Transport(Arc<Mutex<usize>>, Arc<Mutex<usize>>);
    impl AttachTransport for Transport {
        fn attach(
            &self,
            _d: &str,
            _s: &str,
        ) -> BoxFuture<Result<Arc<dyn AttachedClient>, RemoteCommandOutcome>> {
            let c: Arc<dyn AttachedClient> =
                Arc::new(Client(Arc::clone(&self.0), Arc::clone(&self.1)));
            Box::pin(async move { Ok(c) })
        }
    }

    let manual = Manual::new();
    let cmds = Arc::new(Mutex::new(0));
    let detaches = Arc::new(Mutex::new(0));
    let pool = AttachPool::new(
        Arc::new(Transport(Arc::clone(&cmds), Arc::clone(&detaches))),
        manual.clock(),
        manual.schedule(),
        ATTACH_IDLE_MS,
    );

    let r1 = block_on(pool.session_command(
        "b",
        "s1",
        json!({ "type": "get_available_models" }),
        SessionCommandOptions::default(),
    ));
    assert!(matches!(r1, RemoteCommandOutcome::Ok { local: false, .. }));
    manual.advance(CATALOG_MAX_AGE_MS);
    assert_eq!(*lock(&detaches), 1);

    let r2 = block_on(pool.session_command(
        "b",
        "s1",
        json!({ "type": "get_available_models" }),
        SessionCommandOptions::default(),
    ));
    assert!(matches!(r2, RemoteCommandOutcome::Ok { local: false, .. }));
    manual.advance(CATALOG_MAX_AGE_MS - 1);
    assert_eq!(*lock(&detaches), 1);
    let r3 = block_on(pool.session_command(
        "b",
        "s1",
        json!({ "type": "get_available_models" }),
        SessionCommandOptions::default(),
    ));
    assert!(matches!(r3, RemoteCommandOutcome::Ok { local: true, .. }));
    assert_eq!(*lock(&cmds), 2);
}

#[test]
fn thread_create_for_live_remote_thread_sends_prompt_without_opening_and_supports_here_address() {
    use maho_omo_gateway::thread::control::locate::{
        LocatedControl, ThreadCall, book_of, locate_control,
    };

    const ENDED_B: &str = "0dd00001-0000-4000-8000-000000000004";
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("app");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    let cwd_str = cwd.to_string_lossy().into_owned();

    let remote = ScriptedRemote::new(
        vec![RemoteDevice {
            device: "b".to_owned(),
            label: "Laptop".to_owned(),
            online: true,
            platform: Some("darwin".to_owned()),
            kind: Some("laptop".to_owned()),
            last_seen_ms: Some(1_700_000_000_000),
        }],
        vec![
            RemoteSessionRow {
                device: "b".to_owned(),
                session_id: "rk-b".to_owned(),
                durable_id: Some(B_ID.to_owned()),
                title: "Remote Live".to_owned(),
                project: "/work/b".to_owned(),
                path: "/work/b".to_owned(),
                state: "idle".to_owned(),
                updated_ms: 1_000,
                ..RemoteSessionRow::default()
            },
            RemoteSessionRow {
                device: "b".to_owned(),
                session_id: "rk-ended".to_owned(),
                durable_id: Some(ENDED_B.to_owned()),
                title: "Remote Ended".to_owned(),
                project: "/work/b".to_owned(),
                path: "/work/b".to_owned(),
                state: "ended".to_owned(),
                updated_ms: 900,
                ..RemoteSessionRow::default()
            },
        ],
    );
    *lock(&remote.command_replies) = VecDeque::from([
        RemoteCommandOutcome::Ok {
            response: json!({ "success": true, "data": { "isStreaming": false } }),
            local: false,
            located: None,
        },
        RemoteCommandOutcome::Ok {
            response: json!({
                "success": true,
                "data": { "disposition": "started", "turnId": "t-9" }
            }),
            local: false,
            located: None,
        },
        RemoteCommandOutcome::Ok {
            response: json!({
                "success": true,
                "data": { "isStreaming": true, "pendingQuestions": [{ "id": "q1" }] }
            }),
            local: false,
            located: None,
        },
        RemoteCommandOutcome::Ok {
            response: json!({ "success": true, "data": { "kind": "queued" } }),
            local: false,
            located: None,
        },
    ]);

    let local = local_session(LOCAL_ID, &cwd_str);
    let view = ThreadHostView {
        hosts: vec![AddressBookHost {
            socket: local.socket.clone(),
            list_sessions: Some(HostListSessions::ok(vec![local.host_session()])),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
            legacy: false,
        }],
        sessions: vec![local],
        disk: Vec::new(),
    };
    let component = make_component(&dir, None, view.clone(), cwd_str.clone(), remote.clone());

    let started = run(
        &component,
        "thread_create",
        json!({ "thread": B_ID, "prompt": "carry on" }),
    );
    assert_eq!(started["kind"], "ok", "{started}");
    assert_eq!(started["thread"], B_ID);
    assert_eq!(started["deduplicated"], true);
    assert_eq!(started["device"], "b");
    assert_eq!(started["prompt"]["delivery"]["kind"], "started");
    assert_eq!(started["prompt"]["delivery"]["turn_id"], "t-9");
    assert_eq!(started["prompt"]["effective_mode"], "start");

    let queued = run(
        &component,
        "thread_create",
        json!({ "thread": format!("b/{B_ID}"), "prompt": "queued prompt" }),
    );
    assert_eq!(queued["kind"], "ok", "{queued}");
    assert_eq!(queued["thread"], B_ID);
    assert_eq!(queued["deduplicated"], true);
    assert_eq!(queued["device"], "b");
    assert_eq!(queued["prompt"]["delivery"]["kind"], "queued");

    let sent = lock(&remote.sent_commands);
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0].0, "b");
    assert_eq!(sent[0].1, "rk-b");
    assert_eq!(sent[0].2["type"], "get_state");
    assert_eq!(sent[1].2["type"], "prompt");
    assert_eq!(sent[1].2["message"], "carry on");
    assert_eq!(sent[2].2["type"], "get_state");
    assert_eq!(sent[3].2["type"], "follow_up");
    assert_eq!(sent[3].2["message"], "queued prompt");
    drop(sent);

    let reopen_ended = run(&component, "thread_create", json!({ "thread": ENDED_B }));
    assert_eq!(reopen_ended["kind"], "error", "{reopen_ended}");
    assert_eq!(reopen_ended["error"]["code"], "unsupported");
    assert_eq!(
        reopen_ended["error"]["details"]["reason"],
        "relay_session_host_unsupported"
    );

    let new_remote = run(
        &component,
        "thread_create",
        json!({ "device": "b", "name": "remote-child" }),
    );
    assert_eq!(new_remote["kind"], "error", "{new_remote}");
    assert_eq!(new_remote["error"]["code"], "unsupported");
    assert_eq!(
        new_remote["error"]["details"]["reason"],
        "relay_session_host_unsupported"
    );

    let surface = Arc::new(RemoteThreadSurface::new(
        remote.clone(),
        remote.clone(),
        Some(remote),
    ));
    let locator = Arc::new(RelayControlLocator::new(surface));
    let now = "2026-11-26T00:00:00.000Z".to_owned();
    let book = book_of(&view, &[], &now);
    let call = ThreadCall {
        host: Arc::new(StubHost {
            view: Mutex::new(view.clone()),
        }),
        view,
        book,
        caller_id: LOCAL_ID.to_owned(),
        caller_workspace_root: cwd_str,
        sessions_directory: None,
        remote: Some(locator),
        now,
    };
    let LocatedControl::Control(here_ctrl) =
        block_on(locate_control(&call, &format!("here/{LOCAL_ID}"), false))
    else {
        panic!("expected local control for here/<LOCAL_ID>");
    };
    assert_eq!(here_ctrl.thread_id(), LOCAL_ID);
    assert_eq!(here_ctrl.device(), None);

    let LocatedControl::Control(remote_ctrl) = block_on(locate_control(&call, B_ID, false)) else {
        panic!("expected relay control for remote B_ID");
    };
    assert_eq!(remote_ctrl.thread_id(), B_ID);
    assert_eq!(remote_ctrl.device(), Some("b"));
}
