//! Port of `cold-wake/darwin.ts`: LaunchAgents in the user's GUI domain.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::ManagedUnit;
use super::host::{
    ColdWakeHost, ColdWakeOutcome, CommandOutput, remove_if_present, write_if_changed,
};
use super::session_host_unit::{
    DEVICE_SERVICE_LAUNCHD_LABEL, SESSION_HOST_LAUNCHD_LABEL, SessionHostLaunch,
    launchd_device_service_plist, launchd_session_host_plist,
};

const BOOTSTRAP_ATTEMPTS: u32 = 5;
const BOOTSTRAP_RETRY: Duration = Duration::from_secs(1);

pub fn plist_path(home: &Path, label: &str) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

pub fn label(unit: ManagedUnit) -> &'static str {
    match unit {
        ManagedUnit::SessionHost => SESSION_HOST_LAUNCHD_LABEL,
        ManagedUnit::DeviceService => DEVICE_SERVICE_LAUNCHD_LABEL,
    }
}

fn launchctl(host: &ColdWakeHost, args: &[&str]) -> CommandOutput {
    let mut argv = vec!["launchctl"];
    argv.extend_from_slice(args);
    host.runner.run(&argv)
}

/// `launchctl bootstrap` right after a bootout can fail while launchd still tears the job down: it is
/// retried, as the installer does.
fn bootstrap_job(host: &ColdWakeHost, plist: &Path) -> CommandOutput {
    let domain = format!("gui/{}", host.uid);
    let plist = plist.to_string_lossy().into_owned();
    let mut result = launchctl(host, &["bootstrap", &domain, &plist]);
    let mut attempt = 1;
    while attempt < BOOTSTRAP_ATTEMPTS && result.exit_code != 0 {
        (host.pause)(BOOTSTRAP_RETRY);
        result = launchctl(host, &["bootstrap", &domain, &plist]);
        attempt += 1;
    }
    result
}

pub fn install(
    host: &ColdWakeHost,
    unit: ManagedUnit,
    launch: &SessionHostLaunch,
) -> ColdWakeOutcome {
    let label = label(unit);
    let plist = plist_path(&host.home, label);
    let job = format!("gui/{}/{label}", host.uid);
    let text = match unit {
        ManagedUnit::SessionHost => launchd_session_host_plist(launch),
        ManagedUnit::DeviceService => launchd_device_service_plist(launch),
    };
    let changed = match write_if_changed(&plist, &text) {
        Ok(changed) => changed,
        Err(error) => return ColdWakeOutcome::failed("unit_write", &error.to_string()),
    };
    // A bootout fails when the job is not loaded, which is the state it is meant to reach.
    if changed {
        launchctl(host, &["bootout", &job]);
    }
    if changed || launchctl(host, &["print", &job]).exit_code != 0 {
        // A job someone disabled refuses a bootstrap; RunAtLoad starts it once it is loaded.
        launchctl(host, &["enable", &job]);
        let loaded = bootstrap_job(host, &plist);
        if loaded.exit_code != 0 {
            return ColdWakeOutcome::failed("unit_load", &loaded.output);
        }
    } else {
        // Loaded and unchanged: start it again (the session host's ensure reuses a running host).
        let kicked = launchctl(host, &["kickstart", &job]);
        if kicked.exit_code != 0 {
            return ColdWakeOutcome::failed("unit_start", &kicked.output);
        }
    }
    ColdWakeOutcome::Installed {
        unit_changed: changed,
    }
}

pub fn uninstall(host: &ColdWakeHost, unit: ManagedUnit) -> ColdWakeOutcome {
    let label = label(unit);
    let plist = plist_path(&host.home, label);
    if plist.exists() {
        launchctl(host, &["bootout", &format!("gui/{}/{label}", host.uid)]);
    }
    match remove_if_present(&plist) {
        Ok(_) => ColdWakeOutcome::Removed,
        Err(error) => ColdWakeOutcome::failed("unit_remove", &error.to_string()),
    }
}
