//! Port of omo `packages/omo-native/sync-status-cli.ts` and `sync-engine-status.ts` (omo#14437,
//! omo#14456): `mhc sync status`.
//!
//! A read-only report of this device's memory sync: the account, this device's join and switch, the
//! account's device rows, pending join requests and whether an engine runs on this machine. It writes
//! no sync state, and every field is an id, a flag, a time or a count, never key material. Each
//! section reports its OWN failure, so one unreadable source cannot hide the other three.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use memory_core::sync::crypto::trust_pin::PinLoad;
use serde_json::{Value, json};

use crate::context::RemoteContext;
use crate::exit::RemoteExit;
use crate::usage::is_help_flag;

/// One report section: its own value, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section<T> {
    Ok(T),
    Failed(String),
}

impl<T> Section<T> {
    fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok(_) => None,
            Self::Failed(reason) => Some(reason),
        }
    }
}

/// One device row of the account, as the local service reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusDevice {
    pub id: String,
    pub name: String,
    /// Absent when the service is older than the field.
    pub syncing: Option<bool>,
    pub last_seen_at: Option<String>,
    pub current: bool,
}

/// This machine's engine host, as its operator socket answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSyncRead {
    /// No engine host listens on this machine's operator socket.
    NoEngine,
    /// An engine host listens; its sync state is shown inside its sessions.
    Running,
    /// The socket could not be checked; the reason names why.
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    pub joined: bool,
    /// On unless this device's own switch is off.
    pub switch_on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingJoin {
    pub id: String,
    pub expires_at: String,
    pub answered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatusReport {
    pub account: String,
    pub device_id: String,
    pub device: Section<DeviceStatus>,
    pub devices: Section<Vec<StatusDevice>>,
    pub pending: Section<Vec<PendingJoin>>,
    pub engine: EngineSyncRead,
}

impl SyncStatusReport {
    /// The report as one JSON object, in the upstream field order.
    pub fn to_json(&self) -> Value {
        let device = match &self.device {
            Section::Ok(device) => json!({
                "ok": true,
                "joined": device.joined,
                "switch": if device.switch_on { "on" } else { "off" },
            }),
            Section::Failed(reason) => json!({ "ok": false, "reason": reason }),
        };
        let devices = match &self.devices {
            Section::Ok(list) => json!({
                "ok": true,
                "list": list.iter().map(device_json).collect::<Vec<_>>(),
            }),
            Section::Failed(reason) => json!({ "ok": false, "reason": reason }),
        };
        let pending = match &self.pending {
            Section::Ok(list) => json!({
                "ok": true,
                "list": list
                    .iter()
                    .map(|row| json!({
                        "id": row.id,
                        "expiresAt": row.expires_at,
                        "answered": row.answered,
                    }))
                    .collect::<Vec<_>>(),
            }),
            Section::Failed(reason) => json!({ "ok": false, "reason": reason }),
        };
        let engine = match &self.engine {
            EngineSyncRead::NoEngine => json!({ "kind": "no_engine" }),
            EngineSyncRead::Running => json!({ "kind": "running" }),
            EngineSyncRead::Unreadable(reason) => json!({ "kind": "unreadable", "reason": reason }),
        };
        json!({
            "account": self.account,
            "deviceId": self.device_id,
            "device": device,
            "devices": devices,
            "pending": pending,
            "engine": engine,
        })
    }

    /// The human report; one line per fact, one section per paragraph.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Account: {}", self.account),
            match &self.device {
                Section::Ok(device) => format!(
                    "This device: {}, {}, sync {}",
                    self.device_id,
                    if device.joined {
                        "joined"
                    } else {
                        "not joined"
                    },
                    if device.switch_on { "on" } else { "off" }
                ),
                Section::Failed(reason) => format!(
                    "This device: {}, local sync state could not be read ({reason}).",
                    self.device_id
                ),
            },
        ];
        match &self.devices {
            Section::Ok(list) => {
                lines.push("Devices:".to_owned());
                for device in list {
                    let membership = match device.syncing {
                        None => "sync unknown",
                        Some(true) => "syncing",
                        Some(false) => "not syncing",
                    };
                    let seen = device
                        .last_seen_at
                        .as_ref()
                        .map(|seen| format!(", last seen {seen}"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "  {}{} ({}): {membership}{seen}",
                        if device.current { "* " } else { "  " },
                        device.name,
                        device.id
                    ));
                }
            }
            Section::Failed(reason) => {
                lines.push(format!("Devices: could not be read ({reason})."));
            }
        }
        match &self.pending {
            Section::Failed(reason) => {
                lines.push(format!(
                    "Pending join requests: could not be read ({reason})."
                ));
            }
            Section::Ok(list) if list.is_empty() => {
                lines.push("Pending join requests: none".to_owned());
            }
            Section::Ok(list) => {
                lines.push("Pending join requests:".to_owned());
                for row in list {
                    lines.push(format!(
                        "  {}: expires {}{}",
                        row.id,
                        row.expires_at,
                        if row.answered {
                            ", answered"
                        } else {
                            ", waiting"
                        }
                    ));
                }
            }
        }
        lines.push(match &self.engine {
            EngineSyncRead::NoEngine => "Engine: no engine is running on this machine.".to_owned(),
            EngineSyncRead::Running => "Engine: running. Its memory sync details are not readable from the CLI yet; the TUI footer and the desktop app show them.".to_owned(),
            EngineSyncRead::Unreadable(reason) => format!("Engine: could not be checked ({reason})."),
        });
        lines
    }

    /// Whether no section could be read: nothing a person can act on, so the verb fails.
    fn sections_all_failed(&self) -> bool {
        self.device.reason().is_some()
            && self.devices.reason().is_some()
            && self.pending.reason().is_some()
    }
}

