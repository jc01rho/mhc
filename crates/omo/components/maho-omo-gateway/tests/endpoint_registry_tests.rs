//! Upstream `thread/endpoint-registry.test.ts`: the registry mirror lists one entry per layout-2
//! endpoint directory, names the file that identified each socket, keeps a `tui` endpoint's kind,
//! and reads the session claims a directory holds.

use std::collections::BTreeMap;
use std::fs;

use maho_omo_gateway::thread::endpoint_registry::{
    EndpointIdentitySource, EndpointKind, RegistryEndpoint, claimed_session_paths,
    control_socket_secret_path, is_tui_control_socket, list_registry_endpoints,
};
use serde_json::{Value, json};
use tempfile::tempdir;

/// A layout-2 registry root under `dir`, the flat `rpc-host-daemon` directory the engine writes.
fn registry_root(dir: &tempfile::TempDir) -> String {
    let root = dir.path().to_str().expect("registry root path").to_owned();
    fs::create_dir_all(format!("{root}/rpc-host-daemon")).expect("registry root");
    fs::write(
        format!("{root}/rpc-host-daemon/layout.json"),
        r#"{"layout":2}"#,
    )
    .expect("layout marker");
    root
}

/// One endpoint directory; `name` must be the hash the engine derives from `socket`.
fn write_endpoint(root: &str, name: &str, files: &[(&str, Value)]) -> String {
    let dir = format!("{root}/rpc-host-daemon/{name}");
    fs::create_dir_all(&dir).expect("endpoint directory");
    for (file, content) in files {
        fs::write(
            format!("{dir}/{file}"),
            serde_json::to_string(content).expect("record json"),
        )
        .expect("record file");
    }
    dir
}

fn by_socket(listed: &[RegistryEndpoint]) -> BTreeMap<String, &RegistryEndpoint> {
    listed
        .iter()
        .filter_map(|entry| Some((entry.socket.clone()?, entry)))
        .collect()
}

#[test]
fn layout_marker_required() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_owned();
    fs::create_dir_all(format!("{root}/rpc-host-daemon")).unwrap();
    assert!(list_registry_endpoints(&root).is_empty());
    fs::write(
        format!("{root}/rpc-host-daemon/layout.json"),
        r#"{"layout":1}"#,
    )
    .unwrap();
    assert!(list_registry_endpoints(&root).is_empty());
}

#[test]
fn is_tui_control_socket_matches_lowercase_hex_only() {
    // upstream `TUI_SOCKET_NAME = /^t-[0-9a-f]{16}\.sock$/`
    assert!(is_tui_control_socket("/tmp/t-0123456789abcdef.sock"));
    assert!(!is_tui_control_socket("/tmp/t-0123456789ABCDEF.sock"));
    assert!(!is_tui_control_socket("/tmp/t-0123456789abcde.sock"));
    assert!(!is_tui_control_socket("/tmp/rpc.sock"));
}

#[test]
fn control_socket_secret_path_appends() {
    assert_eq!(
        control_socket_secret_path("/tmp/x.sock"),
        "/tmp/x.sock.secret"
    );
}

