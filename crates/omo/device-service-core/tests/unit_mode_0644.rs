#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use device_service_core::cold_wake::installed_unit::read_installed_session_host_unit;
use device_service_core::cold_wake::session_host_unit::{
    launchd_session_host_plist, systemd_quote, systemd_session_host_unit,
};
use device_service_core::cold_wake::{
    ColdWakeHost, ColdWakeOutcome, CommandOutput, CommandRunner, ManagedUnit, ServicePlatform,
    SessionHostLaunch, UnitLaunchOptions, install_unit, uninstall_unit, unit_path,
    write_if_changed,
};

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// Runs `body` under umask 077, the setting that used to leave units 0600 (omo#14247).
fn under_umask_077<T>(body: impl FnOnce() -> T) -> T {
    let previous = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let result = body();
    rustix::process::umask(previous);
    result
}

#[derive(Default)]
struct FakeSystemctl {
    calls: Mutex<Vec<String>>,
    active_state: Mutex<String>,
    unit_file_state: Mutex<String>,
}

impl FakeSystemctl {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    fn clear(&self) {
        self.calls.lock().expect("calls").clear();
    }
}

impl CommandRunner for FakeSystemctl {
    fn run(&self, argv: &[&str]) -> CommandOutput {
        let call = argv.join(" ");
        self.calls.lock().expect("calls").push(call.clone());
        let output = if argv.contains(&"show") {
            format!(
                "UnitFileState={}\nActiveState={}\n",
                self.unit_file_state.lock().expect("state"),
                self.active_state.lock().expect("state")
            )
        } else {
            if argv.contains(&"enable") {
                *self.unit_file_state.lock().expect("state") = "enabled".into();
            }
            String::new()
        };
        CommandOutput {
            exit_code: 0,
            output,
        }
    }
}

fn launch() -> SessionHostLaunch {
    SessionHostLaunch::session_host(&UnitLaunchOptions {
        mhc_bin: "/opt/mhc/bin/mhc".into(),
        agent_dir: "/home/me/.omo/agent".into(),
        path: Some("/usr/local/bin:/usr/bin".into()),
    })
    .expect("checked launch")
}

fn host(home: &Path, runner: Arc<FakeSystemctl>) -> ColdWakeHost {
    ColdWakeHost {
        platform: ServicePlatform::Linux,
        home: home.to_path_buf(),
        uid: 1000,
        runner,
        pause: Arc::new(|_| {}),
    }
}

#[test]
fn a_new_unit_is_written_0644_under_umask_077() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("units").join("mhc-session-host.service");
    let written = under_umask_077(|| write_if_changed(&path, "[Unit]\n"));
    assert!(written.expect("write"));
    assert_eq!(mode(&path), 0o644);
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "the temp file is renamed into place");
}

#[test]
fn an_unchanged_unit_left_0600_is_repaired_without_counting_as_a_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("unit.service");
    std::fs::write(&path, "same\n").expect("seed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(!write_if_changed(&path, "same\n").expect("repair"));
    assert_eq!(mode(&path), 0o644);
    assert!(write_if_changed(&path, "changed\n").expect("rewrite"));
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "changed\n");
    assert_eq!(mode(&path), 0o644);
}

#[test]
fn install_enables_and_starts_then_an_unchanged_reinstall_only_starts() {
    let home = tempfile::tempdir().expect("home");
    let runner = Arc::new(FakeSystemctl::default());
    *runner.active_state.lock().expect("state") = "inactive".into();
    let host = host(home.path(), Arc::clone(&runner));

    let first = under_umask_077(|| install_unit(&host, ManagedUnit::SessionHost, &launch()));
    assert_eq!(first, ColdWakeOutcome::Installed { unit_changed: true });
    assert_eq!(
        runner.calls(),
        vec![
            "systemctl --user daemon-reload",
            "systemctl --user show mhc-session-host.service -p UnitFileState -p ActiveState",
            "systemctl --user enable mhc-session-host.service",
            "systemctl --user restart mhc-session-host.service",
        ]
    );
    let unit = unit_path(&host, ManagedUnit::SessionHost);
    assert_eq!(mode(&unit), 0o644);

    runner.clear();
    let second = install_unit(&host, ManagedUnit::SessionHost, &launch());
    assert_eq!(
        second,
        ColdWakeOutcome::Installed {
            unit_changed: false
        }
    );
    assert_eq!(
        runner.calls(),
        vec![
            "systemctl --user show mhc-session-host.service -p UnitFileState -p ActiveState",
            "systemctl --user start mhc-session-host.service",
        ],
        "no reload and no restart for an unchanged unit"
    );
}

