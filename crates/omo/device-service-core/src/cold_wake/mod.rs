//! Port of `device-service-core/src/cold-wake/`: the always-on session host unit beside the device
//! service, so this device's host is up at login and a message from another device finds it.
//!
//! mhc divergences: the session host runs `<mhc> host ensure` (omo: `omo daemon run --pinned`), and
//! the device service is mhc's own `mhc-relayd`, installed beside it by the same writer. omo's
//! launch-time service table (`OMO_RELAYD_SERVICES`, a contract with the closed omo-relay-daemon) is
//! not written: mhc-relayd needs no table to reach the host.

pub mod darwin;
pub mod host;
pub mod installed_unit;
pub mod linux;
pub mod session_host_unit;

pub use host::{
    ColdWakeHost, ColdWakeOutcome, CommandOutput, CommandRunner, ServicePlatform,
    SystemCommandRunner, UNIT_MODE, remove_if_present, write_if_changed,
};
pub use installed_unit::{InstalledSessionHostUnit, read_installed_session_host_unit};
pub use session_host_unit::{
    ColdWakeUnitError, DEVICE_SERVICE_LAUNCHD_LABEL, DEVICE_SERVICE_SYSTEMD_UNIT,
    SESSION_HOST_LAUNCHD_LABEL, SESSION_HOST_SYSTEMD_UNIT, SessionHostLaunch, UnitLaunchOptions,
};

/// The installed units this module manages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedUnit {
    /// The session host: `<mhc> host ensure`, once per start.
    SessionHost,
    /// The device service: `<mhc> remote daemon`, kept running.
    DeviceService,
}

/// Installs (or refreshes) `unit` for `launch` on the host's platform.
pub fn install_unit(
    host: &ColdWakeHost,
    unit: ManagedUnit,
    launch: &SessionHostLaunch,
) -> ColdWakeOutcome {
    match host.platform {
        ServicePlatform::Linux => linux::install(host, unit, launch),
        ServicePlatform::Darwin => darwin::install(host, unit, launch),
    }
}

/// Stops and removes `unit`; a unit that is not there is already removed.
pub fn uninstall_unit(host: &ColdWakeHost, unit: ManagedUnit) -> ColdWakeOutcome {
    match host.platform {
        ServicePlatform::Linux => linux::uninstall(host, unit),
        ServicePlatform::Darwin => darwin::uninstall(host, unit),
    }
}

/// The unit's file, wherever the platform keeps it.
pub fn unit_path(host: &ColdWakeHost, unit: ManagedUnit) -> std::path::PathBuf {
    match host.platform {
        ServicePlatform::Linux => linux::unit_dir(&host.home).join(linux::unit_name(unit)),
        ServicePlatform::Darwin => darwin::plist_path(&host.home, darwin::label(unit)),
    }
}
