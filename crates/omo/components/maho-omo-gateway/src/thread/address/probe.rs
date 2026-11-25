//! The single-device probe (omo `thread/address/probe.ts`, #14305, #14280, #14328): the one query a thread id nothing
//! lists gets, to every online device (`probe_across`) or to the one device a reply names (`pinned`).

use maho_thread_address_core::{
    NotAskedReason, SessionRef, SilentDevice, SilentReason, device_not_asked, devices_silent,
    no_thread_on_device, same_session,
};
use serde_json::{Map, Value, json};

use crate::thread::remote::address_book::{device_listing, relay_address_entry};
use crate::thread::remote::ports::{
    RemoteListing, RemoteLookupOutcome, RemoteSessionRow, RemoteThreadSurface, ended_row,
    probe_may_build_ended_row,
};

use super::account::{StatusRound, account_devices};
use super::device::AccountDevice;
use super::refusals::{ThreadRefusal, refusal, tool_refusal};
use super::types::{Found, Pin, Resolution};

fn name_of(devices: Option<&[AccountDevice]>, id: &str) -> String {
    devices
        .and_then(|devices| devices.iter().find(|device| device.device == id))
        .map_or_else(|| id.to_owned(), |device| device.label.clone())
}

fn probed(row: &RemoteSessionRow, resolution: Resolution) -> Found {
    Found {
        entry: relay_address_entry(row, &[]),
        resolution,
        all_scope: Some(true),
    }
}

/// omo#13037 A4.3.3: a thread id this device and the account listing both miss is looked up with one single-probe query
/// to each ONLINE device other than this one, in parallel. One holder: its ended row. Several: `ambiguous_target`. None
/// and a device stayed silent: `device_unanswered`. None, every device answered: `not_found`. `None` when this device's
/// service makes no single probe: the caller answers as before.
pub async fn probe_across(
    remote: &RemoteThreadSurface,
    thread: &str,
    roster: Option<&[AccountDevice]>,
    here: Option<&str>,
) -> Option<Result<Found, ThreadRefusal>> {
    if !remote.sessions.can_lookup().await {
        return None;
    }
    if !probe_may_build_ended_row(&remote.sessions).await {
        return None;
    }
    let devices: Vec<AccountDevice> = match roster {
        Some(roster) => roster.to_vec(),
        None => account_devices(&remote.client).await.ok()?.devices,
    };
    let lookups: Vec<_> = devices
        .iter()
        .filter(|device| device.online && Some(device.device.as_str()) != here)
        .map(|device| {
            let id = device.device.clone();
            let pending = remote.sessions.lookup(id.clone(), thread.to_owned());
            async move { (id, pending.await) }
        })
        .collect();
    let answers = futures::future::join_all(lookups).await;
    if answers
        .iter()
        .any(|(_, answer)| *answer == RemoteLookupOutcome::Unavailable)
    {
        return None;
    }
    let holders: Vec<(&str, &str)> = answers
        .iter()
        .filter_map(|(device, answer)| match answer {
            RemoteLookupOutcome::Found { key } => Some((device.as_str(), key.as_str())),
            _ => None,
        })
        .collect();
    if let [(device, key)] = holders.as_slice() {
        return Some(Ok(probed(
            &ended_row(device, key, thread),
            Resolution::Probe,
        )));
    }
    if holders.len() > 1 {
        let candidates: Vec<Value> = holders
            .iter()
            .map(|(device, _)| {
                json!({
                    "thread": thread,
                    "name": "",
                    "cwd": "",
                    "preview": "",
                    "updatedAt": null,
                    "state": "resumable",
                    "device": device,
                })
            })
            .collect();
        return Some(Err(refusal(
            "ambiguous_target",
            format!("Session {thread} is held on more than one device; none was picked."),
            "Address the session on one device with <device>/<session id>.",
            Some(json!({ "candidates": candidates })),
        )));
    }
    let silent: Vec<SilentDevice> = answers
        .iter()
        .filter_map(|(device, answer)| match answer {
            RemoteLookupOutcome::Silent { reason } => Some(SilentDevice {
                id: device.clone(),
                name: name_of(Some(devices.as_slice()), device),
                reason: Some(reason.clone()),
            }),
            _ => None,
        })
        .collect();
    let mut answered: Vec<String> = here.map(str::to_owned).into_iter().collect();
    answered.extend(
        answers
            .iter()
            .filter(|(_, answer)| !matches!(answer, RemoteLookupOutcome::Silent { .. }))
            .map(|(device, _)| device.clone()),
    );
    if !silent.is_empty() {
        return Some(Err(tool_refusal(
            &devices_silent(SilentReason::DeviceUnanswered, &silent),
            Some(thread),
            Some(answered.as_slice()),
        )));
    }
    Some(Err(refusal(
        "not_found",
        format!("No online device of this account holds session {thread}."),
        "The session may live on a device that is offline: retry when that device is online, or call thread_list (tool.thread_list() in an eval cell).",
        Some(json!({ "kind": "thread", "thread": thread })),
    )))
}

/// That device's listing and the roster as the resolution already read them: the first pass reads nothing again.
pub struct Known {
    pub listing: RemoteListing,
    pub roster: Option<Vec<AccountDevice>>,
}

