//! Port of omo `packages/omo-native/host-scope.ts` (omo#14341): move a compiled host supervisor into
//! its own systemd user scope before it spawns its host.
//!
//! On Linux a host supervisor and the host and tool children it spawns stay in the systemd cgroup of
//! whatever started them (an ensure loop service, the oneshot session host unit, a terminal holder, an
//! ssh scope). That unit's `OOMPolicy` then decides the host's fate: one OOM-killed tool child stops
//! the unit and SIGTERMs every session. The supervisor therefore asks its user manager to move its own
//! pid into a fresh transient scope before it spawns its host, so the host and every tool child inherit
//! the scope. The supervisor keeps running in place; any failure (no manager, a refused unit, an old
//! systemd, a slow bus) only leaves it where it was. `busctl` reaches the manager through the user
//! message bus, so a host with a user manager but no user bus keeps the direct placement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The supervisor waits at most this long for its scope; the engine's bootstrap phase budget is 30 s.
pub const SCOPE_BUDGET: Duration = Duration::from_secs(2);
const SCOPE_POLL: Duration = Duration::from_millis(10);
/// The scope's memory cap, as a share of `MemTotal`, so one runaway child cannot OOM its siblings.
const MEMORY_MAX_SHARE_NUMERATOR: u64 = 9;
const MEMORY_MAX_SHARE_DENOMINATOR: u64 = 10;
/// Bytes of the random nonce in the unit name; the name is built from a pid and this, never from input.
const NONCE_LEN: usize = 6;

/// What the decision reads. Only `platform` and `supervisor_launch` are known before any filesystem or
/// process probe, so a launch that is not a Linux host supervisor never runs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostScopeInputs {
    pub platform: &'static str,
    pub supervisor_launch: bool,
    pub has_user_bus: bool,
    pub owns_user_manager: bool,
    pub busctl: Option<PathBuf>,
    pub mem_total_bytes: Option<u64>,
}

impl HostScopeInputs {
    /// The two facts every launch carries, before anything is probed.
    pub fn for_launch(platform: &'static str, supervisor_launch: bool) -> Self {
        Self {
            platform,
            supervisor_launch,
            has_user_bus: false,
            owns_user_manager: false,
            busctl: None,
            mem_total_bytes: None,
        }
    }
}

/// The `busctl` call that starts the scope, and the unit it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostScopePlan {
    pub busctl: PathBuf,
    pub argv: Vec<String>,
    pub unit: String,
    pub pid: u32,
}

fn host_scope_unit_name(pid: u32, nonce: &str) -> String {
    format!("omo-host-{pid}-{nonce}.scope")
}

/// `MemTotal` in bytes times 90%, floored; `None` when the file carries no `MemTotal` line.
pub fn memory_max_bytes(meminfo: &str) -> Option<u64> {
    let kib = meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let value = rest.trim().strip_suffix("kB")?.trim();
        value.parse::<u64>().ok()
    })?;
    kib.checked_mul(1024)?
        .checked_mul(MEMORY_MAX_SHARE_NUMERATOR)
        .map(|bytes| bytes / MEMORY_MAX_SHARE_DENOMINATOR)
}

