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
}

#[test]
fn control_socket_secret_path_appends() {
    assert_eq!(
        control_socket_secret_path("/tmp/x.sock"),
        "/tmp/x.sock.secret"
    );
}