#[test]
fn list_registry_endpoints_records_kinds_identities_and_versions() {
    let dir = tempdir().unwrap();
    let root = registry_root(&dir);

    // `socket_names_directory` hashes the socket, so each directory is named by its own hash.
    let tui = "/run/t-0123456789abcdef.sock".to_owned();
    let legacy = "/run/rpc.sock".to_owned();
    let shard = "/run/i-0123456789abcdef.sock".to_owned();

    write_endpoint(
        &root,
        "85bd7e46b9fb2cd8",
        &[(
            "endpoint.json",
            json!({
                "layout": 2,
                "registry_version": 1,
                "endpoint_kind": "tui",
                "socket": tui.clone(),
                "created_at": "2026-09-29T00:00:00.000Z",
            }),
        )],
    );
    write_endpoint(
        &root,
        "1010e032b9710667",
        &[(
            "endpoint.json",
            json!({ "layout": 2, "socket": legacy.clone() }),
        )],
    );
    write_endpoint(
        &root,
        "1f68fe26d1f3effd",
        &[("settings.json", json!({ "socket": shard.clone() }))],
    );

    let listed = list_registry_endpoints(&root);
    let found = by_socket(&listed);

    let tui_entry = found.get(&tui).expect("the tui endpoint");
    assert_eq!(tui_entry.endpoint_kind, EndpointKind::Tui);
    assert_eq!(tui_entry.identity, EndpointIdentitySource::Endpoint);
    assert_eq!(tui_entry.registry_version, Some(1));

    let legacy_entry = found.get(&legacy).expect("the legacy endpoint");
    assert_eq!(legacy_entry.endpoint_kind, EndpointKind::RpcHost);
    assert_eq!(legacy_entry.identity, EndpointIdentitySource::Endpoint);
    assert_eq!(legacy_entry.registry_version, None);

    let shard_entry = found.get(&shard).expect("the shard endpoint");
    assert_eq!(shard_entry.identity, EndpointIdentitySource::Settings);
    assert_eq!(shard_entry.endpoint_kind, EndpointKind::RpcHost);
}

#[test]
fn a_directory_naming_no_matching_socket_is_unknown() {
    let dir = tempdir().unwrap();
    let root = registry_root(&dir);
    // A record whose socket does not hash to its directory names no endpoint.
    write_endpoint(
        &root,
        "ffffffffffffffff",
        &[("endpoint.json", json!({ "socket": "/x.sock" }))],
    );

    let listed = list_registry_endpoints(&root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].socket, None);
    assert_eq!(listed[0].identity, EndpointIdentitySource::Unknown);
}

#[test]
fn a_generation_settings_record_identifies_its_endpoint() {
    let dir = tempdir().unwrap();
    let root = registry_root(&dir);
    let shard = "/run/i-0123456789abcdef.sock".to_owned();
    let endpoint_dir = write_endpoint(&root, "1f68fe26d1f3effd", &[]);
    fs::create_dir_all(format!("{endpoint_dir}/generations/g-1")).unwrap();
    fs::write(
        format!("{endpoint_dir}/generations/g-1/settings.json"),
        json!({ "socket": shard.clone() }).to_string(),
    )
    .unwrap();

    let listed = list_registry_endpoints(&root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].socket.as_deref(), Some(shard.as_str()));
    assert_eq!(
        listed[0].identity,
        EndpointIdentitySource::GenerationSettings
    );
}

#[test]
fn claimed_session_paths_reads_only_named_session_files() {
    let dir = tempdir().unwrap();
    let root = registry_root(&dir);
    let endpoint_dir = write_endpoint(&root, "ffffffffffffffff", &[]);
    let claims = format!("{endpoint_dir}/reservations");
    fs::create_dir_all(&claims).unwrap();
    let write = |name: &str, body: &str| fs::write(format!("{claims}/{name}"), body).unwrap();
    write("b0000000000000000.json", r#"{"sessionPath":"/s/b.jsonl"}"#);
    write("a0000000000000000.json", r#"{"sessionPath":"/s/a.jsonl"}"#);
    write("c0000000000000000.json", r#"{"sessionPath":""}"#);
    write("d0000000000000000.json", r#"{"sessionPath":7}"#);
    write("e0000000000000000.json", r#"{}"#);
    write("f0000000000000000.json", "{ not json");
    write("notes.txt", r#"{"sessionPath":"/s/not-a-claim.jsonl"}"#);

    assert_eq!(
        claimed_session_paths(&endpoint_dir),
        ["/s/a.jsonl".to_owned(), "/s/b.jsonl".to_owned()]
    );
}

#[test]
fn claimed_session_paths_is_empty_without_a_reservations_directory() {
    let dir = tempdir().unwrap();
    let root = registry_root(&dir);
    let endpoint_dir = write_endpoint(&root, "ffffffffffffffff", &[]);
    assert!(claimed_session_paths(&endpoint_dir).is_empty());
}