/// `busctl`'s `StartTransientUnit` equivalent of `systemd-run --scope --collect`, for an already
/// running pid.
pub fn start_transient_scope_args(unit: &str, pid: u32, memory_max: u64) -> Vec<String> {
    let mut argv: Vec<String> = [
        "--user",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
        unit,
        "fail",
        "4",
        "PIDs",
        "au",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    argv.push(pid.to_string());
    for property in ["OOMPolicy", "s", "continue", "MemoryMax", "t"] {
        argv.push(property.to_owned());
    }
    argv.push(memory_max.to_string());
    for property in ["CollectMode", "s", "inactive-or-failed", "0"] {
        argv.push(property.to_owned());
    }
    argv
}

/// Cheap checks first: the launch must be a Linux host supervisor, and only then do the environment
/// probes matter.
pub fn scope_eligible(inputs: &HostScopeInputs) -> bool {
    inputs.platform == "linux" && inputs.supervisor_launch
}

/// The plan for a launch, or `None` to leave it where it is. Precedence is the upstream one: bus, then
/// manager, then `busctl`, then `MemTotal`.
pub fn plan_host_scope(inputs: &HostScopeInputs, pid: u32, nonce: &str) -> Option<HostScopePlan> {
    if !scope_eligible(inputs) || !inputs.has_user_bus || !inputs.owns_user_manager {
        return None;
    }
    let busctl = inputs.busctl.clone()?;
    let memory_max = inputs.mem_total_bytes?;
    let unit = host_scope_unit_name(pid, nonce);
    Some(HostScopePlan {
        busctl,
        argv: start_transient_scope_args(&unit, pid, memory_max),
        unit,
        pid,
    })
}

fn split_path_entries(value: Option<&String>) -> Vec<PathBuf> {
    value
        .into_iter()
        .flat_map(|value| std::env::split_paths(value))
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect()
}

/// The first executable `busctl` on `PATH`.
fn busctl_on_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    split_path_entries(env.get("PATH"))
        .into_iter()
        .map(|directory| directory.join("busctl"))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn socket_metadata(path: &Path) -> Option<std::fs::Metadata> {
    use std::os::unix::fs::FileTypeExt;
    let metadata = std::fs::metadata(path).ok()?;
    metadata.file_type().is_socket().then_some(metadata)
}

/// The unix socket paths a `DBUS_SESSION_BUS_ADDRESS` names (`unix:`, possibly with other keys).
pub fn session_bus_paths(address: &str) -> Vec<PathBuf> {
    address
        .split(';')
        .filter_map(|entry| {
            let rest = entry.strip_prefix("unix:")?;
            rest.split(',')
                .find_map(|pair| pair.strip_prefix("path="))
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        })
        .collect()
}

/// A cheap pre-check for the bus `busctl --user` connects to: a live socket among
/// `DBUS_SESSION_BUS_ADDRESS`' unix paths, or `$XDG_RUNTIME_DIR/bus`. `busctl` itself uses only the
/// address when it is set, so this can pass where `busctl` then fails fast (a stale or abstract
/// address); that only costs one failed, bounded call, and the host stays where it was.
#[cfg(unix)]
fn has_user_bus(env: &HashMap<String, String>) -> bool {
    let addressed = env
        .get("DBUS_SESSION_BUS_ADDRESS")
        .map(String::as_str)
        .map(session_bus_paths)
        .unwrap_or_default();
    if addressed.iter().any(|path| socket_metadata(path).is_some()) {
        return true;
    }
    env.get("XDG_RUNTIME_DIR")
        .filter(|directory| !directory.is_empty())
        .map(|directory| Path::new(directory).join("bus"))
        .is_some_and(|bus| socket_metadata(&bus).is_some())
}

/// A user manager this user owns answers on `$XDG_RUNTIME_DIR/systemd/private`; containers and sudo
/// shells have none.
#[cfg(unix)]
fn owns_user_manager(env: &HashMap<String, String>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(directory) = env.get("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) else {
        return false;
    };
    let private = Path::new(directory).join("systemd").join("private");
    let Some(metadata) = socket_metadata(&private) else {
        return false;
    };
    metadata.uid() == rustix::process::getuid().as_raw()
}

/// The probes a real launch runs, in the order the decision reads them.
pub fn live_host_scope_inputs(
    inputs: HostScopeInputs,
    env: &HashMap<String, String>,
    meminfo: Option<&str>,
) -> HostScopeInputs {
    if !scope_eligible(&inputs) {
        return inputs;
    }
    #[cfg(unix)]
    let bus = has_user_bus(env);
    #[cfg(not(unix))]
    let bus = false;
    if !bus {
        return inputs;
    }
    #[cfg(unix)]
    let manager = owns_user_manager(env);
    #[cfg(not(unix))]
    let manager = false;
    if !manager {
        return HostScopeInputs {
            has_user_bus: true,
            ..inputs
        };
    }
    let Some(busctl) = busctl_on_path(env) else {
        return HostScopeInputs {
            has_user_bus: true,
            owns_user_manager: true,
            ..inputs
        };
    };
    let mem_total_bytes = meminfo.and_then(memory_max_bytes);
    HostScopeInputs {
        has_user_bus: true,
        owns_user_manager: true,
        busctl: Some(busctl),
        mem_total_bytes,
        ..inputs
    }
}

/// A fresh nonce for the unit name, so two supervisors on one pid number never share a scope.
fn nonce() -> String {
    let mut bytes = [0u8; NONCE_LEN];
    // A weak nonce only costs a retried unit name; it never decides placement.
    let filled = getrandom::fill(&mut bytes).is_ok();
    bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            let value = if filled { *byte } else { index as u8 };
            char::from_digit(u32::from(value) % 16, 16).unwrap_or('0')
        })
        .collect()
}

