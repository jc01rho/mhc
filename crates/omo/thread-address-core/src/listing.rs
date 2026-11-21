//! How a listing round saw a device.
//!
//! Upstream counterpart: `listing.ts`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::refusals::DeviceRow;

/// How a listing round saw a device: answered in full, did not answer in full, or offline in the roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceListing {
    Listed,
    Incomplete,
    Offline,
}

impl DeviceListing {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Incomplete => "incomplete",
            Self::Offline => "offline",
        }
    }
}

/// One listing round: the devices it names incomplete, and each device's own round state when the service reports one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingRound {
    pub incomplete: Vec<String>,
    /// Per-device round states; every state other than `answered` counts as incomplete.
    pub states: Option<BTreeMap<String, String>>,
}

/// The one listing state for a device, the same for the list and the resolver (design section 6).
pub fn device_listing(device: &str, round: &ListingRound, roster: &[DeviceRow]) -> DeviceListing {
    if roster
        .iter()
        .any(|row| row.id.as_str() == device && !row.online)
    {
        return DeviceListing::Offline;
    }
    let state = round.states.as_ref().and_then(|states| states.get(device));
    if round.incomplete.iter().any(|id| id.as_str() == device)
        || state.is_some_and(|state| state.as_str() != "answered")
    {
        DeviceListing::Incomplete
    } else {
        DeviceListing::Listed
    }
}
