//! The one device resolver.
//!
//! Upstream counterpart: `device.ts`.

use std::sync::Arc;

use crate::listing::DeviceListing;
use crate::name::normalize_name;
use crate::parse::{DEVICE_PREFIX_MIN, HERE};
use crate::refusals::{
    AddressRefusal, DeviceRow, ShareBy, device_offline, devices_share, no_device,
};

/// A resolved device: its id and name, whether it is online and this device, and how this round listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub here: bool,
    pub listing: DeviceListing,
}

/// How a round listed one device.
pub type ListingOf = Arc<dyn Fn(&str) -> DeviceListing + Send + Sync>;

/// What device resolution reads: this device's id, the roster, and each device's listing when a round was read.
#[derive(Clone, Default)]
pub struct DeviceRoster {
    pub here: Option<String>,
    pub devices: Vec<DeviceRow>,
    pub listing_of: Option<ListingOf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceResolution {
    Ok(DeviceRef),
    Refusal(AddressRefusal),
}

/// `online`: a device found offline by any form is refused. `exact_id`: the caller holds a listing row's device id,
/// so only that exact id resolves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveDeviceOptions {
    pub online: bool,
    pub exact_id: bool,
}

fn found(
    roster: &DeviceRoster,
    text: &str,
    options: ResolveDeviceOptions,
    row: &DeviceRow,
) -> DeviceResolution {
    if options.online && !row.online {
        return DeviceResolution::Refusal(device_offline(text, row, &[row]));
    }
    let listing = match &roster.listing_of {
        Some(listing_of) => listing_of(row.id.as_str()),
        None if row.online => DeviceListing::Listed,
        None => DeviceListing::Offline,
    };
    DeviceResolution::Ok(DeviceRef {
        id: row.id.clone(),
        name: row.name.clone(),
        online: row.online,
        here: roster.here.as_deref() == Some(row.id.as_str()),
        listing,
    })
}

/// The one device resolver (design section 3): exact id > `here` > name (of the devices sharing it the online ones
/// win, so an offline twin a re-registered machine left behind never shadows the live device; several winners are
/// ambiguous_target) > a unique id prefix of 6+ characters. Whether an offline device resolves depends only on
/// `online`, never on the form. A caller that holds a listing row's device id passes `exact_id`: the id resolves or the
/// refusal is the unknown-device one, so a row whose device left the roster never lands on a device that shares its
/// prefix or carries its text as a name.
pub fn resolve_device(
    roster: &DeviceRoster,
    text: &str,
    options: ResolveDeviceOptions,
) -> DeviceResolution {
    if let Some(row) = roster.devices.iter().find(|row| row.id == text) {
        return found(roster, text, options, row);
    }
    if options.exact_id {
        return DeviceResolution::Refusal(no_device(text, &roster.devices));
    }
    if text == HERE {
        let here = roster.here.as_deref();
        if let Some(row) = roster
            .devices
            .iter()
            .find(|row| Some(row.id.as_str()) == here)
        {
            return found(roster, text, options, row);
        }
        return match here {
            None => DeviceResolution::Refusal(no_device(text, &roster.devices)),
            Some(id) => DeviceResolution::Ok(DeviceRef {
                id: id.to_owned(),
                name: HERE.to_owned(),
                online: true,
                here: true,
                listing: DeviceListing::Listed,
            }),
        };
    }
    let wanted = normalize_name(text);
    let named: Vec<&DeviceRow> = roster
        .devices
        .iter()
        .filter(|row| normalize_name(&row.name) == wanted)
        .collect();
    let online: Vec<&DeviceRow> = named.iter().copied().filter(|row| row.online).collect();
    if options.online
        && online.is_empty()
        && let Some(offline) = named.first()
    {
        return DeviceResolution::Refusal(device_offline(text, offline, &named));
    }
    let winners = if online.is_empty() { &named } else { &online };
    if winners.len() == 1 {
        return found(roster, text, options, winners[0]);
    }
    if winners.len() > 1 {
        return DeviceResolution::Refusal(devices_share(ShareBy::DevicesShareName, text, winners));
    }
    let prefix: Vec<&DeviceRow> = if wanted.chars().count() >= DEVICE_PREFIX_MIN {
        roster
            .devices
            .iter()
            .filter(|row| row.id.to_lowercase().starts_with(wanted.as_str()))
            .collect()
    } else {
        Vec::new()
    };
    if prefix.len() == 1 {
        return found(roster, text, options, prefix[0]);
    }
    if prefix.len() > 1 {
        return DeviceResolution::Refusal(devices_share(
            ShareBy::DevicesSharePrefix,
            text,
            &prefix,
        ));
    }
    DeviceResolution::Refusal(no_device(text, &roster.devices))
}
