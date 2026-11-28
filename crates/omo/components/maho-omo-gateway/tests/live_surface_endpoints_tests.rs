//! Ported from upstream `thread/live-surface-endpoints.test.ts` and the enumeration cache/cost/count
//! tests: the native surface enumerates the registry and lists every host shard and tui endpoint,
//! shares one enumeration per cache window, and reads a dead endpoint's sessions from their files.
//! No sleeps: the injected clock and registry/status-all fakes decide every timing fact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use maho_omo_gateway::thread::adapter::BoxFuture;
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::endpoint_registry::RegistryEndpoint;
use maho_omo_gateway::thread::host_view::ThreadHost;
use maho_omo_gateway::thread::live_surface::{
    HOST_ENDPOINTS_CACHE_TTL_MS, HostEndpointReport, LiveThreadSurface, LiveThreadSurfaceOptions,
};
use maho_omo_gateway::thread::types::GatewayEndpointKind;
use serde_json::{Value, json};
use tempfile::tempdir;

const LEGACY: &str = "/run/rpc.sock";
const SHARD: &str = "/run/i-0123456789abcdef.sock";
const TUI: &str = "/run/t-0123456789abcdef.sock";

/// An in-memory endpoint host: one `list_sessions` answer per socket, plus a set of sockets that
/// refuse the request (dead endpoints); a record of every dial is kept too.
#[derive(Default)]
struct FakeHost {
    sessions: Mutex<BTreeMap<String, Value>>,
    failing: Mutex<BTreeSet<String>>,
    dialed: Mutex<Vec<String>>,
}

impl FakeHost {
    fn with_sessions(sessions: Vec<(String, Value)>) -> Self {
        Self {
            sessions: Mutex::new(sessions.into_iter().collect()),
            failing: Mutex::default(),
            dialed: Mutex::default(),
        }
    }

    fn failing(mut self, socket: &str) -> Self {
        self.failing.lock().unwrap().insert(socket.to_owned());
        self
    }
}

impl RpcTransport for FakeHost {
    fn request(
        &self,
        socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        self.dialed.lock().unwrap().push(socket.to_owned());
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let answer = if self.failing.lock().unwrap().contains(socket) {
            return Box::pin(async move { Err(format!("host_unavailable:{socket}")) });
        } else if kind == "list_sessions" {
            let sessions = self
                .sessions
                .lock()
                .unwrap()
                .get(socket)
                .cloned()
                .unwrap_or_default();
            json!({ "success": true, "data": { "sessions": sessions } })
        } else {
            json!({ "success": true, "data": {} })
        };
        Box::pin(async move { Ok(answer) })
    }
}

fn surface(
    transport: Arc<dyn RpcTransport>,
    registry: Vec<RegistryEndpoint>,
    status_all: Option<Vec<HostEndpointReport>>,
) -> LiveThreadSurface {
    LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport,
        status_all: status_all.map(|reports| Arc::new(move || Some(reports.clone())) as Arc<_>),
        now: None,
        monotonic: None,
        enumerations: None,
        registry: Some(Arc::new(move || registry.clone())),
    })
}

fn registry_endpoint(socket: &str, dir: &str, kind: GatewayEndpointKind) -> RegistryEndpoint {
    RegistryEndpoint {
        socket: Some(socket.to_owned()),
        dir: dir.to_owned(),
        identity: maho_omo_gateway::thread::endpoint_registry::EndpointIdentitySource::Endpoint,
        endpoint_kind: match kind {
            GatewayEndpointKind::RpcHost => {
                maho_omo_gateway::thread::endpoint_registry::EndpointKind::RpcHost
            }
            GatewayEndpointKind::Tui => {
                maho_omo_gateway::thread::endpoint_registry::EndpointKind::Tui
            }
        },
        registry_version: Some(2),
    }
}

