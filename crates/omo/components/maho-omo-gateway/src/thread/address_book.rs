//! The cross-project thread address book.
//!
//! Upstream counterpart: `thread/address-book.ts`. This function retains no process state: host
//! liveness is recomputed solely from this call's host results, and durable disk sessions are the
//! only fallback when a host disappears.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::adapter::{EndpointLiveness, GatewayEndpointRef};
use super::contracts_results::ThreadStatus;
use super::engine::GatewayAddressEntry;
use super::types::GatewayEndpointKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSessionStatus {
    Opening,
    Open,
    Closing,
    Closed,
}

impl HostSessionStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "opening" => Some(Self::Opening),
            "open" => Some(Self::Open),
            "closing" => Some(Self::Closing),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSession {
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: String,
    pub name: Option<String>,
    pub status: Option<HostSessionStatus>,
    pub kind: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// What a host's `list_sessions` probe answered: the sessions, or the failure it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostListSessions {
    pub sessions: Vec<HostSession>,
    pub error: Option<String>,
}

impl HostListSessions {
    pub fn ok(sessions: Vec<HostSession>) -> Self {
        Self {
            sessions,
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            sessions: Vec::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressBookHost {
    pub socket: String,
    pub list_sessions: Option<HostListSessions>,
    pub endpoint_kind: Option<GatewayEndpointKind>,
    /// The engine's liveness verdict for the endpoint, when it gave one.
    pub alive: Option<bool>,
    pub reason: Option<String>,
    pub legacy: bool,
}

/// Where a thread runs: a terminal, a Desktop thread shard, a task child, or the shared daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSurface {
    Tui,
    Desktop,
    Child,
    Daemon,
}

impl ThreadSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
            Self::Child => "child",
            Self::Daemon => "daemon",
        }
    }
}

pub const SESSION_FACTS_WINDOW_BYTES: usize = 64 * 1024;
pub const SESSION_FACTS_LAST_LINE_MAX_BYTES: usize = SESSION_FACTS_WINDOW_BYTES * 4;
pub const THREAD_TITLE_MAX_CHARS: usize = 60;
const SESSION_ARCHIVED_CUSTOM_TYPE: &str = "session_archived";

/// A session read from disk, for a thread no endpoint lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSession {
    pub durable_id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub session_path: String,
    pub source_host: Option<String>,
    pub first_user_text: Option<String>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEndpoint {
    pub kind: GatewayEndpointKind,
    pub socket: String,
    pub routing_id: Option<String>,
    pub durable_session_id: String,
}

/// Cross-project address record; the snake-case summary fields let the addressing layer consume it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEntry {
    pub durable_id: String,
    pub routing_id: Option<String>,
    pub name: Option<String>,
    pub cwd: String,
    pub status: ThreadStatus,
    pub liveness: ThreadStatus,
    pub source_host: Option<String>,
    pub session_path: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub thread_id: String,
    pub error_note: Option<String>,
    pub endpoint: Option<AddressEndpoint>,
    pub surface: Option<ThreadSurface>,
    pub alive: bool,
    pub title: Option<String>,
    pub archived: bool,
}

/// The title a listing shows: the name, else the first user message's opening, never the durable id.
pub fn thread_title(name: Option<&str>, first_user_text: Option<&str>) -> Option<String> {
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        return Some(name.to_owned());
    }
    first_user_text.and_then(|text| {
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!collapsed.is_empty()).then(|| collapsed.chars().take(THREAD_TITLE_MAX_CHARS).collect())
    })
}

/// senpi's surface for a session, from what serves it.
pub fn derive_surface(
    endpoint_kind: Option<GatewayEndpointKind>,
    session_kind: Option<&str>,
    socket: &str,
) -> ThreadSurface {
    if endpoint_kind == Some(GatewayEndpointKind::Tui) {
        return ThreadSurface::Tui;
    }
    if session_kind == Some("worker") {
        return ThreadSurface::Child;
    }
    let name = Path::new(socket)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.starts_with("i-") {
        return ThreadSurface::Desktop;
    }
    if name.starts_with("p-") {
        return ThreadSurface::Child;
    }
    ThreadSurface::Daemon
}

