//! The one resolver (design sections 2-5): `self`, a bare address, or `<device>/<thread>`.
//!
//! Upstream counterpart: `resolve.ts` (#14305). Over this device's book and the account (`ResolveContext.remote`), read
//! at most once. A device reading is taken only when the first part names a device; when it misses there, or names no
//! device, the whole text is resolved as a bare address (a thread name may contain `/`), and the device's refusal answers
//! only when that also misses. Both readings resolving to different threads is ambiguous. Without a remote surface
//! (`ctx.remote === undefined` upstream) a pinned reply answers `remote_unavailable`, `here/<thread>` asks this device's
//! book (calling `local_for` with the thread part when provided, #14305 review M2), and any other `<device>/<thread>`
//! resolves as the whole text against this device's book.

use maho_thread_address_core::{
    HERE, ParsedAddress, ResolveDeviceOptions, SilentDevice, SilentReason, devices_silent,
    format_address, id_shaped, no_device, no_thread_on_device, parse_address,
};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::thread::addressing::{ThreadAddressEntry, candidate_thread};
use crate::thread::remote::address_book::relay_address_entry;
use crate::thread::remote::ports::RemoteThreadSurface;

use super::account::{
    Account, AccountAttempt, AccountRead, AccountReads, Reads, account_devices, read_account,
    read_roster,
};
use super::bare::{BareResolved, device_name_of, resolve_bare};
use super::device::{AccountDevice, AccountDevices, device_rows, resolve_account_device};
use super::on_device::resolve_on_device;
use super::probe::pinned;
use super::refusals::{ThreadRefusal, refusal, tool_refusal, with_addresses};
use super::types::{Found, ResolveContext, ResolveOptions, Resolved, ResolvedWhere};
use maho_thread_address_core::{DeviceListing, DeviceRef};

fn ref_of(row: Option<&AccountDevice>, id: &str, here: Option<&str>) -> DeviceRef {
    DeviceRef {
        id: id.to_owned(),
        name: row.map_or_else(|| id.to_owned(), |row| row.label.clone()),
        online: row.is_none_or(|row| row.online),
        here: Some(id) == here,
        listing: match row {
            Some(row) if !row.online => DeviceListing::Offline,
            _ => DeviceListing::Listed,
        },
    }
}

/// The devices the online check reads: the account's roster when this resolution read one, else one device list.
async fn roster_for(
    remote: &RemoteThreadSurface,
    account: Option<&Account>,
    device: &str,
) -> Result<Vec<AccountDevice>, ThreadRefusal> {
    if let Some(roster) = account.and_then(|read| read.roster.clone()) {
        return Ok(roster);
    }
    match account_devices(remote.client.as_ref()).await {
        Ok(read) => Ok(read.devices),
        Err(message) => Err(refusal(
            "host_unavailable",
            "This device's device service could not list the account's devices.",
            "Retry when the device service is reachable.",
            Some(json!({ "device": device, "reason": message })),
        )),
    }
}