fn device_json(device: &StatusDevice) -> Value {
    let mut value = json!({
        "id": device.id,
        "name": device.name,
        "current": device.current,
    });
    if let Some(syncing) = device.syncing {
        value["syncing"] = Value::Bool(syncing);
    }
    if let Some(seen) = &device.last_seen_at {
        value["lastSeenAt"] = Value::String(seen.clone());
    }
    value
}

/// `resolveTaskHostSocket`: `RPC_SOCKET`, then `<agent dir>/rpc/rpc.sock`, matching `mhc host`'s own
/// resolution so the probe looks at the socket this machine's engine binds.
pub fn operator_socket(agent_dir: &Path) -> PathBuf {
    maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| agent_dir.join("rpc/rpc.sock"))
}

/// Whether this machine's engine host is running (omo#14456). It does not read that engine's memory
/// sync state: the engine answers it only inside a session, and reaching one from here would attach
/// to (and so resume and re-park) a session another surface owns. The probe connects and closes
/// without sending a request, which neither authenticates nor attaches.
pub fn read_engine_sync_status(agent_dir: &Path) -> EngineSyncRead {
    probe_socket(&operator_socket(agent_dir))
}

#[cfg(unix)]
fn probe_socket(socket: &Path) -> EngineSyncRead {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixStream;
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if !metadata.file_type().is_socket() => return EngineSyncRead::NoEngine,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return EngineSyncRead::NoEngine;
        }
        Err(error) => return EngineSyncRead::Unreadable(error.to_string()),
    }
    match UnixStream::connect(socket) {
        Ok(stream) => {
            drop(stream);
            EngineSyncRead::Running
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            EngineSyncRead::NoEngine
        }
        Err(error) => {
            EngineSyncRead::Unreadable(format!("the engine host could not be checked: {error}"))
        }
    }
}

#[cfg(not(unix))]
fn probe_socket(_socket: &Path) -> EngineSyncRead {
    EngineSyncRead::Unreadable("the operator endpoint is not a socket here".to_owned())
}