/// Runs `busctl` under the plan's budget. Never panics; the error names how the call ended.
async fn run_busctl(plan: &HostScopePlan, deadline: tokio::time::Instant) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let seconds = remaining.as_secs().max(1);
    let mut command = tokio::process::Command::new(&plan.busctl);
    command
        .arg(format!("--timeout={seconds}"))
        .args(&plan.argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{}: {error}", plan.busctl.display()))?;
    let stderr = child.stderr.take();
    let reader = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut collected = Vec::new();
        let mut buffer = [0u8; 4096];
        if let Some(mut stderr) = stderr {
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        collected.extend_from_slice(&buffer[..read]);
                        if collected.len() > 8192 {
                            break;
                        }
                    }
                }
            }
        }
        String::from_utf8_lossy(&collected).trim().to_owned()
    });
    let waited = tokio::time::timeout(remaining, child.wait()).await;
    let mut killed = false;
    let status = match waited {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => return Err(format!("busctl {error}")),
        Err(_) => {
            killed = true;
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };
    let text = reader.await.unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => {
            let code = status
                .code()
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string());
            Err(format!("busctl exit {code}: {text}"))
        }
        None if killed => Err(format!("busctl SIGKILL: {text}")),
        None => Err(format!("busctl: {text}")),
    }
}

/// Whether the kernel reports `pid` inside `unit`, retried until the budget runs out.
#[cfg(unix)]
async fn landed_in(plan: &HostScopePlan, deadline: tokio::time::Instant) -> bool {
    let marker = format!("/{}", plan.unit);
    loop {
        if std::fs::read_to_string(format!("/proc/{}/cgroup", plan.pid))
            .is_ok_and(|cgroups| cgroups.contains(&marker))
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(SCOPE_POLL).await;
    }
}

#[cfg(not(unix))]
async fn landed_in(_plan: &HostScopePlan, _deadline: tokio::time::Instant) -> bool {
    false
}

/// Starts the plan's scope and waits, within the budget, for the pid to land in it. Never panics.
pub async fn move_into_host_scope(plan: &HostScopePlan) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + SCOPE_BUDGET;
    run_busctl(plan, deadline).await?;
    if landed_in(plan, deadline).await {
        Ok(())
    } else {
        Err(format!(
            "pid {} did not reach {} in time",
            plan.pid, plan.unit
        ))
    }
}