async fn finish(
    ctx: &ResolveContext<'_>,
    hit: Result<Found, ThreadRefusal>,
    account: Option<&Account>,
    opts: &ResolveOptions<'_>,
) -> Result<Resolved, ThreadRefusal> {
    let hit = hit?;
    let (Some(row), Some(remote)) = (hit.entry.row.clone(), ctx.remote) else {
        let here = account.and_then(|read| read.here.as_deref());
        let self_row = account.and_then(|read| {
            read.devices
                .iter()
                .find(|device| Some(device.device.as_str()) == here)
        });
        let device = here.map(|id| ref_of(self_row, id, Some(id)));
        let thread = hit.entry.thread_id.clone();
        let address = match &device {
            None => thread.clone(),
            Some(device) => format_address(&device.id, &thread),
        };
        return Ok(Resolved {
            thread,
            address,
            device,
            where_: ResolvedWhere::Here {
                entry: hit.entry.clone(),
            },
            entry: hit.entry,
            resolution: hit.resolution,
            all_scope: hit.all_scope,
        });
    };
    let roster: Option<Vec<AccountDevice>> = if opts.need_online || account.is_none() {
        Some(roster_for(remote, account, &row.device).await?)
    } else {
        account.and_then(|read| read.roster.clone())
    };
    let listed = roster
        .as_ref()
        .and_then(|devices| devices.iter().find(|device| device.device == row.device))
        .cloned();
    if opts.need_online {
        let Some(listed) = &listed else {
            let known = device_rows(roster.as_deref().unwrap_or(&[]));
            return Err(tool_refusal(&no_device(&row.device, &known), None, None));
        };
        let only = AccountDevices {
            here: account.and_then(|read| read.here.clone()),
            devices: vec![listed.clone()],
            listing_of: None,
        };
        resolve_account_device(
            &only,
            &row.device,
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        )?;
    }
    let mut device = ref_of(
        listed.as_ref(),
        &row.device,
        account.and_then(|read| read.here.as_deref()),
    );
    if let Some(read) = account {
        device.listing = read.listing_of(&row.device);
    }
    let thread = hit.entry.thread_id.clone();
    let key = if thread.is_empty() {
        row.session_id.clone()
    } else {
        thread.clone()
    };
    Ok(Resolved {
        address: format_address(&row.device, &key),
        thread,
        device: Some(device),
        where_: ResolvedWhere::Device { row },
        entry: hit.entry,
        resolution: hit.resolution,
        all_scope: hit.all_scope,
    })
}

fn candidate_json(entry: &ThreadAddressEntry, device: &str) -> Value {
    json!({
        "thread": candidate_thread(entry),
        "name": entry.name,
        "cwd": entry.cwd,
        "preview": "",
        "updatedAt": entry.updated_at,
        "state": entry.status.as_str(),
        "device": device,
    })
}

fn ambiguous(text: &str, readings: &[Found], account: &Account) -> ThreadRefusal {
    let candidates: Vec<Value> = readings
        .iter()
        .map(|reading| {
            let device = reading
                .entry
                .row
                .as_ref()
                .map_or("here", |row| row.device.as_str());
            candidate_json(&reading.entry, device)
        })
        .collect();
    let failure = refusal(
        "ambiguous_target",
        format!(
            "\"{text}\" names a thread on a device and a thread named \"{text}\"; none was picked."
        ),
        "Pass the exact thread value of the intended thread from candidates.",
        Some(json!({ "candidates": candidates })),
    );
    with_addresses(
        *failure,
        account.here.as_deref(),
        device_name_of(Some(account)),
    )
}

/// #14048 (`unique_holder`): the refusal when more than one device lists the resolved thread id, this device's book
/// included, one candidate per device; `None` when one device does. Read from the resolution's own account read.
async fn other_holder(
    ctx: &ResolveContext<'_>,
    hit: &Found,
    reads: &mut Reads<'_>,
) -> Option<ThreadRefusal> {
    let account = match reads.account().await {
        AccountAttempt::None => return None,
        AccountAttempt::Unavailable(unavailable) => return Some(unavailable),
        AccountAttempt::Failed(message) => {
            return Some(refusal(
                "host_unavailable",
                "The account's session listing could not be read to confirm the session's device.",
                "Retry when the device service is reachable.",
                Some(json!({ "reason": message })),
            ));
        }
        AccountAttempt::Account(account) => account,
    };
    let thread = hit.entry.thread_id.as_str();
    let mut by_device: Vec<(String, ThreadAddressEntry)> = Vec::new();
    let local = ctx.local.iter().filter(|entry| entry.thread_id == thread);
    let remote_rows = account
        .rows
        .iter()
        .filter(|row| {
            Some(row.device.as_str()) != account.here.as_deref()
                && row.durable_id.as_deref().unwrap_or_default() == thread
        })
        .map(|row| relay_address_entry(row, &[]));
    let entries: Vec<ThreadAddressEntry> = local.cloned().chain(remote_rows).collect();
    for entry in entries {
        let device = entry
            .row
            .as_ref()
            .map_or_else(|| "here".to_owned(), |row| row.device.clone());
        match by_device.iter().position(|(known, _)| *known == device) {
            Some(position) => by_device[position].1 = entry,
            None => by_device.push((device, entry)),
        }
    }
    if by_device.len() <= 1 {
        return None;
    }
    let candidates: Vec<Value> = by_device
        .iter()
        .map(|(device, entry)| {
            let mut candidate = candidate_json(entry, device);
            candidate["thread"] = Value::String(thread.to_owned());
            candidate
        })
        .collect();
    let failure = refusal(
        "ambiguous_target",
        format!(
            "Session {thread} is listed on more than one device and nothing names which one sent it; none was picked."
        ),
        "Address the session on one device with <device>/<session id>.",
        Some(json!({ "candidates": candidates })),
    );
    Some(with_addresses(
        *failure,
        account.here.as_deref(),
        device_name_of(Some(&*account)),
    ))
}

