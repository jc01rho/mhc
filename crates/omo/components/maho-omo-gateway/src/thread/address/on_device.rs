//! The thread part of `<device>/<thread>` among that device's threads only (omo `thread/address/on-device.ts`, #14305,
//! #14328, design section 3).

use maho_thread_address_core::{
    DeviceListing, DeviceRef, DeviceRow, SessionRef, SilentDevice, SilentReason, device_offline,
    devices_silent, id_shaped, is_thread_id, no_thread_on_device, same_session,
};

use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::remote::address_book::{named_device_row, relay_address_entry};
use crate::thread::remote::ports::{RemoteListing, RemoteSessionRow, RemoteThreadSurface};

use super::account::{Account, StatusRound};
use super::bare::{device_name_of, listed_entry};
use super::device::device_rows;
use super::holder::{RemoteTarget, holder_target};
use super::ladder::ladder;
use super::probe::{Known, pinned};
use super::refusals::{ThreadRefusal, tool_refusal, with_addresses};
use super::types::{Found, Pin, Resolution, ResolveContext, ResolveOptions};

async fn alias_on(
    remote: &RemoteThreadSurface,
    account: &Account,
    device: &str,
    alias: &str,
) -> Option<Found> {
    let aliases = remote.aliases.as_ref()?;
    let holder = aliases.resolve(alias.to_owned()).await.ok()?;
    if holder.device != device {
        return None;
    }
    let target = holder_target(
        remote,
        &holder,
        account.here.as_deref(),
        &account.rows,
        account.roster.as_deref(),
    )
    .await;
    match target {
        Ok(RemoteTarget::Remote { row }) => Some(Found {
            entry: listed_entry(&row, &[alias.to_owned()]),
            resolution: Resolution::Alias,
            all_scope: Some(true),
        }),
        _ => None,
    }
}

/// An offline device cannot answer: what the rows in hand do not resolve is `device_offline`, and nothing is sent to it
/// (design section 5; review M2 of #14328).
fn offline(account: &Account, device: &DeviceRef) -> ThreadRefusal {
    let rows = device_rows(account.roster.as_deref().unwrap_or(&[]));
    let row = rows
        .into_iter()
        .find(|candidate| candidate.id == device.id)
        .unwrap_or_else(|| DeviceRow {
            id: device.id.clone(),
            name: device.name.clone(),
            online: false,
            last_seen_ms: None,
        });
    tool_refusal(&device_offline(&device.name, &row, &[]), None, None)
}

/// The thread part of `<device>/<thread>` among that device's threads only. This device: its own book, workspace scope as
/// for a bare address. Another device: its listing rows; a thread id is answered by the device itself when no row lists
/// it yet (a session opened a moment ago, #12709). A miss on a device that did not answer this round is
/// `host_unavailable`, never `not_found`.
pub async fn resolve_on_device(
    ctx: &ResolveContext<'_>,
    remote: &RemoteThreadSurface,
    account: &Account,
    device: &DeviceRef,
    thread: &str,
    opts: &ResolveOptions<'_>,
    status: &mut StatusRound<'_>,
) -> Result<Found, ThreadRefusal> {
    if device.here {
        let owned = ctx.local_for.map(|local_for| local_for(thread));
        let entries: &[ThreadAddressEntry] = owned.as_deref().unwrap_or(ctx.local);
        return match ladder(ctx, entries, thread, opts) {
            Ok((entry, resolution)) => Ok(Found {
                entry,
                resolution,
                all_scope: Some(opts.all_scope),
            }),
            Err(error) => Err(with_addresses(
                *error,
                account.here.as_deref(),
                device_name_of(Some(account)),
            )),
        };
    }
    let rows: Vec<RemoteSessionRow> = account
        .rows
        .iter()
        .filter(|row| row.device == device.id)
        .cloned()
        .collect();
    let wanted = SessionRef {
        device: device.id.clone(),
        thread: Some(thread.to_owned()),
        key: Some(thread.to_owned()),
    };
    let listed = rows.iter().find(|row| {
        same_session(
            &wanted,
            &SessionRef {
                device: row.device.clone(),
                thread: row.durable_id.clone(),
                key: Some(row.session_id.clone()),
            },
        )
    });
    if let Some(listed) = listed {
        let resolution = if is_thread_id(thread) {
            Resolution::Id
        } else {
            Resolution::SessionId
        };
        return Ok(Found {
            entry: listed_entry(listed, &[]),
            resolution,
            all_scope: Some(true),
        });
    }
    if is_thread_id(thread) && !device.online {
        return Err(offline(account, device));
    }
    if is_thread_id(thread) {
        let known = Known {
            listing: RemoteListing {
                rows,
                incomplete: account.incomplete.clone(),
                devices: account.round.states.clone(),
            },
            roster: account.roster.clone(),
        };
        let pin = Pin {
            device: &device.id,
            thread,
        };
        return pinned(Some(remote), &pin, status, Some(known)).await;
    }
    let entries: Vec<ThreadAddressEntry> = rows.iter().map(|row| listed_entry(row, &[])).collect();
    let wide = ResolveOptions {
        all_scope: true,
        ..*opts
    };
    match ladder(ctx, &entries, thread, &wide) {
        Ok((entry, resolution)) => {
            return Ok(Found {
                entry,
                resolution,
                all_scope: Some(true),
            });
        }
        Err(error) if error.code.as_str() == "ambiguous_target" => {
            return Err(with_addresses(
                *error,
                account.here.as_deref(),
                device_name_of(Some(account)),
            ));
        }
        Err(_) => {}
    }
    if let Some(alias) = alias_on(remote, account, &device.id, thread).await {
        return Ok(alias);
    }
    if !device.online {
        return Err(offline(account, device));
    }
    if id_shaped(thread) {
        let mut row = named_device_row(&device.id, thread);
        row.durable_id = None;
        return Ok(Found {
            entry: relay_address_entry(&row, &[]),
            resolution: Resolution::SessionId,
            all_scope: Some(true),
        });
    }
    if device.listing == DeviceListing::Incomplete {
        let silent = [SilentDevice {
            id: device.id.clone(),
            name: device.name.clone(),
            reason: None,
        }];
        return Err(tool_refusal(
            &devices_silent(SilentReason::ListingIncomplete, &silent),
            None,
            None,
        ));
    }
    Err(tool_refusal(
        &no_thread_on_device(thread, &device.id, &device.name),
        None,
        None,
    ))
}
