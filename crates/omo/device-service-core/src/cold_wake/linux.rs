//! Port of `cold-wake/linux.ts`: systemd user units.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::ManagedUnit;
use super::host::{
    ColdWakeHost, ColdWakeOutcome, CommandOutput, remove_if_present, write_if_changed,
};
use super::session_host_unit::{
    DEVICE_SERVICE_SYSTEMD_UNIT, SESSION_HOST_SYSTEMD_UNIT, SessionHostLaunch,
    systemd_device_service_unit, systemd_session_host_unit,
};

pub fn unit_dir(home: &Path) -> PathBuf {
    home.join(".config").join("systemd").join("user")
}

pub fn unit_name(unit: ManagedUnit) -> &'static str {
    match unit {
        ManagedUnit::SessionHost => SESSION_HOST_SYSTEMD_UNIT,
        ManagedUnit::DeviceService => DEVICE_SERVICE_SYSTEMD_UNIT,
    }
}

fn systemctl(host: &ColdWakeHost, args: &[&str]) -> CommandOutput {
    let mut argv = vec!["systemctl", "--user"];
    argv.extend_from_slice(args);
    host.runner.run(&argv)
}

fn show_fields(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        .collect()
}

// Enabled for the user's manager and started. A changed unit is restarted onto its new text; an
// unchanged one is started again when it is not running (for the session host that only reuses a
// running host).
fn run_unit(host: &ColdWakeHost, name: &str, changed: bool) -> Option<ColdWakeOutcome> {
    let shown = systemctl(
        host,
        &["show", name, "-p", "UnitFileState", "-p", "ActiveState"],
    );
    if shown.exit_code != 0 {
        return Some(ColdWakeOutcome::failed("unit_show", &shown.output));
    }
    let state = show_fields(&shown.output);
    let mut steps = Vec::new();
    if state.get("UnitFileState").map(String::as_str) != Some("enabled") {
        steps.push("enable");
    }
    let active = state.get("ActiveState").map(String::as_str);
    if changed {
        steps.push("restart");
    } else if active != Some("active") && active != Some("activating") {
        steps.push("start");
    }
    for step in steps {
        let result = systemctl(host, &[step, name]);
        if result.exit_code != 0 {
            return Some(ColdWakeOutcome::failed(
                &format!("unit_{step}"),
                &result.output,
            ));
        }
    }
    None
}

pub fn install(
    host: &ColdWakeHost,
    unit: ManagedUnit,
    launch: &SessionHostLaunch,
) -> ColdWakeOutcome {
    let name = unit_name(unit);
    let text = match unit {
        ManagedUnit::SessionHost => systemd_session_host_unit(launch),
        ManagedUnit::DeviceService => systemd_device_service_unit(launch),
    };
    let changed = match write_if_changed(&unit_dir(&host.home).join(name), &text) {
        Ok(changed) => changed,
        Err(error) => return ColdWakeOutcome::failed("unit_write", &error.to_string()),
    };
    if changed {
        let reload = systemctl(host, &["daemon-reload"]);
        if reload.exit_code != 0 {
            return ColdWakeOutcome::failed("daemon_reload", &reload.output);
        }
    }
    run_unit(host, name, changed).unwrap_or(ColdWakeOutcome::Installed {
        unit_changed: changed,
    })
}

/// `disable --now` first; the unit file goes after, so a failed disable is tried again next time.
pub fn uninstall(host: &ColdWakeHost, unit: ManagedUnit) -> ColdWakeOutcome {
    let name = unit_name(unit);
    let path = unit_dir(&host.home).join(name);
    if path.exists() {
        let disabled = systemctl(host, &["disable", "--now", name]);
        if disabled.exit_code != 0 {
            return ColdWakeOutcome::failed("unit_disable", &disabled.output);
        }
    }
    match remove_if_present(&path) {
        Ok(true) => {
            let reload = systemctl(host, &["daemon-reload"]);
            if reload.exit_code != 0 {
                return ColdWakeOutcome::failed("daemon_reload", &reload.output);
            }
            ColdWakeOutcome::Removed
        }
        Ok(false) => ColdWakeOutcome::Removed,
        Err(error) => ColdWakeOutcome::failed("unit_remove", &error.to_string()),
    }
}

/// `systemctl --user show` of a unit: `(UnitFileState, ActiveState)`, `None` when it cannot be read.
pub fn unit_state(host: &ColdWakeHost, unit: ManagedUnit) -> Option<(String, String)> {
    let shown = systemctl(
        host,
        &[
            "show",
            unit_name(unit),
            "-p",
            "UnitFileState",
            "-p",
            "ActiveState",
        ],
    );
    if shown.exit_code != 0 {
        return None;
    }
    let state = show_fields(&shown.output);
    Some((
        state.get("UnitFileState").cloned().unwrap_or_default(),
        state.get("ActiveState").cloned().unwrap_or_default(),
    ))
}
