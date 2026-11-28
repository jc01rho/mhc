//! Ported from upstream `thread/live-surface-endpoints.test.ts` and the enumeration cache/cost tests:
//! the native surface enumerates the registry and lists every host shard and tui endpoint, reuses one
//! enumeration per cache window, and reads a dead endpoint's claimed sessions from their files.
//! No sleeps: the injected clock and the registry/status-all fakes decide every timing fact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use maho_omo_gateway::thread::adapter::BoxFuture;
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::endpoint_registry::{
    EndpointIdentitySource, EndpointKind, RegistryEndpoint,
};
use maho_omo_gateway::thread::host_view::ThreadHost;
use maho_omo_gateway::thread::live_surface::{
    HOST_ENDPOINTS_CACHE_TTL_MS, HostEndpointReport, LiveThreadSurface, LiveThreadSurfaceOptions,
    SharedEnumeration,
};
use maho_omo_gateway::thread::types::GatewayEndpointKind;
use serde_json::{Value, json};
use tempfile::tempdir;

const LEGACY: &str = "/run/rpc.sock";
const SHARD: &str = "/run/i-0123456789abcdef.sock";
const TUI: &str = "/run/t-0123456789abcdef.sock";

type StatusAll = Arc<dyn Fn() -> Option<Vec<HostEndpointReport>> + Send + Sync>;
type Registry = Arc<dyn Fn() -> Vec<RegistryEndpoint> + Send + Sync>;

/// An in-memory endpoint host: one `list_sessions` answer per socket, plus the sockets that refuse
/// the request (dead endpoints). Every dial is recorded.
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

    fn failing(self, socket: &str) -> Self {
        self.failing
            .lock()
            .expect("failing endpoints")
            .insert(socket.to_owned());
        self
    }

    fn dialed(&self) -> Vec<String> {
        self.dialed.lock().expect("dialed sockets").clone()
    }
}

impl RpcTransport for FakeHost {
    fn request(
        &self,
        socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        self.dialed
            .lock()
            .expect("dialed sockets")
            .push(socket.to_owned());
        if self
            .failing
            .lock()
            .expect("failing endpoints")
            .contains(socket)
        {
            let error = format!("host_unavailable:{socket}");
            return Box::pin(async move { Err(error) });
        }
        let answer = if command["type"].as_str() == Some("list_sessions") {
            let sessions = self
                .sessions
                .lock()
                .expect("endpoint sessions")
                .get(socket)
                .cloned()
                .unwrap_or_else(|| json!([]));
            json!({ "success": true, "data": { "sessions": sessions } })
        } else {
            json!({ "success": true, "data": {} })
        };
        Box::pin(async move { Ok(answer) })
    }
}

fn status_all_of(reports: Vec<HostEndpointReport>) -> StatusAll {
    Arc::new(move || Some(reports.clone()))
}

fn registry_of(entries: Vec<RegistryEndpoint>) -> Registry {
    Arc::new(move || entries.clone())
}

fn surface(
    transport: Arc<dyn RpcTransport>,
    registry: Vec<RegistryEndpoint>,
    status_all: Option<Vec<HostEndpointReport>>,
) -> LiveThreadSurface {
    LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport,
        status_all: status_all.map(status_all_of),
        now: None,
        enumerations: None,
        registry: Some(registry_of(registry)),
    })
}