fn host_failure(host: &AddressBookHost) -> Option<String> {
    if host.reason.as_deref() == Some("live_unresponsive") {
        return Some("live_unresponsive".to_owned());
    }
    let result = host.list_sessions.as_ref()?;
    if let Some(error) = &result.error {
        return Some(error.clone());
    }
    None
}

fn from_disk(
    session: &DiskSession,
    host: Option<&AddressBookHost>,
    failure: Option<&str>,
) -> AddressEntry {
    let endpoint = session.source_host.as_ref().map(|socket| AddressEndpoint {
        kind: host
            .and_then(|host| host.endpoint_kind)
            .unwrap_or(GatewayEndpointKind::RpcHost),
        socket: socket.clone(),
        routing_id: None,
        durable_session_id: session.durable_id.clone(),
    });
    AddressEntry {
        durable_id: session.durable_id.clone(),
        routing_id: None,
        name: session.name.clone(),
        cwd: session.cwd.clone(),
        status: ThreadStatus::Resumable,
        liveness: ThreadStatus::Resumable,
        source_host: session.source_host.clone(),
        session_path: Some(session.session_path.clone()),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        thread_id: session.durable_id.clone(),
        error_note: failure.map(str::to_owned),
        surface: endpoint
            .as_ref()
            .map(|endpoint| derive_surface(Some(endpoint.kind), None, &endpoint.socket)),
        endpoint,
        alive: false,
        title: thread_title(session.name.as_deref(), session.first_user_text.as_deref()),
        archived: session.archived,
    }
}

/// Assemble one fresh view from this call's host results and the durable disk sessions.
pub fn assemble_address_book(
    hosts: &[AddressBookHost],
    disk_sessions: &[DiskSession],
    assembled_at: &str,
) -> Vec<AddressEntry> {
    let mut by_durable_id: BTreeMap<String, AddressEntry> = BTreeMap::new();
    let failures: BTreeMap<String, String> = hosts
        .iter()
        .filter_map(|host| host_failure(host).map(|failure| (host.socket.clone(), failure)))
        .collect();
    let by_socket: BTreeMap<String, &AddressBookHost> = hosts
        .iter()
        .map(|host| (host.socket.clone(), host))
        .collect();

    for session in disk_sessions {
        if session.durable_id.is_empty() {
            continue;
        }
        // The workspace scan reads a file an endpoint already claimed with no endpoint: keep the endpoint's
        // record, which carries its failure (`live_unresponsive`), so the thread never reads as plain closed (#14308).
        if session.source_host.is_none()
            && by_durable_id
                .get(&session.durable_id)
                .and_then(|existing| existing.source_host.as_ref())
                .is_some()
        {
            continue;
        }
        let failure = session
            .source_host
            .as_ref()
            .and_then(|socket| failures.get(socket))
            .map(String::as_str);
        let host = session
            .source_host
            .as_ref()
            .and_then(|socket| by_socket.get(socket))
            .copied();
        by_durable_id.insert(
            session.durable_id.clone(),
            from_disk(session, host, failure),
        );
    }

    for host in hosts {
        let Some(listed) = &host.list_sessions else {
            continue;
        };
        if listed.error.is_some() {
            continue;
        }
        for session in &listed.sessions {
            if session.status == Some(HostSessionStatus::Closed) {
                continue;
            }
            // A terminal endpoint lists its one session by its durable id: there is no routing layer.
            let durable_id = session.durable_session_id.clone().or_else(|| {
                (host.endpoint_kind == Some(GatewayEndpointKind::Tui))
                    .then(|| session.session_id.clone())
            });
            let Some(durable_id) = durable_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            let disk = by_durable_id.get(&durable_id);
            let session_path = session
                .session_path
                .clone()
                .or_else(|| disk.and_then(|disk| disk.session_path.clone()));
            let created_at = session
                .created_at
                .clone()
                .or_else(|| disk.map(|disk| disk.created_at.clone()))
                .unwrap_or_else(|| assembled_at.to_owned());
            let updated_at = session
                .updated_at
                .clone()
                .or_else(|| disk.and_then(|disk| disk.updated_at.clone()));
            let name = session
                .name
                .clone()
                .or_else(|| disk.and_then(|disk| disk.name.clone()));
            let disk_title = disk.and_then(|disk| {
                disk.title
                    .clone()
                    .or_else(|| thread_title(disk.name.as_deref(), None))
            });
            let kind = host.endpoint_kind.unwrap_or(GatewayEndpointKind::RpcHost);
            by_durable_id.insert(
                durable_id.clone(),
                AddressEntry {
                    durable_id: durable_id.clone(),
                    routing_id: Some(session.session_id.clone()),
                    name: name.clone(),
                    cwd: session.cwd.clone(),
                    status: ThreadStatus::Live,
                    liveness: ThreadStatus::Live,
                    source_host: Some(host.socket.clone()),
                    session_path,
                    created_at,
                    updated_at,
                    thread_id: durable_id.clone(),
                    error_note: None,
                    endpoint: Some(AddressEndpoint {
                        kind,
                        socket: host.socket.clone(),
                        routing_id: Some(session.session_id.clone()),
                        durable_session_id: durable_id,
                    }),
                    surface: Some(derive_surface(
                        Some(kind),
                        session.kind.as_deref(),
                        &host.socket,
                    )),
                    alive: host.alive != Some(false),
                    title: thread_title(name.as_deref(), None).or_else(|| disk_title.clone()),
                    archived: false,
                },
            );
        }
    }

    let mut entries: Vec<AddressEntry> = by_durable_id.into_values().collect();
    entries.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (Some(left_updated), Some(right_updated)) if left_updated != right_updated => {
            right_updated.cmp(left_updated)
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => left.durable_id.cmp(&right.durable_id),
    });
    entries
}

