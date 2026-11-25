//! A bare address over this device's book and the account's (decision 0027: one resolver).
//!
//! Upstream counterpart: `bare.ts` (#14305). A local id hit never reads the account. A configured relay's failure, plan
//! refusal or alias holder's typed failure answers before `not_found`; a thread id nothing lists gets the per-device
//! probe; a miss while a device did not answer this listing round is `host_unavailable` naming it (the remembered device
//! when the memory has one), never `not_found`. No relay set up: a local miss stays a local `not_found`. Without
//! `ResolveContext.remote` this is the local ladder alone, with every refusal enriched by `with_addresses`.

use std::sync::Arc;

use maho_thread_address_core::{
    DeviceListing, SilentDevice, SilentReason, devices_silent, is_thread_id,
};
use serde_json::json;

use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadStatus;
use crate::thread::errors::thread_tool_failure;
use crate::thread::remote::address_book::relay_address_entry;
use crate::thread::remote::ports::{RemoteSessionRow, RemoteThreadSurface};

use super::account::{Account, AccountAttempt, Reads};
use super::holder::{RemoteTarget, holder_target};
use super::ladder::ladder;
use super::probe::probe_across;
use super::refusals::{ThreadRefusal, refusal, tool_refusal, with_addresses};
use super::types::{Found, Resolution, ResolveContext, ResolveOptions};

/// A bare resolution's answer, and the account it read (when it read one).
pub struct BareResolved {
    pub result: Result<Found, ThreadRefusal>,
    pub account: Option<Arc<Account>>,
}

#[derive(Default)]
struct Book {
    entries: Vec<ThreadAddressEntry>,
    account: Option<Arc<Account>>,
    unavailable: Option<ThreadRefusal>,
    alias_failure: Option<ThreadRefusal>,
}

/// A listing row as an address entry. A row that publishes no thread id is addressed by its listing key alone, so the
/// key is its session id (the ladder's session-id step); `relay_address_entry` gives it none.
pub fn listed_entry(row: &RemoteSessionRow, aliases: &[String]) -> ThreadAddressEntry {
    let mut entry = relay_address_entry(row, aliases);
    if entry.thread_id.is_empty() && !row.session_id.is_empty() {
        entry.session_ids = vec![row.session_id.clone()];
    }
    entry
}

/// The name a device carries in the account's device list, for refusals that name devices.
pub fn device_name_of(account: Option<&Account>) -> impl Fn(&str) -> Option<String> + '_ {
    move |device: &str| {
        account.and_then(|account| {
            account
                .devices
                .iter()
                .find(|row| row.device == device)
                .map(|row| row.label.clone())
        })
    }
}

fn found(entry: ThreadAddressEntry, resolution: Resolution, all_scope: bool) -> Found {
    let wide = resolution == Resolution::Alias || entry.row.is_some();
    Found {
        entry,
        resolution,
        all_scope: Some(wide || all_scope),
    }
}

/// The account's threads on OTHER devices as address entries, plus the holder of `text` when it is an account alias (the
/// registry resolves one alias at a time). A holder on this device becomes a local entry carrying the alias; a holder
/// elsewhere is its listing row on that device.
async fn account_book(remote: &RemoteThreadSurface, text: &str, read: &AccountAttempt) -> Book {
    let account = match read {
        AccountAttempt::Failed(message) => {
            return Book {
                unavailable: Some(refusal(
                    "host_unavailable",
                    "The account's session listing could not be reached.",
                    "Retry when the device service is reachable.",
                    Some(json!({ "reason": message })),
                )),
                ..Book::default()
            };
        }
        AccountAttempt::None => return Book::default(),
        AccountAttempt::Unavailable(unavailable) => {
            return Book {
                unavailable: Some(unavailable.clone()),
                ..Book::default()
            };
        }
        AccountAttempt::Account(account) => account,
    };
    let here = account.here.as_deref();
    let entries: Vec<ThreadAddressEntry> = account
        .rows
        .iter()
        .filter(|row| Some(row.device.as_str()) != here)
        .map(|row| listed_entry(row, &[]))
        .collect();
    let mut book = Book {
        entries,
        account: Some(Arc::clone(account)),
        ..Book::default()
    };
    let Some(aliases) = &remote.aliases else {
        return book;
    };
    let Ok(holder) = aliases.resolve(text.to_owned()).await else {
        return book;
    };
    let target = holder_target(
        remote,
        &holder,
        here,
        &account.rows,
        account.roster.as_deref(),
    )
    .await;
    match target {
        Err(failure) => book.alias_failure = Some(failure),
        Ok(RemoteTarget::Here { session_id }) => book.entries.push(ThreadAddressEntry {
            thread_id: session_id,
            name: text.to_owned(),
            cwd: String::new(),
            status: ThreadStatus::Live,
            updated_at: None,
            session_ids: Vec::new(),
            device: None,
            aliases: vec![text.to_owned()],
            row: None,
        }),
        Ok(RemoteTarget::Remote { row }) => {
            book.entries
                .retain(|entry| entry.row.as_ref().is_none_or(|existing| **existing != *row));
            book.entries.push(listed_entry(&row, &[text.to_owned()]));
        }
    }
    book
}

