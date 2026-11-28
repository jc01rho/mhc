use maho_omo_gateway::thread::endpoint_registry::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn layout_marker_required() {
    let dir = tempdir().unwrap();
    let flat = dir.path().join("rpc-host-daemon");
    fs::create_dir_all(&flat).unwrap();
    assert!(list_registry_endpoints(dir.path().to_str().unwrap()).is_empty());
    fs::write(flat.join("layout.json"), r#"{"layout":2}"#).unwrap();
    assert!(list_registry_endpoints(dir.path().to_str().unwrap()).is_empty());
}

#[test]
fn is_tui_control_socket_matches() {
    assert!(is_tui_control_socket("/tmp/t-0123456789abcdef.sock"));
    assert!(!is_tui_control_socket("/tmp/rpc.sock"));
    // uppercase hex is not accepted — upstream `TUI_SOCKET_NAME = /^t-[0-9a-f]{16}\\.sock$/`
    assert!(!is_tui_control_socket("/tmp/t-0123456789ABCDEF.sock"));
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
    let root = dir.path().to_str().unwrap().to_owned();
    fs::create_dir_all(format!("{root}/rpc-host-daemon")).unwrap();
    fs::write(
        format!("{root}/rpc-host-daemon/layout.json"),
        r#"{"layout":2}"#,
    )
    .unwrap();

    let tui = "/run/t-0123456789abcdef.sock".to_owned();
    let legacy = "/run/rpc.sock".to_owned();
    let shard = "/run/i-0123456789abcdef.sock".to_owned();

    write_endpoint(
        &root,
        "85bd7e46b9fb2cd8",
        &tui,
        &[(
            "endpoint.json",
            serde_json::json!({
                "layout": 2,
                "registry_version": 1,
                "endpoint_kind": "tui",
                "socket": tui,
                "created_at": "2026-09-29T00:00:00.000Z",
            }),
        )],
    );
    write_endpoint(
        &root,
        "1010e032b9710667",
        &legacy,
        &[(
            "endpoint.json",
            serde_json::json!({
                "layout": 2,
                "socket": legacy,
                "created_at": "2026-09-01T00:00:00.000Z",
            }),
        )],
    );
    write_endpoint(
        &root,
        "1f68fe26d1f3effd",
        &shard,
        &[("settings.json", serde_json::json!({ "socket": shard }))],
    );

    let listed = list_registry_endpoints(&root);
    let by_socket: std::collections::BTreeMap<Option<&str>, &RegistryEndpoint> =
        listed.iter().map(|e| (e.socket.as_deref(), e)).collect();

    assert_eq!(
        by_socket.get(Some(&tui)).map(|e| e.endpoint_kind.clone()),
        Some(EndpointKind::Tui)
    );
    assert_eq!(
        by_socket.get(Some(&tui)).map(|e| e.identity.clone()),
        Some(EndpointIdentitySource::Endpoint)
    );
    assert_eq!(
        by_socket.get(Some(&tui)).map(|e| e.registry_version),
        Some(1)
    );
    assert_eq!(
        by_socket.get(Some(&legacy)).map(|e| e.identity.clone()),
        Some(EndpointIdentitySource::Endpoint)
    );
    assert_eq!(
        by_socket.get(Some(&legacy)).map(|e| e.registry_version),
        None
    );
    assert_eq!(
        by_socket.get(Some(&shard)).map(|e| e.identity.clone()),
        Some(EndpointIdentitySource::Settings)
    );
}

#[test]
fn list_registry_endpoints_unknown_for_no_matching_record() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_owned();
    fs::create_dir_all(format!("{root}/rpc-host-daemon")).unwrap();
    fs::write(
        format!("{root}/rpc-host-daemon/layout.json"),
        r#"{"layout":2}"#,
    )
    .unwrap();
    write_endpoint(
        &root,
        "f90870743c87a7c2",
        "/x.sock",
        &[("endpoint.json", serde_json::json!({ "socket": "/x.sock" }))],
    );
    let listed = list_registry_endpoints(&root);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].socket, None);
    assert_eq!(listed[0].identity, EndpointIdentitySource::Unknown);
}

#[test]
fn claimed_session_paths_reads_only_named_session_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_owned();
    fs::create_dir_all(format!("{root}/rpc-host-daemon")).unwrap();
    fs::write(
        format!("{root}/rpc-host-daemon/layout.json"),
        r#"{"layout":2}"#,
    )
    .unwrap();
    let dir = write_endpoint(&root, "0000000000000000", "/x.sock", &[]);
    fs::create_dir_all(format!("{dir}/reservations")).unwrap();
    fs::write(
        format!("{dir}/reservations/b0000000000000000.json"),
        r#"{"sessionPath":"/s/b.jsonl"}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/a0000000000000000.json"),
        r#"{"sessionPath":"/s/a.jsonl"}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/c0000000000000000.json"),
        r#"{"sessionPath":""}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/d0000000000000000.json"),
        r#"{"sessionPath":7}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/e0000000000000000.json"),
        r#"{}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/notes.txt"),
        r#"{"sessionPath":"/s/not-a-claim.jsonl"}"#,
    )
    .unwrap();
    fs::write(
        format!("{dir}/reservations/f0000000000000000.json"),
        "{ not json",
    )
    .unwrap();

    let paths = claimed_session_paths(&dir);
    assert_eq!(
        paths,
        vec!["/s/a.jsonl".to_owned(), "/s/b.jsonl".to_owned()]
    );
}

fn write_endpoint(
    root: &str,
    name: &str,
    socket: &str,
    files: &[(&str, serde_json::Value)],
) -> String {
    let dir = format!("{root}/rpc-host-daemon/{name}");
    fs::create_dir_all(&dir).unwrap();
    for (file, content) in files {
        fs::write(
            format!("{dir}/{file}"),
            serde_json::to_string(content).unwrap(),
        )
        .unwrap();
    }
    dir.to_owned()
}