/// Awaited by the compiled entry right before a supervisor starts, so the host it spawns is born in
/// the scope. The launch continues the same way whatever happens; `OMO_DEBUG` reports why a scope was
/// not entered.
pub async fn enter_own_host_scope(args: &[String], env: &HashMap<String, String>) {
    let launch = HostScopeInputs::for_launch(
        std::env::consts::OS,
        args.iter()
            .any(|arg| arg == crate::host_lifecycle::INTERNAL_SUPERVISOR_FLAG),
    );
    if !scope_eligible(&launch) {
        return;
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok();
    let inputs = live_host_scope_inputs(launch, env, meminfo.as_deref());
    let Some(plan) = plan_host_scope(&inputs, std::process::id(), &nonce()) else {
        return;
    };
    let outcome = move_into_host_scope(&plan).await;
    if let Err(failure) = outcome
        && env.contains_key("OMO_DEBUG")
    {
        maho_core::output_guard::maho_write_stderr(&format!(
            "omo: host scope not entered: {failure}\n"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probes() -> HostScopeInputs {
        HostScopeInputs {
            platform: "linux",
            supervisor_launch: true,
            has_user_bus: true,
            owns_user_manager: true,
            busctl: Some(PathBuf::from("/usr/bin/busctl")),
            mem_total_bytes: Some(1000),
        }
    }

    #[test]
    fn plans_a_start_transient_unit_call_that_moves_the_supervisor_pid_into_its_own_scope() {
        let plan = plan_host_scope(&probes(), 4242, "abcdef012345").expect("plan");
        assert_eq!(plan.busctl, PathBuf::from("/usr/bin/busctl"));
        assert_eq!(plan.unit, "omo-host-4242-abcdef012345.scope");
        assert_eq!(plan.pid, 4242);
        assert_eq!(
            plan.argv,
            [
                "--user",
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                "omo-host-4242-abcdef012345.scope",
                "fail",
                "4",
                "PIDs",
                "au",
                "1",
                "4242",
                "OOMPolicy",
                "s",
                "continue",
                "MemoryMax",
                "t",
                "1000",
                "CollectMode",
                "s",
                "inactive-or-failed",
                "0",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn keeps_the_launch_where_it_is_for_every_missing_precondition() {
        let cases: [(&str, HostScopeInputs); 6] = [
            (
                "macOS",
                HostScopeInputs {
                    platform: "macos",
                    ..probes()
                },
            ),
            (
                "Windows",
                HostScopeInputs {
                    platform: "windows",
                    ..probes()
                },
            ),
            (
                "a launch that is not a host supervisor",
                HostScopeInputs {
                    supervisor_launch: false,
                    ..probes()
                },
            ),
            (
                "no user bus",
                HostScopeInputs {
                    has_user_bus: false,
                    ..probes()
                },
            ),
            (
                "no user manager of its own",
                HostScopeInputs {
                    owns_user_manager: false,
                    ..probes()
                },
            ),
            (
                "no busctl",
                HostScopeInputs {
                    busctl: None,
                    ..probes()
                },
            ),
        ];
        for (label, inputs) in cases {
            assert_eq!(
                plan_host_scope(&inputs, 4242, "abcdef012345"),
                None,
                "{label}"
            );
        }
        let unreadable = HostScopeInputs {
            mem_total_bytes: None,
            ..probes()
        };
        assert_eq!(plan_host_scope(&unreadable, 4242, "abcdef012345"), None);
    }

    #[test]
    fn a_launch_that_is_not_a_linux_supervisor_is_ineligible_before_any_probe() {
        assert!(!scope_eligible(&HostScopeInputs::for_launch("macos", true)));
        assert!(!scope_eligible(&HostScopeInputs::for_launch(
            "linux", false
        )));
        assert!(scope_eligible(&HostScopeInputs::for_launch("linux", true)));
        // Nothing probed: every environment-derived field keeps its empty default.
        let untouched = live_host_scope_inputs(
            HostScopeInputs::for_launch("macos", true),
            &HashMap::new(),
            Some("MemTotal:       94535368 kB\n"),
        );
        assert_eq!(untouched, HostScopeInputs::for_launch("macos", true));
    }

    #[test]
    fn caps_the_scope_at_90_percent_of_mem_total_in_bytes() {
        assert_eq!(
            memory_max_bytes("MemTotal:       94535368 kB\nMemFree: 1 kB\n"),
            Some(94_535_368 * 1024 * 9 / 10)
        );
        assert_eq!(memory_max_bytes("MemFree: 1 kB\n"), None);
        assert_eq!(memory_max_bytes("MemTotal: not-a-number kB\n"), None);
    }

    #[test]
    fn reads_the_user_bus_paths_out_of_a_session_address() {
        assert_eq!(
            session_bus_paths("unix:path=/run/user/1000/bus"),
            vec![PathBuf::from("/run/user/1000/bus")]
        );
        assert_eq!(
            session_bus_paths("unix:guid=abc,path=/run/user/1000/bus;unix:path=/tmp/bus"),
            vec![
                PathBuf::from("/run/user/1000/bus"),
                PathBuf::from("/tmp/bus")
            ]
        );
        assert!(session_bus_paths("tcp:host=localhost,port=1").is_empty());
        assert!(session_bus_paths("").is_empty());
    }

    /// A hung `busctl` is killed at the budget, reported, and gone: its recorded pid no longer answers
    /// `kill -0`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hanging_busctl_is_killed_at_the_budget_and_reported() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("tempdir");
        let record = directory.path().join("pid");
        let busctl = directory.path().join("busctl");
        std::fs::write(
            &busctl,
            format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", record.display()),
        )
        .expect("stub");
        let mut permissions = std::fs::metadata(&busctl).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&busctl, permissions).expect("mode");
        let plan = HostScopePlan {
            busctl,
            argv: Vec::new(),
            unit: "omo-host-1-x.scope".to_owned(),
            pid: 1,
        };
        let started = std::time::Instant::now();
        let error = move_into_host_scope(&plan)
            .await
            .expect_err("a hanging stub must not report a scope");
        assert!(error.contains("SIGKILL"), "{error}");
        assert!(
            started.elapsed() < SCOPE_BUDGET + Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let stub_pid: i32 = std::fs::read_to_string(&record)
            .expect("the stub records its pid")
            .trim()
            .parse()
            .expect("numeric pid");
        assert!(stub_pid > 1, "{stub_pid}");
        let alive = std::process::Command::new("kill")
            .args(["-0", &stub_pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "the killed busctl stub {stub_pid} is gone");
    }

    /// A `busctl` that succeeds without moving the pid is a failure, and one that fails outright is
    /// reported instead of swallowed as a scope.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_scope_the_pid_never_reaches_and_a_failing_busctl_are_failures() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("tempdir");
        let write_stub = |body: &str| {
            let path = directory.path().join("busctl");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stub");
            let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).expect("mode");
            path
        };
        let plan_for = |busctl: PathBuf| HostScopePlan {
            busctl,
            argv: Vec::new(),
            unit: "omo-host-1-x.scope".to_owned(),
            pid: 1,
        };
        let error = move_into_host_scope(&plan_for(write_stub("exit 1")))
            .await
            .expect_err("a failing stub must not report a scope");
        assert!(error.contains("exit 1"), "{error}");
        let error = move_into_host_scope(&plan_for(write_stub("exit 0")))
            .await
            .expect_err("a pid outside the unit never lands in it");
        assert!(error.contains("did not reach"), "{error}");
    }
}