/// Where the per-account sync marks live, beside the memory repositories.
pub fn sync_marks_dir(env: &BTreeMap<String, String>, cwd: &Path) -> PathBuf {
    memory_core::sync::pin_dir::sync_pin_dir(memory_core::identity::layout::resolve_memory_root(
        env, cwd,
    ))
}

fn read_device(pin_dir: &Path, account: &str) -> Section<DeviceStatus> {
    let joined = match memory_core::sync::crypto::trust_pin::load_trust_anchor(pin_dir, account) {
        Ok(PinLoad::Loaded { .. }) => true,
        Ok(PinLoad::Refused { .. }) => false,
        Err(error) => return Section::Failed(error.to_string()),
    };
    match memory_core::sync::account::device_switch::is_device_sync_off(pin_dir, account) {
        Ok(off) => Section::Ok(DeviceStatus {
            joined,
            switch_on: !off,
        }),
        Err(error) => Section::Failed(error.to_string()),
    }
}

/// The account's device rows, from the same local API read `mhc remote devices` uses.
async fn read_devices(ctx: &RemoteContext) -> Section<Vec<StatusDevice>> {
    let client = match ctx.connect().await {
        Ok(client) => client,
        Err(error) => return Section::Failed(error.reason),
    };
    let rows = client.devices().await;
    client.close().await;
    match rows {
        Ok(rows) => Section::Ok(
            rows.iter()
                .map(|row| StatusDevice {
                    id: row["device"].as_str().unwrap_or_default().to_owned(),
                    name: row["label"].as_str().unwrap_or_default().to_owned(),
                    // The local service carries no per-device sync flag; upstream omits a field an
                    // older service does not send rather than guessing `false`.
                    syncing: row.get("syncing").and_then(Value::as_bool),
                    last_seen_at: row
                        .get("last_seen_ms")
                        .and_then(Value::as_i64)
                        .and_then(iso_of_ms),
                    current: row.get("this_device").and_then(Value::as_bool) == Some(true),
                })
                .collect(),
        ),
        Err(error) => Section::Failed(error.reason),
    }
}

