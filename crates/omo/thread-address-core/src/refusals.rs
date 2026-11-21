//! Every typed refusal of an address resolution, as plain data.
//!
//! Upstream counterpart: `refusals.ts`. A refusal carries its code, a `reason` naming the condition and the `details`
//! a caller reads; the caller words the message, because it knows its own tool names.

use std::collections::BTreeMap;

use serde::Serialize;

/// The refusal codes an address resolution answers (design section 5); they are thread error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    InvalidArguments,
    CallerContextMissing,
    NotFound,
    AmbiguousTarget,
    DeviceOffline,
    HostUnavailable,
    ScopeDenied,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::CallerContextMissing => "caller_context_missing",
            Self::NotFound => "not_found",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::DeviceOffline => "device_offline",
            Self::HostUnavailable => "host_unavailable",
            Self::ScopeDenied => "scope_denied",
        }
    }
}

/// A device as a refusal lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceCandidate {
    pub device: String,
    pub name: String,
    pub online: bool,
}

/// Why a device that does not list a thread could not be asked whether it holds it (#14280): this device has no
/// account connection, its service carries no wake (so it makes no probe), or it makes no single probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NotAskedReason {
    #[serde(rename = "remote_unavailable")]
    Remote,
    #[serde(rename = "wake_unavailable")]
    Wake,
    #[serde(rename = "lookup_unavailable")]
    Lookup,
}

impl NotAskedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote_unavailable",
            Self::Wake => "wake_unavailable",
            Self::Lookup => "lookup_unavailable",
        }
    }
}

/// Why the devices a refusal names did not answer: missing from this listing round, or silent to a direct question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SilentReason {
    ListingIncomplete,
    DeviceUnanswered,
}

impl SilentReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListingIncomplete => "listing_incomplete",
            Self::DeviceUnanswered => "device_unanswered",
        }
    }
}

/// What several devices share when a device part names more than one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareBy {
    DevicesShareName,
    DevicesSharePrefix,
}

impl ShareBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DevicesShareName => "devices_share_name",
            Self::DevicesSharePrefix => "devices_share_prefix",
        }
    }
}

/// A device row as the roster names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<i64>,
}

/// A device a silence refusal names, with the reason it gave when it gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilentDevice {
    pub id: String,
    pub name: String,
    pub reason: Option<String>,
}

/// The `details` a refusal carries; each shape is the one upstream's union names for its reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RefusalDetails {
    /// `kind: "device"`: the device part matched no device, or several.
    Device {
        kind: &'static str,
        device: String,
        candidates: Vec<DeviceCandidate>,
    },
    Offline {
        device: String,
        device_name: String,
        last_seen: Option<i64>,
        candidates: Vec<DeviceCandidate>,
    },
    Silent {
        reason: SilentReason,
        devices: Vec<String>,
        device_names: Vec<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        device_reasons: BTreeMap<String, String>,
    },
    NotAsked {
        reason: NotAskedReason,
        thread: String,
        device: String,
        device_name: String,
    },
    /// `kind: "thread"`: the device answered and lists no such thread.
    Thread {
        kind: &'static str,
        thread: String,
        device: String,
        device_name: String,
    },
}

/// One typed refusal: its code, the `reason` naming the condition, the text the caller passed when it names a device,
/// and the details a caller reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddressRefusal {
    pub code: RefusalCode,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typed: Option<String>,
    pub details: RefusalDetails,
}

const CANDIDATE_LIMIT: usize = 10;

fn device_candidate(row: &DeviceRow) -> DeviceCandidate {
    DeviceCandidate {
        device: row.id.clone(),
        name: row.name.clone(),
        online: row.online,
    }
}

/// A device as every message names it: `name (id)`, or the id alone when its name is empty or the id itself.
pub fn device_label(id: &str, name: &str) -> String {
    if name.is_empty() || name == id {
        id.to_owned()
    } else {
        format!("{name} ({id})")
    }
}

pub fn no_device(typed: &str, devices: &[DeviceRow]) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::NotFound,
        reason: "no_device",
        typed: Some(typed.to_owned()),
        details: RefusalDetails::Device {
            kind: "device",
            device: typed.to_owned(),
            candidates: devices
                .iter()
                .take(CANDIDATE_LIMIT)
                .map(device_candidate)
                .collect(),
        },
    }
}

pub fn devices_share(by: ShareBy, typed: &str, matches: &[&DeviceRow]) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::AmbiguousTarget,
        reason: by.as_str(),
        typed: Some(typed.to_owned()),
        details: RefusalDetails::Device {
            kind: "device",
            device: typed.to_owned(),
            candidates: matches
                .iter()
                .take(CANDIDATE_LIMIT)
                .map(|row| device_candidate(row))
                .collect(),
        },
    }
}

pub fn device_offline(typed: &str, device: &DeviceRow, matched: &[&DeviceRow]) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::DeviceOffline,
        reason: "device_offline",
        typed: Some(typed.to_owned()),
        details: RefusalDetails::Offline {
            device: device.id.clone(),
            device_name: device.name.clone(),
            last_seen: device.last_seen_ms,
            candidates: matched
                .iter()
                .take(CANDIDATE_LIMIT)
                .map(|row| device_candidate(row))
                .collect(),
        },
    }
}

pub fn devices_silent(reason: SilentReason, devices: &[SilentDevice]) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::HostUnavailable,
        reason: reason.as_str(),
        typed: None,
        details: RefusalDetails::Silent {
            reason,
            devices: devices.iter().map(|device| device.id.clone()).collect(),
            device_names: devices.iter().map(|device| device.name.clone()).collect(),
            device_reasons: devices
                .iter()
                .filter_map(|device| {
                    device
                        .reason
                        .as_ref()
                        .map(|reason| (device.id.clone(), reason.clone()))
                })
                .collect(),
        },
    }
}

pub fn device_not_asked(
    reason: NotAskedReason,
    thread: &str,
    device_id: &str,
    device_name: &str,
) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::HostUnavailable,
        reason: reason.as_str(),
        typed: None,
        details: RefusalDetails::NotAsked {
            reason,
            thread: thread.to_owned(),
            device: device_id.to_owned(),
            device_name: device_name.to_owned(),
        },
    }
}

pub fn no_thread_on_device(thread: &str, device_id: &str, device_name: &str) -> AddressRefusal {
    AddressRefusal {
        code: RefusalCode::NotFound,
        reason: "no_thread_on_device",
        typed: None,
        details: RefusalDetails::Thread {
            kind: "thread",
            thread: thread.to_owned(),
            device: device_id.to_owned(),
            device_name: device_name.to_owned(),
        },
    }
}