fn on_pin(mut refused: ThreadRefusal, pin: &Pin<'_>, reason: Option<&str>) -> ThreadRefusal {
    let mut details = match refused.details.take() {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    details.insert("thread".to_owned(), json!(pin.thread));
    details.insert("device".to_owned(), json!(pin.device));
    if let Some(reason) = reason {
        details.insert("reason".to_owned(), json!(reason));
    }
    refused.details = Some(Value::Object(details));
    refused
}

async fn pinned_device(
    remote: &RemoteThreadSurface,
    known: &Option<Known>,
    pin: &Pin<'_>,
) -> (String, String) {
    let name = match known {
        Some(known) => name_of(known.roster.as_deref(), pin.device),
        None => match account_devices(&remote.client).await {
            Ok(read) => name_of(Some(read.devices.as_slice()), pin.device),
            Err(_) => pin.device.to_owned(),
        },
    };
    (pin.device.to_owned(), name)
}

async fn listed_once(
    remote: &RemoteThreadSurface,
    pin: &Pin<'_>,
    given: Option<RemoteListing>,
    known: &Option<Known>,
) -> Option<Result<Found, ThreadRefusal>> {
    let listing = match given {
        Some(listing) => listing,
        None => match device_listing(remote, pin.device).await {
            Ok(listing) => listing,
            Err(message) => {
                return Some(Err(refusal(
                    "host_unavailable",
                    "The account's session listing could not be reached.",
                    "Retry when the device service is reachable.",
                    Some(json!({ "reason": message })),
                )));
            }
        },
    };
    let wanted = SessionRef {
        device: pin.device.to_owned(),
        thread: Some(pin.thread.to_owned()),
        key: None,
    };
    let row = listing.rows.iter().find(|candidate| {
        same_session(
            &wanted,
            &SessionRef {
                device: candidate.device.clone(),
                thread: candidate.durable_id.clone(),
                key: Some(candidate.session_id.clone()),
            },
        )
    });
    if let Some(row) = row {
        return Some(Ok(Found {
            entry: relay_address_entry(row, &[]),
            resolution: Resolution::Id,
            all_scope: Some(true),
        }));
    }
    if listing.incomplete.iter().any(|id| id == pin.device) {
        let (id, name) = pinned_device(remote, known, pin).await;
        let silent = [SilentDevice {
            id,
            name,
            reason: None,
        }];
        return Some(Err(on_pin(
            tool_refusal(
                &devices_silent(SilentReason::ListingIncomplete, &silent),
                None,
                None,
            ),
            pin,
            None,
        )));
    }
    None
}

/// Amendment A4.3.1 of #13037: an approved reply to a sender whose delivery came from another device resolves on THAT
/// device and nowhere else: its listing row (a read of that one device), else one probe of it, which finds a session it
/// holds but does not run as an ended row. #14280: the listing finds the sender's key; it never decides the sender is
/// gone. A row miss is read again once, and each refusal names its own reason: the device missed the round
/// (`listing_incomplete`), this device cannot reach the account or cannot ask the device (`remote_unavailable`,
/// `wake_unavailable`, `lookup_unavailable`), the probe got no answer (the service's own reason). `not_found` only when
/// the device answered twice that it lists no such session and its probe answered that it holds none.
pub async fn pinned(
    remote: Option<&RemoteThreadSurface>,
    pin: &Pin<'_>,
    status: &mut StatusRound<'_>,
    known: Option<Known>,
) -> Result<Found, ThreadRefusal> {
    let Some(remote) = remote else {
        let core = device_not_asked(NotAskedReason::Remote, pin.thread, pin.device, pin.device);
        return Err(tool_refusal(&core, None, None));
    };
    status.ready().await?;
    let first = listed_once(
        remote,
        pin,
        known.as_ref().map(|known| known.listing.clone()),
        &known,
    )
    .await;
    if let Some(first) = first {
        return first;
    }
    let may_probe = probe_may_build_ended_row(&remote.sessions).await;
    let lookup = if may_probe {
        Some(
            remote
                .sessions
                .lookup(pin.device.to_owned(), pin.thread.to_owned())
                .await,
        )
    } else {
        None
    };
    match &lookup {
        Some(RemoteLookupOutcome::Found { key }) => {
            return Ok(probed(
                &ended_row(pin.device, key, pin.thread),
                Resolution::Probe,
            ));
        }
        Some(RemoteLookupOutcome::Silent { reason }) => {
            let (id, name) = pinned_device(remote, &known, pin).await;
            let silent = [SilentDevice {
                id,
                name,
                reason: Some(reason.clone()),
            }];
            return Err(on_pin(
                tool_refusal(
                    &devices_silent(SilentReason::DeviceUnanswered, &silent),
                    Some(pin.thread),
                    None,
                ),
                pin,
                Some(reason),
            ));
        }
        _ => {}
    }
    if let Some(again) = listed_once(remote, pin, None, &known).await {
        return again;
    }
    let (id, name) = pinned_device(remote, &known, pin).await;
    if !may_probe {
        let core = device_not_asked(NotAskedReason::Wake, pin.thread, &id, &name);
        return Err(tool_refusal(&core, None, None));
    }
    if !matches!(lookup, Some(RemoteLookupOutcome::NotHeld)) {
        let core = device_not_asked(NotAskedReason::Lookup, pin.thread, &id, &name);
        return Err(tool_refusal(&core, None, None));
    }
    Err(tool_refusal(
        &no_thread_on_device(pin.thread, &id, &name),
        None,
        None,
    ))
}