fn iso_of_ms(ms: i64) -> Option<String> {
    Some(
        chrono::DateTime::from_timestamp_millis(ms)?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// Pending join requests. This account model has none: a device joins by listing another device's
/// public key in `relayd.json` (docs/remote.md), so there is no request to answer and nothing that
/// expires. Reporting the reason keeps the section honest instead of printing an empty list.
fn read_pending() -> Section<Vec<PendingJoin>> {
    Section::Failed(
        "this account model has no join requests: devices join by listing each other's keys (docs/remote.md)"
            .to_owned(),
    )
}

/// Collects every section. Each read is independent, so a failure in one leaves the others readable.
pub async fn collect_sync_status(ctx: &RemoteContext) -> SyncStatusReport {
    let account = mhc_relayd::RelaydConfig::load(&ctx.state_dir)
        .unwrap_or_default()
        .account;
    let device_id = mhc_relayd::DeviceIdentity::load(&ctx.state_dir)
        .ok()
        .flatten()
        .map(|identity| identity.id())
        .unwrap_or_default();
    let env: BTreeMap<String, String> = maho_core::config::current_env().into_iter().collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let pin_dir = sync_marks_dir(&env, &cwd);
    let device = read_device(&pin_dir, &account);
    let devices = read_devices(ctx).await;
    let pending = read_pending();
    let engine = read_engine_sync_status(&ctx.agent_dir);
    SyncStatusReport {
        account,
        device_id,
        device,
        devices,
        pending,
        engine,
    }
}

const USAGE: &str = "usage: mhc sync <status> [--json]\n\n  status   this device's memory sync: the account, this device's join and switch, the\n           account's devices, pending join requests and whether an engine runs here";

/// Runs `mhc sync <args>`; the process exit code. Only `status` exists.
pub async fn run_sync(ctx: &RemoteContext, args: &[String]) -> i32 {
    let Some(first) = args.first().map(String::as_str) else {
        ctx.err(USAGE);
        return RemoteExit::Usage.code();
    };
    if first == "help" || is_help_flag(first) {
        ctx.out(USAGE);
        return RemoteExit::Ok.code();
    }
    if first != "status" {
        ctx.err(&format!("mhc sync: unknown command {first}"));
        ctx.err(USAGE);
        return RemoteExit::Usage.code();
    }
    let mut json_output = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--json" => json_output = true,
            other if is_help_flag(other) => {
                ctx.out(USAGE);
                return RemoteExit::Ok.code();
            }
            other => {
                ctx.err(&format!("mhc sync status: unknown argument {other}"));
                ctx.err(USAGE);
                return RemoteExit::Usage.code();
            }
        }
    }
    let report = collect_sync_status(ctx).await;
    if json_output {
        ctx.out(&report.to_json().to_string());
    } else {
        for line in report.lines() {
            ctx.out(&line);
        }
    }
    // A report whose sections all failed is nothing a person can act on; one failed section is a
    // fact about that source, which the report already names.
    if report.sections_all_failed() {
        RemoteExit::Failed.code()
    } else {
        RemoteExit::Ok.code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SyncStatusReport {
        SyncStatusReport {
            account: "acct".into(),
            device_id: "dev_svc".into(),
            device: Section::Ok(DeviceStatus {
                joined: true,
                switch_on: true,
            }),
            devices: Section::Ok(vec![
                StatusDevice {
                    id: "dev_svc".into(),
                    name: "laptop".into(),
                    syncing: Some(true),
                    last_seen_at: Some("2026-11-20T01:00:00.000Z".into()),
                    current: true,
                },
                StatusDevice {
                    id: "dev_old".into(),
                    name: "old".into(),
                    syncing: None,
                    last_seen_at: None,
                    current: false,
                },
            ]),
            pending: Section::Ok(Vec::new()),
            engine: EngineSyncRead::NoEngine,
        }
    }

    #[test]
    fn json_carries_every_section_with_the_upstream_field_names() {
        let value = report().to_json();
        assert_eq!(value["account"], "acct");
        assert_eq!(value["deviceId"], "dev_svc");
        assert_eq!(
            value["device"],
            json!({ "ok": true, "joined": true, "switch": "on" })
        );
        assert_eq!(
            value["devices"]["list"],
            json!([
                {
                    "id": "dev_svc",
                    "name": "laptop",
                    "current": true,
                    "syncing": true,
                    "lastSeenAt": "2026-11-20T01:00:00.000Z",
                },
                { "id": "dev_old", "name": "old", "current": false },
            ])
        );
        assert_eq!(value["pending"], json!({ "ok": true, "list": [] }));
        assert_eq!(value["engine"], json!({ "kind": "no_engine" }));
    }

    #[test]
    fn a_not_joined_device_that_is_switched_off_says_so() {
        let mut report = report();
        report.device = Section::Ok(DeviceStatus {
            joined: false,
            switch_on: false,
        });
        let lines = report.lines();
        assert!(
            lines.contains(&"This device: dev_svc, not joined, sync off".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"    old (dev_old): sync unknown".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Pending join requests: none".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Engine: no engine is running on this machine.".to_owned()),
            "{lines:?}"
        );
    }

    #[test]
    fn a_refused_section_reports_its_own_reason_and_leaves_the_others_readable() {
        let mut refused = report();
        refused.devices = Section::Failed("service answered 503".into());
        let lines = refused.lines();
        assert!(
            lines.contains(&"Devices: could not be read (service answered 503).".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"This device: dev_svc, joined, sync on".to_owned()),
            "{lines:?}"
        );
        assert_eq!(
            refused.to_json()["devices"],
            json!({ "ok": false, "reason": "service answered 503" })
        );
        assert!(!refused.sections_all_failed());
        refused.device = Section::Failed("permission denied".into());
        refused.pending = Section::Failed("no join requests".into());
        assert!(refused.sections_all_failed());
        let lines = refused.lines();
        assert!(
            lines.contains(
                &"This device: dev_svc, local sync state could not be read (permission denied)."
                    .to_owned()
            ),
            "{lines:?}"
        );
    }

    #[test]
    fn an_unreadable_engine_names_why_and_a_running_one_never_promises_sync_details() {
        let mut unsafe_only = report();
        unsafe_only.engine = EngineSyncRead::Unreadable("permission denied".into());
        assert!(
            unsafe_only
                .lines()
                .contains(&"Engine: could not be checked (permission denied).".to_owned())
        );
        assert_eq!(
            unsafe_only.to_json()["engine"],
            json!({ "kind": "unreadable", "reason": "permission denied" })
        );
        let mut running = report();
        running.engine = EngineSyncRead::Running;
        let line = running.lines().pop().expect("engine line");
        assert!(line.starts_with("Engine: running."), "{line}");
        assert!(line.contains("not readable from the CLI yet"), "{line}");
        assert_eq!(running.to_json()["engine"], json!({ "kind": "running" }));
    }

    #[test]
    fn the_operator_socket_falls_back_to_the_agent_dirs_rpc_socket() {
        let socket = operator_socket(Path::new("/tmp/agent"));
        if maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env()).is_none() {
            assert_eq!(socket, PathBuf::from("/tmp/agent/rpc/rpc.sock"));
        }
    }

    /// A leftover regular file at the operator socket path is no engine, and a live listener is
    /// `running`; the probe sends nothing, so the listener receives no bytes.
    #[cfg(unix)]
    #[test]
    fn the_operator_socket_decides_no_engine_running_or_unreadable() {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir().expect("tempdir");
        let agent = directory.path().join("agent");
        std::fs::create_dir_all(agent.join("rpc")).expect("rpc dir");
        let socket = agent.join("rpc/rpc.sock");
        assert_eq!(read_engine_sync_status(&agent), EngineSyncRead::NoEngine);
        std::fs::write(&socket, b"").expect("stale file");
        assert_eq!(read_engine_sync_status(&agent), EngineSyncRead::NoEngine);
        std::fs::remove_file(&socket).expect("remove stale file");
        let listener = UnixListener::bind(&socket).expect("listener");
        let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&received);
        let accepted = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the probe connects");
            let mut buffer = [0u8; 64];
            let read = stream.read(&mut buffer).unwrap_or(0);
            counter.fetch_add(read, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(read_engine_sync_status(&agent), EngineSyncRead::Running);
        accepted.join().expect("listener thread");
        assert_eq!(
            received.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the probe sends no request"
        );
        std::fs::remove_file(&socket).expect("remove socket");
        // a socket this user may not reach is "could not be checked", never "no engine"; root
        // bypasses the mode bits, so only a non-root run can make the directory unreachable.
        if current_uid() != 0 {
            let hidden = directory.path().join("hidden");
            std::fs::create_dir(&hidden).expect("hidden dir");
            let listener = UnixListener::bind(hidden.join("engine.sock")).expect("listener");
            drop(listener);
            let mut permissions = std::fs::metadata(&hidden).expect("metadata").permissions();
            permissions.set_mode(0o000);
            std::fs::set_permissions(&hidden, permissions).expect("mode");
            let status = probe_socket(&hidden.join("engine.sock"));
            assert!(
                matches!(status, EngineSyncRead::Unreadable(_)),
                "an unreachable socket is unreadable, not {status:?}"
            );
            let mut permissions = std::fs::metadata(&hidden).expect("metadata").permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&hidden, permissions).expect("restore mode");
        }
    }

    #[cfg(unix)]
    fn current_uid() -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self")
            .map(|metadata| metadata.uid())
            .unwrap_or(1)
    }
}