async fn bare(
    book: &ResolveContext<'_>,
    address: &str,
    opts: &ResolveOptions<'_>,
    reads: &mut Reads<'_>,
) -> Result<Resolved, ThreadRefusal> {
    let BareResolved { result, account } = resolve_bare(book, address, opts, reads).await;
    let held = match &result {
        Ok(found) if opts.unique_holder => other_holder(book, found, reads).await,
        _ => None,
    };
    let result = match held {
        Some(refused) => Err(refused),
        None => result,
    };
    finish(book, result, account.as_deref(), opts).await
}

enum OneRead {
    Account(Arc<Account>),
    None,
    Refused(ThreadRefusal),
}

/// The resolution without an account answer for `<device>/<thread>`: this device's book for `here`, else the whole text.
async fn without_account(
    ctx: &ResolveContext<'_>,
    text: &str,
    device: &str,
    thread: &str,
    opts: &ResolveOptions<'_>,
    reads: &mut Reads<'_>,
) -> Result<Resolved, ThreadRefusal> {
    if device != HERE {
        return bare(ctx, text, opts, reads).await;
    }
    let owned = ctx.local_for.map(|local_for| local_for(thread));
    let local: &[ThreadAddressEntry] = owned.as_deref().unwrap_or(ctx.local);
    let book = ResolveContext { local, ..*ctx };
    bare(&book, thread, opts, reads).await
}