fn session_row(id: &str, path: &str) -> Value {
    json!({
        "sessionId": id,
        "durableSessionId": id,
        "sessionPath": path,
        "cwd": "/work",
        "status": "open",
    })
}

fn write_session_file(path: &Path, durable_id: &str) {
    let text = format!(
        "{{\"type\":\"session\",\"id\":\"{durable_id}\",\"cwd\":\"/work\",\"timestamp\":\"2026-11-12T10:00:00.000Z\"}}\n{{\"type\":\"message\",\"timestamp\":\"2026-11-12T10:01:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n"
    );
    fs::write(path, text).unwrap();
}

#[test]
fn a_registry_endpoint_is_listed_alongside_the_legacy_host() {
    let host = Arc::new(FakeHost::with_sessions(vec![
        (
            LEGACY.to_owned(),
            json!([session_row("dur-legacy", "/sessions/legacy.jsonl")]),
        ),
        (
            SHARD.to_owned(),
            json!([session_row("dur-shard", "/sessions/shard.jsonl")]),
        ),
    ]));
    let registry = vec![registry_endpoint(
        SHARD,
        "/registry/i-1",
        GatewayEndpointKind::RpcHost,
    )];
    let surface = surface(host.clone(), registry, None);
    let view = block_on(surface.list_view()).expect("list_view");

    assert_eq!(view.sessions.len(), 2);
    assert_eq!(
        view.sessions
            .iter()
            .map(|session| session.socket.as_str())
            .collect::<Vec<_>>(),
        vec![LEGACY, SHARD]
    );
    assert_eq!(view.hosts.len(), 2);
    assert!(host.dialed.lock().unwrap().contains(&SHARD.to_owned()));
}

#[test]
fn a_tui_endpoint_is_listed_live_from_its_control_socket() {
    let host = Arc::new(FakeHost::with_sessions(vec![(
        TUI.to_owned(),
        json!([session_row("dur-tui", "/sessions/tui.jsonl")]),
    )]));
    let registry = vec![registry_endpoint(
        TUI,
        "/registry/t-1",
        GatewayEndpointKind::Tui,
    )];
    let surface = surface(host.clone(), registry, None);
    let view = block_on(surface.list_view()).expect("list_view");

    let tui = view
        .sessions
        .iter()
        .find(|session| session.socket == TUI)
        .expect("tui session");
    assert_eq!(tui.endpoint_kind, GatewayEndpointKind::Tui);
    assert!(host.dialed.lock().unwrap().contains(&TUI.to_owned()));
}

#[test]
fn a_dead_shard_lists_its_claimed_session_files_from_disk() {
    let dir = tempdir().unwrap();
    let session_path = dir.path().join("shard.jsonl");
    write_session_file(&session_path, "dur-shard");
    let claims = dir.path().join("reservations");
    fs::create_dir_all(&claims).unwrap();
    fs::write(
        claims.join("a0000000000000000.json"),
        json!({ "sessionPath": session_path.to_str().unwrap() }).to_string(),
    )
    .unwrap();

    let host = Arc::new(
        FakeHost::with_sessions(vec![(
            LEGACY.to_owned(),
            json!([session_row("dur-legacy", "/sessions/legacy.jsonl")]),
        )])
        .failing(SHARD),
    );
    let registry = vec![RegistryEndpoint {
        socket: Some(SHARD.to_owned()),
        dir: dir.path().to_str().unwrap().to_owned(),
        identity: maho_omo_gateway::thread::endpoint_registry::EndpointIdentitySource::Endpoint,
        endpoint_kind: maho_omo_gateway::thread::endpoint_registry::EndpointKind::RpcHost,
        registry_version: Some(2),
    }];
    let surface = surface(host.clone(), registry, None);
    let view = block_on(surface.list_view()).expect("list_view");

    let disk = view
        .disk
        .iter()
        .find(|session| session.durable_id == "dur-shard")
        .expect("claimed disk session");
    assert_eq!(disk.source_host.as_deref(), Some(SHARD));
    assert!(!host.dialed.lock().unwrap().contains(&SHARD.to_owned()));
}