/// The gateway engine's view: a thread is `routable` only while an endpoint that answered lists it.
pub fn to_gateway_address_entries(entries: &[AddressEntry]) -> Vec<GatewayAddressEntry> {
    entries
        .iter()
        .map(|entry| GatewayAddressEntry {
            thread_id: entry.thread_id.clone(),
            name: entry.name.clone().unwrap_or_default(),
            endpoint: entry.endpoint.as_ref().map(|endpoint| GatewayEndpointRef {
                kind: endpoint.kind,
                socket: endpoint.socket.clone(),
                routing_id: endpoint.routing_id.clone(),
                durable_session_id: endpoint.durable_session_id.clone(),
            }),
            liveness: if entry.alive {
                EndpointLiveness::Routable
            } else if entry.error_note.as_deref() == Some("live_unresponsive") {
                EndpointLiveness::LiveUnresponsive
            } else {
                EndpointLiveness::Dead
            },
        })
        .collect()
}

fn json_str(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key)?.as_str().map(str::to_owned)
}

fn read_window(file: &mut std::fs::File, position: u64, length: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(position)).ok()?;
    let mut buffer = vec![0u8; length];
    let mut read_total = 0usize;
    while read_total < length {
        match file.read(&mut buffer[read_total..]) {
            Ok(0) => break,
            Ok(n) => read_total += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    buffer.truncate(read_total);
    Some(buffer)
}

fn parse_session_lines(text: &str, drop_first: bool, drop_last: bool) -> Vec<Value> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if drop_first && !lines.is_empty() {
        lines.remove(0);
    }
    if drop_last {
        lines.pop();
    }
    lines
        .into_iter()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

fn message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                (part.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| part.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn final_complete_line(file: &mut std::fs::File, size: u64) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    let last = read_window(file, size - 1, 1)?;
    if last.as_slice() != [b'\n'] {
        return None;
    }
    let mut remaining = size - 1;
    let mut scanned = 0usize;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while remaining > 0 && scanned < SESSION_FACTS_LAST_LINE_MAX_BYTES {
        let length = SESSION_FACTS_WINDOW_BYTES
            .min(remaining as usize)
            .min(SESSION_FACTS_LAST_LINE_MAX_BYTES - scanned);
        let start = remaining - length as u64;
        let chunk = read_window(file, start, length)?;
        if chunk.len() != length {
            return None;
        }
        if let Some(newline) = chunk.iter().rposition(|byte| *byte == b'\n') {
            chunks.push(chunk[newline + 1..].to_vec());
            chunks.reverse();
            return Some(chunks.concat());
        }
        chunks.push(chunk);
        if start == 0 {
            chunks.reverse();
            return Some(chunks.concat());
        }
        scanned += length;
        remaining = start;
    }
    None
}

/// One session file's facts, read from bounded head and tail windows (`session-facts.ts`), or none when unreadable or
/// lacking a valid session header.
pub fn read_disk_session(path: &Path, source_host: Option<&str>) -> Option<DiskSession> {
    let mut file = std::fs::File::open(path).ok()?;
    let before = file.metadata().ok()?;
    let size = before.len();
    if size == 0 {
        return None;
    }
    let head_length = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .min(SESSION_FACTS_WINDOW_BYTES);
    let head_bytes = read_window(&mut file, 0, head_length)?;
    let whole = size <= SESSION_FACTS_WINDOW_BYTES as u64;
    let head_entries = parse_session_lines(&String::from_utf8_lossy(&head_bytes), false, !whole);
    let first = head_entries.first()?;
    if first
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "session")
    {
        return None;
    }
    let durable_id = json_str(Some(first), "id")
        .or_else(|| json_str(Some(first), "session_id"))
        .or_else(|| json_str(Some(first), "sessionId"))
        .or_else(|| json_str(Some(first), "durable_id"))
        .filter(|id| !id.is_empty())?;
    let cwd = json_str(Some(first), "cwd").unwrap_or_default();
    let created_at = json_str(Some(first), "timestamp")
        .or_else(|| json_str(Some(first), "created_at"))
        .or_else(|| json_str(Some(first), "createdAt"))
        .unwrap_or_default();
    let mut tail_entries: Vec<Value> = Vec::new();
    if !whole {
        let tail_start = size - SESSION_FACTS_WINDOW_BYTES as u64;
        let tail_bytes = read_window(&mut file, tail_start - 1, SESSION_FACTS_WINDOW_BYTES + 1)?;
        if let Some((&before_byte, body)) = tail_bytes.split_first() {
            tail_entries =
                parse_session_lines(&String::from_utf8_lossy(body), before_byte != b'\n', false);
        }
    }
    let mut name = json_str(Some(first), "name")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let mut first_user_text: Option<String> = None;
    let mut archived = false;
    for entry in head_entries.iter().chain(tail_entries.iter()) {
        let kind = entry.get("type").and_then(Value::as_str);
        if kind == Some("session_info") {
            name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
        } else if kind == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(SESSION_ARCHIVED_CUSTOM_TYPE)
        {
            let data = entry.get("data");
            let same_session = data
                .and_then(|data| data.get("sessionId"))
                .and_then(Value::as_str)
                .is_none_or(|id| id == durable_id);
            if same_session {
                archived = data
                    .and_then(|data| data.get("archived"))
                    .and_then(Value::as_bool)
                    == Some(true);
            }
        } else if first_user_text.is_none()
            && kind == Some("message")
            && let Some(message) = entry.get("message").and_then(Value::as_object)
            && message.get("role").and_then(Value::as_str) == Some("user")
        {
            let text = message_text(message.get("content")).trim().to_owned();
            if !text.is_empty() {
                first_user_text = Some(text);
            }
        }
    }
    let final_line = final_complete_line(&mut file, size);
    let after = file.metadata().ok();
    let unchanged = after.as_ref().is_some_and(|meta| {
        meta.len() == before.len() && meta.modified().ok() == before.modified().ok()
    });
    let updated_at = if !unchanged {
        None
    } else if let Some(line_bytes) = final_line {
        serde_json::from_slice::<Value>(&line_bytes)
            .ok()
            .filter(Value::is_object)
            .and_then(|entry| {
                entry
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .filter(|timestamp| !timestamp.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        (head_entries.len() == 1 && tail_entries.is_empty())
                            .then(|| {
                                json_str(Some(first), "updated_at")
                                    .or_else(|| json_str(Some(first), "updatedAt"))
                            })
                            .flatten()
                    })
            })
    } else {
        None
    };
    Some(DiskSession {
        durable_id,
        name,
        cwd,
        created_at,
        updated_at,
        session_path: path.to_string_lossy().into_owned(),
        source_host: source_host.map(str::to_owned),
        first_user_text,
        archived,
    })
}