/// Resolves `text` (`self`, a bare address, or `<device>/<thread>`) against `ctx`.
pub async fn resolve_address(
    ctx: &ResolveContext<'_>,
    text: &str,
    opts: &ResolveOptions<'_>,
) -> Result<Resolved, ThreadRefusal> {
    let mut reads = Reads::new(ctx.remote);
    if let Some(pin) = &opts.pin {
        let found = pinned(ctx.remote, pin, &mut reads.status, None).await;
        return finish(ctx, found, None, opts).await;
    }
    let (device_part, thread_part) = match parse_address(text) {
        ParsedAddress::SelfThread => return bare(ctx, "self", opts, &mut reads).await,
        ParsedAddress::Bare { .. } => return bare(ctx, text, opts, &mut reads).await,
        ParsedAddress::Device { device, thread, .. } => (device, thread),
    };
    let roster_read = match ctx.remote {
        None => None,
        Some(remote) => {
            let ready = reads.status.ready().await;
            Some(read_roster(remote, ready).await)
        }
    };
    let (Some(remote), Some(AccountRead::Account(roster_account))) = (ctx.remote, roster_read)
    else {
        return without_account(ctx, text, &device_part, &thread_part, opts, &mut reads).await;
    };
    let roster = AccountDevices {
        here: roster_account.here.clone(),
        devices: roster_account.devices.clone(),
        listing_of: None,
    };
    let device = match resolve_account_device(
        &roster,
        &device_part,
        ResolveDeviceOptions {
            online: opts.need_online,
            exact_id: false,
        },
    ) {
        Ok(device) => device,
        Err(device_refusal) => {
            let whole = resolve_bare(ctx, text, opts, &mut reads).await;
            return match whole.result {
                Ok(found) => finish(ctx, Ok(found), whole.account.as_deref(), opts).await,
                Err(_) => Err(device_refusal),
            };
        }
    };
    let one = if device.here {
        OneRead::Account(Arc::clone(&roster_account))
    } else {
        let ready = reads.status.ready().await;
        let given = AccountReads {
            ready: Some(ready),
            roster: Some(roster_account.roster.clone()),
            device: Some(device.id.clone()),
        };
        match read_account(remote, given).await {
            Ok(AccountRead::Account(account)) => OneRead::Account(account),
            Ok(AccountRead::None) => OneRead::None,
            Ok(AccountRead::Unavailable(unavailable)) => OneRead::Refused(unavailable),
            Err(message) => OneRead::Refused(refusal(
                "host_unavailable",
                "The account's session listing could not be reached.",
                "Retry when the device service is reachable.",
                Some(json!({ "reason": message })),
            )),
        }
    };
    let one = match one {
        OneRead::Account(account) => account,
        OneRead::None => return bare(ctx, text, opts, &mut reads).await,
        OneRead::Refused(refused) => return Err(refused),
    };
    let mut device = device;
    device.listing = one.listing_of(&device.id);
    let one: &Account = &one;
    let on_device = resolve_on_device(
        ctx,
        remote,
        one,
        &device,
        &thread_part,
        opts,
        &mut reads.status,
    )
    .await;
    match on_device {
        Ok(found) => {
            if id_shaped(&thread_part) {
                return finish(ctx, Ok(found), Some(one), opts).await;
            }
            let book = ResolveContext {
                remote: None,
                ..*ctx
            };
            let here = resolve_bare(&book, text, opts, &mut reads).await;
            if let Ok(local) = &here.result
                && local.entry.thread_id != found.entry.thread_id
            {
                return Err(ambiguous(text, &[found, local.clone()], one));
            }
            finish(ctx, Ok(found), Some(one), opts).await
        }
        Err(refused) => {
            if id_shaped(&thread_part) {
                return Err(refused);
            }
            let whole = resolve_bare(ctx, text, opts, &mut reads).await;
            match whole.result {
                Ok(found) => finish(ctx, Ok(found), whole.account.as_deref(), opts).await,
                Err(_) => Err(refused),
            }
        }
    }
}

/// Re-export helper for tests and callers building silence/thread refusals through `tool_refusal`.
pub fn silent_refusal(
    reason: SilentReason,
    devices: &[SilentDevice],
    thread: Option<&str>,
    answered: Option<&[String]>,
) -> ThreadRefusal {
    tool_refusal(&devices_silent(reason, devices), thread, answered)
}