#[test]
fn an_enumeration_is_reused_inside_the_cache_window_and_repeated_after_it() {
    let clock = Arc::new(Mutex::new(1_000_i64));
    let registry_calls = Arc::new(Mutex::new(0));
    let host = Arc::new(FakeHost::default());
    let calls = registry_calls.clone();
    let now = clock.clone();
    let surface = LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport: host,
        status_all: None,
        now: Some(Arc::new(move || *now.lock().unwrap())),
        monotonic: Some(Arc::new(move || *now.lock().unwrap())),
        enumerations: None,
        registry: Some(Arc::new(move || {
            *calls.lock().unwrap() += 1;
            Vec::new()
        })),
    });

    block_on(surface.list_view()).expect("first");
    *clock.lock().unwrap() += HOST_ENDPOINTS_CACHE_TTL_MS - 1;
    block_on(surface.list_view()).expect("inside window");
    assert_eq!(*registry_calls.lock().unwrap(), 1);
    *clock.lock().unwrap() += 1;
    block_on(surface.list_view()).expect("past window");
    assert_eq!(*registry_calls.lock().unwrap(), 2);
}

#[test]
fn surfaces_over_one_agent_dir_share_one_enumeration() {
    let enumerations = Arc::new(Mutex::new(BTreeMap::new()));
    let registry_calls = Arc::new(Mutex::new(0));
    let calls = registry_calls.clone();
    let surface_of = || {
        LiveThreadSurface::new(LiveThreadSurfaceOptions {
            socket: LEGACY.to_owned(),
            transport: Arc::new(FakeHost::default()),
            status_all: None,
            now: None,
            monotonic: None,
            enumerations: Some(enumerations.clone()),
            registry: Some(Arc::new(move || {
                *calls.lock().unwrap() += 1;
                Vec::new()
            })),
        })
    };
    block_on(surface_of().list_view()).expect("first surface");
    block_on(surface_of().list_view()).expect("second surface");
    assert_eq!(*registry_calls.lock().unwrap(), 1);
    assert_eq!(enumerations.lock().unwrap().len(), 1);
}

#[test]
fn status_all_and_registry_are_both_read_when_enumerating() {
    let host = Arc::new(FakeHost::default());
    let status = Arc::new(Mutex::new(0));
    let status_calls = status.clone();
    let surface = LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport: host,
        status_all: Some(Arc::new(move || {
            *status_calls.lock().unwrap() += 1;
            Some(vec![HostEndpointReport {
                socket: Some(SHARD.to_owned()),
                reachable: true,
                session_paths: Vec::new(),
                endpoint_kind: Some(GatewayEndpointKind::RpcHost),
                alive: Some(true),
                reason: None,
            }])
        })),
        now: None,
        monotonic: None,
        enumerations: None,
        registry: Some(Arc::new(Vec::new)),
    });
    let view = block_on(surface.list_view()).expect("list_view");
    assert_eq!(*status.lock().unwrap(), 1);
    assert_eq!(view.hosts.len(), 2);
}

#[test]
fn parse_host_status_all_keeps_reported_sessions_and_kind() {
    let stdout = "{\"endpoints\":[{\"socket\":\"/run/i-1.sock\",\"reachable\":true,\"session_rows\":[{\"session_path\":\"/s/a.jsonl\"}],\"endpoint_kind\":\"tui\",\"alive\":true}]}\n";
    let reports =
        maho_omo_gateway::thread::live_surface::parse_host_status_all(stdout).expect("reports");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].socket.as_deref(), Some("/run/i-1.sock"));
    assert_eq!(reports[0].session_paths, vec!["/s/a.jsonl".to_owned()]);
    assert_eq!(reports[0].endpoint_kind, Some(GatewayEndpointKind::Tui));
}
