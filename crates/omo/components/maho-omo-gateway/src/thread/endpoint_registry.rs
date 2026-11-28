//! Read-only mirror of senpi's endpoint registry.
//!
//! Upstream counterpart: `thread/endpoint-registry.ts`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const ENDPOINT_REGISTRY_LAYOUT: i64 = 2;

pub fn is_tui_control_socket(socket: &str) -> bool {
    let name = Path::new(socket)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    // t-<16 hex>.sock
    name.len() == 19
        && name.starts_with("t-")
        && name.ends_with(".sock")
        && name[2..18].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn control_socket_secret_path(socket: &str) -> String {
    format!("{socket}.secret")
}

fn canonical_directory(directory: &str) -> PathBuf {
    // Resolve deepest existing ancestor.
    let mut missing: Vec<String> = Vec::new();
    let mut current = PathBuf::from(directory);
    loop {
        if current.exists() {
            // realpath via fs::canonicalize
            if let Ok(real) = fs::canonicalize(&current) {
                let mut p = real;
                for part in missing.iter().rev() {
                    p = p.join(part);
                }
                return p;
            }
        }
        let parent = current.parent().map(|p| p.to_path_buf());
        match parent {
            Some(p) if p != current => {
                if let Some(base) = current.file_name() {
                    missing.push(base.to_string_lossy().into_owned());
                }
                current = p;
            }
            _ => return PathBuf::from(directory),
        }
    }
}

fn canonical_endpoint_path(socket: &str) -> String {
    if socket.starts_with('\0') {
        return socket.to_owned();
    }
    let dir = Path::new(socket)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = Path::new(socket)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    canonical_directory(&dir)
        .join(base)
        .to_string_lossy()
        .into_owned()
}

fn hash_name(endpoint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(endpoint.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_owned()
}

pub fn socket_names_directory(socket: &str, name: &str) -> bool {
    hash_name(&canonical_endpoint_path(socket)) == name || hash_name(socket) == name
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointIdentitySource {
    Endpoint,
    Settings,
    GenerationSettings,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointKind {
    RpcHost,
    Tui,
}

#[derive(Debug, Clone)]
pub struct RegistryEndpoint {
    pub socket: Option<String>,
    pub dir: String,
    pub identity: EndpointIdentitySource,
    pub endpoint_kind: EndpointKind,
    pub registry_version: Option<i64>,
}

fn read_record(file: &str) -> Option<BTreeMap<String, Value>> {
    let text = fs::read_to_string(file).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    match parsed {
        Value::Object(m) => Some(m.into_iter().collect()),
        _ => None,
    }
}

fn record_naming(file: &str, dir: &str) -> Option<(String, BTreeMap<String, Value>)> {
    let record = read_record(file)?;
    let socket = record.get("socket")?.as_str()?.to_owned();
    if socket.is_empty() {
        return None;
    }
    let name = Path::new(dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if !socket_names_directory(&socket, name) {
        return None;
    }
    Some((socket, record))
}

fn identify(dir: &str) -> RegistryEndpoint {
    let endpoint_file = format!("{dir}/endpoint.json");
    if let Some((socket, record)) = record_naming(&endpoint_file, dir) {
        let kind = if record.get("endpoint_kind").and_then(Value::as_str) == Some("tui") {
            EndpointKind::Tui
        } else {
            EndpointKind::RpcHost
        };
        let rv = record.get("registry_version").and_then(Value::as_i64);
        return RegistryEndpoint {
            socket: Some(socket),
            dir: dir.to_owned(),
            identity: EndpointIdentitySource::Endpoint,
            endpoint_kind: kind,
            registry_version: rv,
        };
    }
    let settings_file = format!("{dir}/settings.json");
    if let Some((socket, _)) = record_naming(&settings_file, dir) {
        return RegistryEndpoint {
            socket: Some(socket),
            dir: dir.to_owned(),
            identity: EndpointIdentitySource::Settings,
            endpoint_kind: EndpointKind::RpcHost,
            registry_version: None,
        };
    }
    // generations
    let gen_dir = format!("{dir}/generations");
    if let Ok(entries) = fs::read_dir(&gen_dir) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for instance in names {
            let f = format!("{gen_dir}/{instance}/settings.json");
            if let Some((socket, _)) = record_naming(&f, dir) {
                return RegistryEndpoint {
                    socket: Some(socket),
                    dir: dir.to_owned(),
                    identity: EndpointIdentitySource::GenerationSettings,
                    endpoint_kind: EndpointKind::RpcHost,
                    registry_version: None,
                };
            }
        }
    }
    RegistryEndpoint {
        socket: None,
        dir: dir.to_owned(),
        identity: EndpointIdentitySource::Unknown,
        endpoint_kind: EndpointKind::RpcHost,
        registry_version: None,
    }
}

pub fn claimed_session_paths(dir: &str) -> Vec<String> {
    let res_dir = format!("{dir}/reservations");
    let mut paths = Vec::new();
    let entries = match fs::read_dir(&res_dir) {
        Ok(e) => e,
        Err(_) => return paths,
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    for name in names {
        if let Some(record) = read_record(&format!("{res_dir}/{name}"))
            && let Some(p) = record.get("sessionPath").and_then(Value::as_str)
            && !p.is_empty()
        {
            paths.push(p.to_owned());
        }
    }
    paths
}

pub fn list_registry_endpoints(agent_dir: &str) -> Vec<RegistryEndpoint> {
    let flat_dir = format!("{agent_dir}/rpc-host-daemon");
    let marker = match read_record(&format!("{flat_dir}/layout.json")) {
        Some(m) => m,
        None => return Vec::new(),
    };
    if marker.get("layout").and_then(Value::as_i64) != Some(ENDPOINT_REGISTRY_LAYOUT) {
        return Vec::new();
    }
    let entries = match fs::read_dir(&flat_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|d| identify(&d.to_string_lossy()))
        .collect()
}

pub fn endpoint_kind_of(socket: &str, registry: &[RegistryEndpoint]) -> EndpointKind {
    if is_tui_control_socket(socket) {
        return EndpointKind::Tui;
    }
    let canonical = canonical_endpoint_path(socket);
    let recorded = registry.iter().find(|e| {
        if let Some(s) = &e.socket {
            s == socket || canonical_endpoint_path(s) == canonical
        } else {
            false
        }
    });
    recorded
        .map(|e| e.endpoint_kind.clone())
        .unwrap_or(EndpointKind::RpcHost)
}

// re-export alias
pub fn endpoint_kind_to_gateway(s: &EndpointKind) -> &'static str {
    match s {
        EndpointKind::RpcHost => "rpc_host",
        EndpointKind::Tui => "tui",
    }
}