/// Re-export helper for a thread-not-found refusal on a known device.
pub fn thread_missing_on_device(thread: &str, device_id: &str, device_name: &str) -> ThreadRefusal {
    tool_refusal(
        &no_thread_on_device(thread, device_id, device_name),
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use maho_thread_address_core::{NotAskedReason, device_not_asked};

    use super::*;
    use crate::thread::address::types::{Pin, Resolution};
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::errors::thread_tool_failure;
    use crate::thread::store_extension_session_ops::UNKNOWN_CALLER;

    fn resolve_sync(
        ctx: &ResolveContext<'_>,
        text: &str,
        opts: &ResolveOptions<'_>,
    ) -> Result<Resolved, ThreadRefusal> {
        futures::executor::block_on(resolve_address(ctx, text, opts))
    }

    const HERE_LIVE: &str = "0aa00001-0000-4000-8000-000000000001";
    const HERE_ENDED: &str = "0aa00002-0000-4000-8000-000000000002";
    const HERE_SHARED_1: &str = "0aa00003-0000-4000-8000-000000000003";
    const HERE_SHARED_2: &str = "0aa00005-0000-4000-8000-000000000005";
    const HERE_SLASH: &str = "0aa00004-0000-4000-8000-000000000004";

    fn entry(thread_id: &str, name: &str, route: Option<&str>, cwd: &str) -> ThreadAddressEntry {
        ThreadAddressEntry {
            thread_id: thread_id.to_owned(),
            name: name.to_owned(),
            cwd: cwd.to_owned(),
            status: ThreadStatus::Live,
            updated_at: None,
            session_ids: route.into_iter().map(str::to_owned).collect(),
            device: None,
            aliases: Vec::new(),
            row: None,
        }
    }

    fn local_book() -> Vec<ThreadAddressEntry> {
        vec![
            entry(HERE_LIVE, "alpha", Some("rpc-3"), "/work/a"),
            entry(HERE_SLASH, "fix/login", Some("rpc-4"), "/work/a"),
            entry(HERE_ENDED, "", None, "/work/a"),
        ]
    }

    #[test]
    fn every_local_input_form_resolves_to_the_same_thread() {
        let book = local_book();
        let ctx = ResolveContext {
            remote: None,
            caller_workspace_root: "/work/a",
            local: &book,
            local_for: None,
            caller_id: HERE_LIVE,
        };
        let opts = ResolveOptions::default();
        let cases = [
            (HERE_LIVE, HERE_LIVE, Resolution::Id),
            ("rpc-3", HERE_LIVE, Resolution::SessionId),
            ("alpha", HERE_LIVE, Resolution::Name),
            ("0aa00001", HERE_LIVE, Resolution::IdPrefix),
            ("self", HERE_LIVE, Resolution::SelfThread),
            (
                "here/0aa00001-0000-4000-8000-000000000001",
                HERE_LIVE,
                Resolution::Id,
            ),
            ("here/alpha", HERE_LIVE, Resolution::Name),
            ("here/rpc-3", HERE_LIVE, Resolution::SessionId),
            ("fix/login", HERE_SLASH, Resolution::Name),
        ];
        for (text, expected_thread, expected_res) in cases {
            let resolved = resolve_sync(&ctx, text, &opts).expect(text);
            assert_eq!(resolved.thread, expected_thread, "{text}");
            assert_eq!(resolved.address, expected_thread, "{text}");
            assert_eq!(resolved.resolution, expected_res, "{text}");
            assert!(resolved.device.is_none(), "{text}");
        }
    }

    #[test]
    fn here_qualified_address_asks_local_for_with_the_thread_part_alone() {
        let all = local_book();
        let visible: Vec<ThreadAddressEntry> = all
            .iter()
            .filter(|item| item.thread_id != HERE_ENDED)
            .cloned()
            .collect();
        let local_for = |part: &str| -> Vec<ThreadAddressEntry> {
            if part == HERE_ENDED {
                all.clone()
            } else {
                visible.clone()
            }
        };
        let ctx = ResolveContext {
            remote: None,
            caller_workspace_root: "/work/a",
            local: &visible,
            local_for: Some(&local_for),
            caller_id: HERE_LIVE,
        };
        let hit = resolve_sync(
            &ctx,
            &format!("here/{HERE_ENDED}"),
            &ResolveOptions::default(),
        )
        .expect("here/<file id> resolves through local_for");
        assert_eq!(hit.thread, HERE_ENDED);
        assert_eq!(hit.resolution, Resolution::Id);
    }

    #[test]
    fn refusal_table_codes_and_details_match_design_section_5() {
        let mut book = local_book();
        book.push(entry(HERE_SHARED_1, "shared", None, "/work/a"));
        book.push(entry(HERE_SHARED_2, "shared", None, "/work/a"));
        let ctx = ResolveContext {
            remote: None,
            caller_workspace_root: "/work/a",
            local: &book,
            local_for: None,
            caller_id: HERE_LIVE,
        };
        let opts = ResolveOptions::default();

        let empty = resolve_sync(&ctx, "", &opts).expect_err("empty");
        assert_eq!(empty.code.as_str(), "invalid_arguments");

        let ctrl = resolve_sync(&ctx, "a\u{0001}b", &opts).expect_err("control");
        assert_eq!(ctrl.code.as_str(), "invalid_arguments");

        let missing = resolve_sync(&ctx, "no thread is named so", &opts).expect_err("missing");
        assert_eq!(missing.code.as_str(), "not_found");
        assert_eq!(missing.details, Some(json!({ "kind": "thread" })));

        let shared = resolve_sync(&ctx, "shared", &opts).expect_err("shared");
        assert_eq!(shared.code.as_str(), "ambiguous_target");
        assert_eq!(
            shared.details.as_ref().and_then(|d| d.get("kind")),
            Some(&json!("thread"))
        );

        let unknown_caller = ResolveContext {
            caller_id: UNKNOWN_CALLER,
            ..ctx
        };
        let self_err = resolve_sync(&unknown_caller, "self", &opts).expect_err("unknown caller");
        assert_eq!(self_err.code.as_str(), "caller_context_missing");

        let other_ws = ResolveContext {
            remote: None,
            caller_workspace_root: "/work/other",
            ..ctx
        };
        let denied = resolve_sync(&other_ws, "alpha", &opts).expect_err("scope denied");
        assert_eq!(denied.code.as_str(), "scope_denied");

        let no_root = ResolveContext {
            remote: None,
            caller_workspace_root: "   ",
            ..ctx
        };
        let fuzzy_opts = ResolveOptions {
            fuzzy: true,
            ..opts
        };
        let fuzzy_err = resolve_sync(&no_root, "alpha", &fuzzy_opts).expect_err("fuzzy no root");
        assert_eq!(fuzzy_err.code.as_str(), "caller_context_missing");

        let pinned_opts = ResolveOptions {
            pin: Some(Pin {
                device: "b7c41e09d2a64f1e",
                thread: "0bb00001-0000-4000-8000-000000000001",
            }),
            ..opts
        };
        let pin_err = resolve_sync(&ctx, "0bb00001-0000-4000-8000-000000000001", &pinned_opts)
            .expect_err("pin without remote");
        assert_eq!(pin_err.code.as_str(), "host_unavailable");
        assert_eq!(
            pin_err.details.as_ref().and_then(|d| d.get("reason")),
            Some(&json!("remote_unavailable"))
        );
    }

    #[test]
    fn account_device_resolution_and_tool_refusal_vocabulary() {
        let account = AccountDevices {
            here: Some("a11ce0de5e1f0123".to_owned()),
            devices: vec![
                AccountDevice {
                    device: "a11ce0de5e1f0123".to_owned(),
                    label: "this mac".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "b7c41e09d2a64f1e".to_owned(),
                    label: "Studio Laptop".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "b7c4ffff00000000".to_owned(),
                    label: "Studio Laptop".to_owned(),
                    online: true,
                    last_seen_ms: None,
                },
                AccountDevice {
                    device: "c0ffee5a1d2b3c4d".to_owned(),
                    label: "attic".to_owned(),
                    online: false,
                    last_seen_ms: Some(1_699_000_000_000),
                },
            ],
            listing_of: None,
        };

        let here = resolve_account_device(
            &account,
            "here",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        )
        .expect("here");
        assert!(here.here);
        assert_eq!(here.listing, DeviceListing::Listed);

        let unknown = resolve_account_device(
            &account,
            "nowhere-device",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("unknown");
        assert_eq!(unknown.code.as_str(), "not_found");
        assert!(unknown.message.contains("No device named nowhere-device"));

        let share_name = resolve_account_device(
            &account,
            "Studio Laptop",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("share name");
        assert_eq!(share_name.code.as_str(), "ambiguous_target");
        assert!(
            share_name
                .message
                .contains("2 online devices are named Studio Laptop")
        );

        let share_prefix = resolve_account_device(
            &account,
            "b7c4",
            ResolveDeviceOptions {
                online: false,
                exact_id: false,
            },
        )
        .expect_err("short prefix is not_found");
        assert_eq!(share_prefix.code.as_str(), "not_found");

        let offline = resolve_account_device(
            &account,
            "attic",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        )
        .expect_err("offline");
        assert_eq!(offline.code.as_str(), "device_offline");
        assert_eq!(
            offline.details.as_ref().and_then(|d| d.get("last_seen")),
            Some(&json!(1_699_000_000_000_i64))
        );

        let exact_miss = resolve_account_device(
            &account,
            "this mac",
            ResolveDeviceOptions {
                online: true,
                exact_id: true,
            },
        )
        .expect_err("exact_id refuses label");
        assert_eq!(exact_miss.code.as_str(), "not_found");
    }

    #[test]
    fn silent_and_not_asked_refusals_keep_reasons_and_misplaced_file_hint() {
        let incomplete = silent_refusal(
            SilentReason::ListingIncomplete,
            &[SilentDevice {
                id: "b7c41e09d2a64f1e".to_owned(),
                name: "Studio Laptop".to_owned(),
                reason: None,
            }],
            None,
            None,
        );
        assert_eq!(incomplete.code.as_str(), "host_unavailable");
        assert!(
            incomplete
                .message
                .contains("Studio Laptop (b7c41e09d2a64f1e)")
        );

        let answered = vec!["a11ce0de5e1f0123".to_owned()];
        let unanswered = silent_refusal(
            SilentReason::DeviceUnanswered,
            &[
                SilentDevice {
                    id: "b7c41e09d2a64f1e".to_owned(),
                    name: "Studio Laptop".to_owned(),
                    reason: Some("lookup_timeout".to_owned()),
                },
                SilentDevice {
                    id: "c1a55e0000000001".to_owned(),
                    name: "closet".to_owned(),
                    reason: Some("host_not_answering".to_owned()),
                },
            ],
            Some(HERE_LIVE),
            Some(&answered),
        );
        assert_eq!(unanswered.code.as_str(), "host_unavailable");
        assert!(unanswered.message.contains("was too slow for the lookup"));
        assert!(unanswered.message.contains("did not answer"));
        assert!(
            unanswered
                .next_action
                .contains("its file is not where that device looks for sessions")
        );
        assert_eq!(
            unanswered
                .details
                .as_ref()
                .and_then(|d| d.get("device_reasons"))
                .and_then(|r| r.get("c1a55e0000000001")),
            Some(&json!("host_not_answering"))
        );

        let wake = tool_refusal(
            &device_not_asked(
                NotAskedReason::Wake,
                HERE_LIVE,
                "b7c41e09d2a64f1e",
                "Studio Laptop",
            ),
            None,
            None,
        );
        assert_eq!(wake.code.as_str(), "host_unavailable");
        assert!(wake.message.contains("cannot carry a wake"));

        let missing_on_b = thread_missing_on_device("no-such", "b7c41e09d2a64f1e", "Studio Laptop");
        assert_eq!(missing_on_b.code.as_str(), "not_found");
        assert!(
            missing_on_b
                .message
                .contains("No thread no-such on Studio Laptop")
        );
    }

    #[test]
    fn with_addresses_enriches_candidates_and_preserves_keyless_row_keys() {
        let raw = thread_tool_failure(
            "ambiguous_target",
            "shared",
            "pick one",
            Some(json!({
                "candidates": [
                    {
                        "thread": HERE_SHARED_1,
                        "name": "shared",
                        "cwd": "/work/a",
                        "preview": "",
                        "updatedAt": null,
                        "state": "live",
                        "device": "here",
                    },
                    {
                        "thread": "b7c41e09d2a64f1e/c2Vzc2lvbi1rZXktMDAwMQ",
                        "name": "shared",
                        "cwd": "/work/b",
                        "preview": "",
                        "updatedAt": null,
                        "state": "live",
                        "device": "b7c41e09d2a64f1e",
                    }
                ]
            })),
        );
        let enriched = with_addresses(raw, Some("a11ce0de5e1f0123"), |device| match device {
            "a11ce0de5e1f0123" => Some("this mac".to_owned()),
            "b7c41e09d2a64f1e" => Some("Studio Laptop".to_owned()),
            _ => None,
        });
        let details = enriched.details.expect("details");
        assert_eq!(details["kind"], "thread");
        let candidates = details["candidates"].as_array().expect("candidates");
        assert_eq!(
            candidates[0]["address"],
            format!("a11ce0de5e1f0123/{HERE_SHARED_1}")
        );
        assert_eq!(candidates[0]["device_name"], "this mac");
        assert_eq!(
            candidates[1]["address"],
            "b7c41e09d2a64f1e/c2Vzc2lvbi1rZXktMDAwMQ"
        );
        assert_eq!(candidates[1]["device_name"], "Studio Laptop");
    }
}