/// senpi's session directory name for a cwd, without the closing `--`.
pub fn session_directory_prefix(root: &str) -> String {
    let stripped = root
        .trim_start_matches(['/', '\\'])
        .replace(['/', '\\', ':'], "-");
    format!("--{stripped}-")
}

fn project_directories(sessions_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.starts_with("--") && name.ends_with("--"))
        .collect()
}

fn session_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("jsonl"))
        .collect()
}

/// The session files an address names, for a session no endpoint lists.
pub fn find_disk_sessions(
    sessions_dir: &Path,
    address: &str,
    all_scope: bool,
    workspace_roots: &[String],
) -> Vec<DiskSession> {
    let directories = project_directories(sessions_dir);
    let suffix = format!("_{address}.jsonl");
    let mut by_id: Vec<DiskSession> = Vec::new();
    for directory in &directories {
        for file in session_files(&sessions_dir.join(directory)) {
            if file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
                && let Some(session) = read_disk_session(&file, None)
                && session.durable_id == address
            {
                by_id.push(session);
            }
        }
    }
    if !by_id.is_empty() {
        return by_id;
    }
    let wanted = address.trim().to_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    let prefixes: Vec<String> = if all_scope {
        Vec::new()
    } else {
        workspace_roots
            .iter()
            .map(|root| session_directory_prefix(root))
            .collect()
    };
    // The directory name maps both `/` and `-` to `-`, so `<root>-tmp` shares `<root>`'s prefix (#14893): the header's
    // cwd decides, compared by path components.
    let roots: Vec<&Path> = workspace_roots.iter().map(Path::new).collect();
    let in_scope =
        |cwd: &str| all_scope || roots.iter().any(|root| Path::new(cwd).starts_with(root));
    directories
        .into_iter()
        .filter(|directory| {
            prefixes.is_empty() || prefixes.iter().any(|prefix| directory.starts_with(prefix))
        })
        .flat_map(|directory| {
            session_files(&sessions_dir.join(&directory))
                .into_iter()
                .filter_map(|file| read_disk_session(&file, None))
                .collect::<Vec<_>>()
        })
        .filter(|session| {
            session
                .name
                .as_deref()
                .is_some_and(|name| name.trim().to_lowercase() == wanted)
                && in_scope(&session.cwd)
        })
        .collect()
}