/// The devices a miss cannot rule out: the one an earlier round listed `text` on, else every device this round missed.
fn silent_devices(account: &Account, text: &str) -> Vec<String> {
    if let Some(remembered) = account.missed_holder(text) {
        return vec![remembered];
    }
    let mut missed: Vec<String> = Vec::new();
    for device in &account.incomplete {
        if Some(device.as_str()) != account.here.as_deref()
            && account.listing_of(device) == DeviceListing::Incomplete
            && !missed.contains(device)
        {
            missed.push(device.clone());
        }
    }
    missed
}

pub async fn resolve_bare(
    ctx: &ResolveContext<'_>,
    text: &str,
    opts: &ResolveOptions<'_>,
    reads: &mut Reads<'_>,
) -> BareResolved {
    if opts.fuzzy && !opts.all_scope && ctx.caller_workspace_root.trim().is_empty() {
        return BareResolved {
            result: Err(Box::new(thread_tool_failure(
                "caller_context_missing",
                "Scoping a fuzzy handoff needs the caller's workspace root, which this call did not provide.",
                "Set all_scope to search every workspace, or hand off from a session that knows its workspace.",
                None,
            ))),
            account: None,
        };
    }
    let here = ladder(ctx, ctx.local, text, opts);
    let exact_here = matches!(
        &here,
        Ok((_, resolution))
            if !matches!(resolution, Resolution::Alias | Resolution::Name | Resolution::IdPrefix)
    );
    if let Ok((entry, resolution)) = &here
        && (ctx.remote.is_none() || exact_here)
    {
        return BareResolved {
            result: Ok(found(entry.clone(), *resolution, opts.all_scope)),
            account: None,
        };
    }
    let Some(remote) = ctx.remote else {
        let result = match here {
            Ok((entry, resolution)) => Ok(found(entry, resolution, opts.all_scope)),
            Err(error) => Err(with_addresses(*error, None, |_| None)),
        };
        return BareResolved {
            result,
            account: None,
        };
    };
    let attempt = reads.account().await;
    let book = account_book(remote, text, &attempt).await;
    let account = book.account.clone();
    if book.entries.is_empty()
        && let Ok((entry, resolution)) = &here
    {
        return BareResolved {
            result: Ok(found(entry.clone(), *resolution, opts.all_scope)),
            account,
        };
    }
    let mut all: Vec<ThreadAddressEntry> = ctx.local.to_vec();
    all.extend(book.entries.iter().cloned());
    let across_error = match ladder(ctx, &all, text, opts) {
        Ok((entry, resolution)) => {
            return BareResolved {
                result: Ok(found(entry, resolution, opts.all_scope)),
                account,
            };
        }
        Err(error) => error,
    };
    if let Some(alias_failure) = book.alias_failure {
        return BareResolved {
            result: Err(alias_failure),
            account,
        };
    }
    let missed_here = matches!(&here, Err(error) if error.code.as_str() == "not_found");
    if missed_here && let Some(unavailable) = book.unavailable {
        return BareResolved {
            result: Err(unavailable),
            account,
        };
    }
    let not_found_across = across_error.code.as_str() == "not_found";
    if opts.probe && not_found_across && is_thread_id(text) {
        let looked = probe_across(
            remote,
            text,
            account.as_deref().and_then(|read| read.roster.as_deref()),
            account.as_deref().and_then(|read| read.here.as_deref()),
        )
        .await;
        if let Some(result) = looked {
            return BareResolved { result, account };
        }
    }
    if let Some(read) = &account
        && missed_here
        && not_found_across
    {
        let silent = silent_devices(read, text);
        if !silent.is_empty() {
            let names = device_name_of(Some(&**read));
            let devices: Vec<SilentDevice> = silent
                .iter()
                .map(|id| SilentDevice {
                    id: id.clone(),
                    name: names(id).unwrap_or_else(|| id.clone()),
                    reason: None,
                })
                .collect();
            return BareResolved {
                result: Err(tool_refusal(
                    &devices_silent(SilentReason::ListingIncomplete, &devices),
                    None,
                    None,
                )),
                account,
            };
        }
    }
    let result = Err(with_addresses(
        *across_error,
        account.as_deref().and_then(|read| read.here.as_deref()),
        device_name_of(account.as_deref()),
    ));
    BareResolved { result, account }
}