#[test]
fn a_unit_an_older_build_left_0600_is_repaired_with_no_reload_or_restart() {
    let home = tempfile::tempdir().expect("home");
    let runner = Arc::new(FakeSystemctl::default());
    *runner.active_state.lock().expect("state") = "active".into();
    *runner.unit_file_state.lock().expect("state") = "enabled".into();
    let host = host(home.path(), Arc::clone(&runner));
    let unit = unit_path(&host, ManagedUnit::SessionHost);
    std::fs::create_dir_all(unit.parent().expect("dir")).expect("dir");
    std::fs::write(&unit, systemd_session_host_unit(&launch())).expect("seed");
    std::fs::set_permissions(&unit, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let outcome = install_unit(&host, ManagedUnit::SessionHost, &launch());
    assert_eq!(
        outcome,
        ColdWakeOutcome::Installed {
            unit_changed: false
        }
    );
    assert_eq!(mode(&unit), 0o644);
    assert_eq!(
        runner.calls(),
        vec!["systemctl --user show mhc-session-host.service -p UnitFileState -p ActiveState"]
    );
}

#[test]
fn uninstall_disables_then_removes_and_is_idempotent() {
    let home = tempfile::tempdir().expect("home");
    let runner = Arc::new(FakeSystemctl::default());
    let host = host(home.path(), Arc::clone(&runner));
    install_unit(&host, ManagedUnit::DeviceService, &launch());
    runner.clear();
    assert_eq!(
        uninstall_unit(&host, ManagedUnit::DeviceService),
        ColdWakeOutcome::Removed
    );
    assert_eq!(
        runner.calls(),
        vec![
            "systemctl --user disable --now mhc-relayd.service",
            "systemctl --user daemon-reload",
        ]
    );
    assert!(!unit_path(&host, ManagedUnit::DeviceService).exists());
    runner.clear();
    assert_eq!(
        uninstall_unit(&host, ManagedUnit::DeviceService),
        ColdWakeOutcome::Removed
    );
    assert!(runner.calls().is_empty());
}

#[test]
fn units_quote_specifiers_and_read_back() {
    let launch = SessionHostLaunch::session_host(&UnitLaunchOptions {
        mhc_bin: "/opt/my %h $HOME \"mhc\"/mhc".into(),
        agent_dir: "/home/me/agent 100%".into(),
        path: None,
    })
    .expect("checked");
    let unit = systemd_session_host_unit(&launch);
    assert!(unit.contains("Type=oneshot"));
    assert!(unit.contains("TimeoutStartSec=300"));
    assert!(unit.contains("KillMode=process"));
    assert!(
        !unit.contains("Restart="),
        "no timer re-runs the ensure (#13668)"
    );
    assert!(unit.contains(&format!(
        "ExecStart={} \"host\" \"ensure\"",
        systemd_quote(launch.program(), true)
    )));
    let read = read_installed_session_host_unit(&unit, false).expect("systemd readback");
    assert_eq!(read.argv, launch.argv());
    assert_eq!(
        read.env.get("MAHO_CODING_AGENT_DIR").map(String::as_str),
        Some("/home/me/agent 100%")
    );

    let plist = launchd_session_host_plist(&launch);
    assert!(plist.contains("<key>AbandonProcessGroup</key>"));
    let read = read_installed_session_host_unit(&plist, true).expect("launchd readback");
    assert_eq!(read.argv, launch.argv());
    assert_eq!(
        read.env.get("MAHO_CODING_AGENT_DIR").map(String::as_str),
        Some("/home/me/agent 100%")
    );
}

#[test]
fn a_launch_refuses_control_characters_and_relative_paths() {
    for (mhc_bin, agent_dir) in [
        ("/opt/mhc\n[Service]", "/agent"),
        ("relative/mhc", "/agent"),
        ("/opt/mhc", "agent"),
        ("", "/agent"),
    ] {
        assert!(
            SessionHostLaunch::session_host(&UnitLaunchOptions {
                mhc_bin: mhc_bin.into(),
                agent_dir: agent_dir.into(),
                path: None,
            })
            .is_err(),
            "{mhc_bin:?} {agent_dir:?}"
        );
    }
}

// omo#14258: a unit naming its launcher script behind the runtime that runs it reads back whole.
#[test]
fn an_installed_unit_reads_back_every_argument() {
    let systemd = "[Service]\nExecStart=\"/h/.bun/bin/bun\" \"/h/.omo/omo-versions/5.1.76/omo-ai/bin/omo.js\" \"daemon\" \"run\" \"--pinned\"\nEnvironment=\"PATH=/opt/tools/bin:/usr/bin\"\n";
    let read = read_installed_session_host_unit(systemd, false).expect("systemd readback");
    assert_eq!(
        read.argv,
        [
            "/h/.bun/bin/bun",
            "/h/.omo/omo-versions/5.1.76/omo-ai/bin/omo.js",
            "daemon",
            "run",
            "--pinned",
        ]
    );
    assert_eq!(
        read.argv.first().map(String::as_str),
        Some("/h/.bun/bin/bun")
    );
    assert_eq!(
        read.env.get("PATH").map(String::as_str),
        Some("/opt/tools/bin:/usr/bin")
    );

    let launchd = "<key>ProgramArguments</key>\n  <array>\n    <string>/h/.bun/bin/bun</string>\n    <string>/h/.omo/omo-versions/5.1.76/omo-ai/bin/omo.js</string>\n    <string>a &amp; b</string>\n  </array>\n";
    let read = read_installed_session_host_unit(launchd, true).expect("launchd readback");
    assert_eq!(
        read.argv,
        [
            "/h/.bun/bin/bun",
            "/h/.omo/omo-versions/5.1.76/omo-ai/bin/omo.js",
            "a & b",
        ]
    );

    assert_eq!(
        read_installed_session_host_unit("[Service]\nExecStart=\n", false),
        None
    );
    assert_eq!(
        read_installed_session_host_unit("<key>ProgramArguments</key><array></array>", true),
        None
    );
}