fn registry_endpoint(socket: &str, dir: &str, kind: GatewayEndpointKind) -> RegistryEndpoint {
    RegistryEndpoint {
        socket: Some(socket.to_owned()),
        dir: dir.to_owned(),
        identity: EndpointIdentitySource::Endpoint,
        endpoint_kind: match kind {
            GatewayEndpointKind::Tui => EndpointKind::Tui,
            GatewayEndpointKind::RpcHost => EndpointKind::RpcHost,
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
    fs::write(path, text).expect("session file");
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
    let surface = surface(
        host.clone(),
        vec![registry_endpoint(
            SHARD,
            "/registry/i-1",
            GatewayEndpointKind::RpcHost,
        )],
        None,
    );
    let view = block_on(surface.list_view()).expect("list_view");

    assert_eq!(
        view.sessions
            .iter()
            .map(|session| session.socket.as_str())
            .collect::<Vec<&str>>(),
        [LEGACY, SHARD]
    );
    assert_eq!(view.hosts.len(), 2);
    assert!(host.dialed().contains(&SHARD.to_owned()));
}

#[test]
fn a_tui_endpoint_is_listed_live_from_its_control_socket() {
    let host = Arc::new(FakeHost::with_sessions(vec![(
        TUI.to_owned(),
        json!([session_row("dur-tui", "/sessions/tui.jsonl")]),
    )]));
    let surface = surface(
        host.clone(),
        vec![registry_endpoint(
            TUI,
            "/registry/t-1",
            GatewayEndpointKind::Tui,
        )],
        None,
    );
    let view = block_on(surface.list_view()).expect("list_view");

    let tui = view
        .sessions
        .iter()
        .find(|session| session.socket == TUI)
        .expect("tui session");
    assert_eq!(tui.endpoint_kind, GatewayEndpointKind::Tui);
    assert!(host.dialed().contains(&TUI.to_owned()));
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
    let surface = surface(
        host.clone(),
        vec![RegistryEndpoint {
            socket: Some(SHARD.to_owned()),
            dir: dir.path().to_str().unwrap().to_owned(),
            identity: EndpointIdentitySource::Endpoint,
            endpoint_kind: EndpointKind::RpcHost,
            registry_version: Some(2),
        }],
        None,
    );
    let view = block_on(surface.list_view()).expect("list_view");

    // The shard was dialled and refused; its session is still listed, read from the file its
    // registry directory claims (#13452).
    assert!(host.dialed().contains(&SHARD.to_owned()));
    let disk = view
        .disk
        .iter()
        .find(|session| session.durable_id == "dur-shard")
        .expect("claimed disk session");
    assert_eq!(disk.source_host.as_deref(), Some(SHARD));
}

#[test]
fn an_enumeration_is_reused_inside_the_cache_window_and_repeated_after_it() {
    let clock = Arc::new(Mutex::new(1_000_i64));
    let registry_calls = Arc::new(Mutex::new(0_usize));
    let calls = registry_calls.clone();
    let now = clock.clone();
    let surface = LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport: Arc::new(FakeHost::default()),
        status_all: None,
        now: Some(Arc::new(move || *now.lock().unwrap())),
        enumerations: None,
        registry: Some(Arc::new(move || {
            *calls.lock().unwrap() += 1;
            Vec::<RegistryEndpoint>::new()
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
    let enumerations: Arc<Mutex<BTreeMap<String, SharedEnumeration>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let registry_calls = Arc::new(Mutex::new(0_usize));
    let calls = registry_calls.clone();
    let surface_of = || {
        let calls = calls.clone();
        LiveThreadSurface::new(LiveThreadSurfaceOptions {
            socket: LEGACY.to_owned(),
            transport: Arc::new(FakeHost::default()),
            status_all: None,
            now: None,
            enumerations: Some(enumerations.clone()),
            registry: Some(Arc::new(move || {
                *calls.lock().unwrap() += 1;
                Vec::<RegistryEndpoint>::new()
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
    let status_calls = Arc::new(Mutex::new(0_usize));
    let calls = status_calls.clone();
    let surface = LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: LEGACY.to_owned(),
        transport: Arc::new(FakeHost::default()),
        status_all: Some(Arc::new(move || {
            *calls.lock().unwrap() += 1;
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
        enumerations: None,
        registry: Some(registry_of(Vec::new())),
    });
    let view = block_on(surface.list_view()).expect("list_view");
    assert_eq!(*status_calls.lock().unwrap(), 1);
    assert_eq!(view.hosts.len(), 2);
}

#[test]
fn parse_host_status_all_keeps_reported_sessions_and_kind() {
    let stdout = "{\"endpoints\":[{\"socket\":\"/run/i-1.sock\",\"reachable\":true,\"session_rows\":[{\"session_path\":\"/s/a.jsonl\"}],\"endpoint_kind\":\"tui\",\"alive\":true}]}\n";
    let reports =
        maho_omo_gateway::thread::live_surface::parse_host_status_all(stdout).expect("reports");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].socket.as_deref(), Some("/run/i-1.sock"));
    assert_eq!(reports[0].session_paths, ["/s/a.jsonl".to_owned()]);
    assert_eq!(reports[0].endpoint_kind, Some(GatewayEndpointKind::Tui));
}