/// Scan the `sessions/--<encoded-cwd>--/*.jsonl` layout; cwd and durable identity come from the header.
pub fn scan_disk_sessions(sessions_dir: &Path, source_host: Option<&str>) -> Vec<DiskSession> {
    let mut found: Vec<DiskSession> = project_directories(sessions_dir)
        .into_iter()
        .flat_map(|directory| session_files(&sessions_dir.join(directory)))
        .filter_map(|file| read_disk_session(&file, source_host))
        .collect();
    found.sort_by(|left, right| match (&left.updated_at, &right.updated_at) {
        (Some(left_updated), Some(right_updated)) if left_updated != right_updated => {
            right_updated.cmp(left_updated)
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => left.durable_id.cmp(&right.durable_id),
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(durable_id: &str) -> DiskSession {
        DiskSession {
            durable_id: durable_id.into(),
            name: Some("Receiver".into()),
            cwd: "/work".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: Some("2026-01-02T00:00:00.000Z".into()),
            session_path: format!("/sessions/{durable_id}.jsonl"),
            source_host: Some("/run/i-host.sock".into()),
            first_user_text: None,
            archived: false,
        }
    }

    fn host(sessions: Vec<HostSession>) -> AddressBookHost {
        AddressBookHost {
            socket: "/run/i-host.sock".into(),
            list_sessions: Some(HostListSessions::ok(sessions)),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: Some(true),
            reason: None,
            legacy: false,
        }
    }

    fn live_session() -> HostSession {
        HostSession {
            session_id: "r-1".into(),
            durable_session_id: Some("d-1".into()),
            session_path: Some("/sessions/d-1.jsonl".into()),
            cwd: "/work".into(),
            name: Some("Receiver".into()),
            status: Some(HostSessionStatus::Open),
            kind: Some("interactive".into()),
            created_at: Some("2026-01-01T00:00:00.000Z".into()),
            updated_at: Some("2026-01-03T00:00:00.000Z".into()),
        }
    }

    #[test]
    fn a_live_host_listing_turns_a_disk_session_into_a_live_routable_entry() {
        let entries = assemble_address_book(
            &[host(vec![live_session()])],
            &[disk("d-1")],
            "2026-01-04T00:00:00.000Z",
        );
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.status, ThreadStatus::Live);
        assert_eq!(entry.routing_id.as_deref(), Some("r-1"));
        assert_eq!(entry.surface, Some(ThreadSurface::Desktop));
        assert!(entry.alive);
        assert_eq!(
            to_gateway_address_entries(&entries)[0].liveness,
            EndpointLiveness::Routable
        );
    }

    #[test]
    fn a_dead_host_falls_back_to_the_disk_session_with_its_error_note() {
        let mut dead = host(vec![]);
        dead.list_sessions = Some(HostListSessions::failed("connection refused"));
        let entries = assemble_address_book(&[dead], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(entries[0].status, ThreadStatus::Resumable);
        assert_eq!(entries[0].error_note.as_deref(), Some("connection refused"));
        assert!(!entries[0].alive);
        assert_eq!(
            to_gateway_address_entries(&entries)[0].liveness,
            EndpointLiveness::Dead
        );
    }

    #[test]
    fn a_live_unresponsive_endpoint_stays_unresponsive_never_gone() {
        let mut unresponsive = host(vec![]);
        unresponsive.reason = Some("live_unresponsive".into());
        unresponsive.alive = Some(false);
        let entries =
            assemble_address_book(&[unresponsive], &[disk("d-1")], "2026-01-04T00:00:00.000Z");
        assert_eq!(
            to_gateway_address_entries(&entries)[0].liveness,
            EndpointLiveness::LiveUnresponsive
        );
    }

    #[test]
    fn the_surface_follows_what_serves_the_session() {
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::Tui), None, "/run/tui-1.sock"),
            ThreadSurface::Tui
        );
        assert_eq!(
            derive_surface(
                Some(GatewayEndpointKind::RpcHost),
                Some("worker"),
                "/run/x.sock"
            ),
            ThreadSurface::Child
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/i-7.sock"),
            ThreadSurface::Desktop
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/p-7.sock"),
            ThreadSurface::Child
        );
        assert_eq!(
            derive_surface(Some(GatewayEndpointKind::RpcHost), None, "/run/host.sock"),
            ThreadSurface::Daemon
        );
    }

    #[test]
    fn a_title_never_falls_back_to_the_durable_id() {
        assert_eq!(
            thread_title(Some("Build"), Some("first words")),
            Some("Build".to_owned())
        );
        assert_eq!(
            thread_title(None, Some("first words")),
            Some("first words".to_owned())
        );
        assert_eq!(thread_title(None, None), None);
    }

    #[test]
    fn a_session_directory_prefix_encodes_the_workspace() {
        assert_eq!(
            session_directory_prefix("/home/indo/work"),
            "--home-indo-work-"
        );
    }

    #[test]
    fn a_scoped_name_lookup_skips_a_sibling_directory_that_extends_the_workspace_path() {
        let base = tempfile::tempdir().expect("tempdir");
        let sessions = base.path().join("sessions");
        let root = base.path().join("fix-guard");
        let write = |id: &str, cwd: &Path| -> String {
            let cwd = cwd.to_string_lossy().into_owned();
            let directory = sessions.join(format!("{}-", session_directory_prefix(&cwd)));
            std::fs::create_dir_all(&directory).expect("session directory");
            let path = directory.join(format!("2026-11-24T00-00-00-000Z_{id}.jsonl"));
            let lines = [
                serde_json::json!({
                    "type": "session",
                    "version": 3,
                    "id": id,
                    "cwd": cwd,
                    "timestamp": "2026-11-24T00:00:00.000Z",
                }),
                serde_json::json!({
                    "type": "session_info",
                    "timestamp": "2026-11-24T00:01:00.000Z",
                    "name": "lane",
                }),
            ];
            let content = lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            std::fs::write(&path, content).expect("write");
            path.to_string_lossy().into_owned()
        };
        let at_root = write("dur-root", &root);
        let below = write("dur-below", &root.join("packages"));
        write("dur-sibling", &base.path().join("fix-guard-tmp"));
        let roots = [root.to_string_lossy().into_owned()];
        let mut found: Vec<String> = find_disk_sessions(&sessions, "lane", false, &roots)
            .into_iter()
            .map(|session| session.session_path)
            .collect();
        found.sort();
        let mut expected = vec![at_root, below];
        expected.sort();
        assert_eq!(found, expected);
        assert_eq!(find_disk_sessions(&sessions, "lane", true, &[]).len(), 3);
    }

    #[test]
    fn workspace_scan_does_not_overwrite_a_claimed_unresponsive_endpoint_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dur-stopped.jsonl");
        let content = [
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "dur-stopped",
                "cwd": "/work",
                "timestamp": "2026-11-01T00:00:00.000Z",
            }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-01T00:01:00.000Z",
                "message": { "role": "user", "content": "fix the build" },
            }),
        ]
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
            + "\n";
        std::fs::write(&path, content).expect("write");

        let socket = "/run/t-stopped.sock";
        let claimed = read_disk_session(&path, Some(socket)).expect("claimed");
        let scanned = read_disk_session(&path, None).expect("scanned");
        let unresponsive = AddressBookHost {
            socket: socket.to_owned(),
            list_sessions: None,
            endpoint_kind: Some(GatewayEndpointKind::Tui),
            alive: Some(false),
            reason: Some("live_unresponsive".into()),
            legacy: false,
        };
        for order in [
            vec![claimed.clone(), scanned.clone()],
            vec![scanned, claimed],
        ] {
            let entries = assemble_address_book(
                std::slice::from_ref(&unresponsive),
                &order,
                "2026-11-02T00:00:00.000Z",
            );
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].durable_id, "dur-stopped");
            assert_eq!(entries[0].error_note.as_deref(), Some("live_unresponsive"));
            assert_eq!(entries[0].source_host.as_deref(), Some(socket));
            assert_eq!(entries[0].title.as_deref(), Some("fix the build"));
        }
    }

    #[test]
    fn read_disk_session_extracts_info_first_user_text_archived_and_bounded_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        let lines = [
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "dur-1",
                "cwd": "/work",
                "timestamp": "2026-11-14T01:00:00.000Z",
            }),
            serde_json::json!({
                "type": "message",
                "timestamp": "2026-11-14T01:01:00.000Z",
                "message": { "role": "user", "content": [{ "type": "text", "text": "  plan   the move  " }] },
            }),
            serde_json::json!({
                "type": "session_info",
                "timestamp": "2026-11-14T01:02:00.000Z",
                "name": "Initial",
            }),
            serde_json::json!({
                "type": "session_info",
                "timestamp": "2026-11-14T01:03:00.000Z",
                "name": "   ",
            }),
            serde_json::json!({
                "type": "custom",
                "customType": "session_archived",
                "data": { "archived": true },
                "timestamp": "2026-11-14T01:04:00.000Z",
            }),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .expect("write");

        let facts = read_disk_session(&path, None).expect("facts");
        assert_eq!(facts.durable_id, "dur-1");
        assert_eq!(facts.name, None);
        assert_eq!(facts.first_user_text.as_deref(), Some("plan   the move"));
        assert_eq!(
            thread_title(facts.name.as_deref(), facts.first_user_text.as_deref()).as_deref(),
            Some("plan the move")
        );
        assert!(facts.archived);
        assert_eq!(
            facts.updated_at.as_deref(),
            Some("2026-11-14T01:04:00.000Z")
        );

        let unarchived = serde_json::json!({
            "type": "custom",
            "customType": "session_archived",
            "data": { "archived": false },
            "timestamp": "2026-11-14T01:05:00.000Z",
        });
        let mut content = std::fs::read_to_string(&path).expect("read");
        content.push_str(&format!("{unarchived}\n"));
        std::fs::write(&path, content).expect("rewrite");
        let after = read_disk_session(&path, None).expect("after");
        assert!(!after.archived);
        assert_eq!(
            after.updated_at.as_deref(),
            Some("2026-11-14T01:05:00.000Z")
        );

        let truncated = dir.path().join("truncated.jsonl");
        std::fs::write(
            &truncated,
            format!(
                "{}\n{{\"type\":\"message\",\"timestamp\":\"2026-11-14T02:00:00.000Z\"",
                lines[0]
            ),
        )
        .expect("truncated");
        assert_eq!(
            read_disk_session(&truncated, None)
                .expect("truncated facts")
                .updated_at,
            None
        );

        let message_only = dir.path().join("no-header.jsonl");
        std::fs::write(&message_only, format!("{}\n", lines[1])).expect("no header");
        assert_eq!(read_disk_session(&message_only, None), None);
    }
}
