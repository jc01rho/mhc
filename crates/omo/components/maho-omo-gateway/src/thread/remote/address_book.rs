//! The relay transport's address book (omo `thread/remote/address-book.ts`, decision 0027 section 1): the account
//! listing becomes entries of the same shape the local book yields, so one resolver judges local and remote threads
//! together.

use std::sync::Arc;

use crate::thread::address::{Found, Pin, Reads, ThreadRefusal, pinned};
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

use super::ports::{
    RemoteListing, RemoteSessionRow, RemoteThreadSurface, iso_from_ms, row_folder, row_status,
};

pub fn relay_address_entry(row: &RemoteSessionRow, aliases: &[String]) -> ThreadAddressEntry {
    let thread_id = row.durable_id.clone().unwrap_or_default();
    let session_ids =
        if !thread_id.is_empty() && !row.session_id.is_empty() && row.session_id != thread_id {
            vec![row.session_id.clone()]
        } else {
            Vec::new()
        };
    ThreadAddressEntry {
        thread_id,
        name: row.title.clone(),
        cwd: row_folder(row).to_owned(),
        status: row_status(row),
        updated_at: Some(iso_from_ms(row.updated_ms)),
        session_ids,
        device: Some(row.device.clone()),
        aliases: aliases.to_vec(),
        row: Some(Arc::new(row.clone())),
    }
}

/// The relay's transport reference for an address that names its device (`<device>/<durable id>`), read without the
/// account listing: the listing settles only with the slowest device and lags a session just opened (#12709).
pub fn named_device_row(device: &str, durable_id: &str) -> RemoteSessionRow {
    RemoteSessionRow {
        device: device.to_owned(),
        session_id: durable_id.to_owned(),
        durable_id: Some(durable_id.to_owned()),
        state: "idle".to_owned(),
        ..RemoteSessionRow::default()
    }
}

/// One device's listing with whether it missed the round (#14280); a port without `listing` reports it answered.
pub async fn device_listing(
    remote: &RemoteThreadSurface,
    device: &str,
) -> Result<RemoteListing, String> {
    remote.sessions.listing(Some(device.to_owned())).await
}

/// The session a create just made on another device (`thread_create`, `omo remote open`), addressed by the relay key
/// its open reply named (`session_id`) and only failing that by its durable id (`omo#13748`).
pub fn created_device_row(
    device: &str,
    thread_id: &str,
    session_id: Option<&str>,
    name: &str,
    cwd: &str,
) -> RemoteSessionRow {
    let mut row = named_device_row(device, thread_id);
    if let Some(key) = session_id.filter(|key| !key.is_empty()) {
        row.session_id = key.to_owned();
    }
    row.title = name.to_owned();
    row.project = cwd.to_owned();
    row
}

/// The account-wide listing, updating `remote.last_listed` with the rows answered this round (#14172).
pub async fn account_listing(remote: &RemoteThreadSurface) -> Result<RemoteListing, String> {
    let listing = remote.sessions.listing(None).await?;
    let _ = remote.last_listed.remember(&listing.rows);
    Ok(listing)
}

/// Refusal when a thread was last listed on `device`, whose listing round did not include its threads in full (#14304).
pub fn listing_incomplete(device: &str) -> ThreadToolFailure {
    thread_tool_failure(
        "host_unavailable",
        format!(
            "The thread was last listed on {device}, whose threads this listing round did not include in full."
        ),
        "Retry the call; call thread_list (tool.thread_list() in an eval cell) to see which devices answered.",
        Some(serde_json::json!({ "devices": [device], "reason": "listing_incomplete" })),
    )
}

const REMOTE_NOT_SET_UP: [&str; 4] = [
    "not_configured",
    "disabled",
    "signed_out",
    "sign_in_required",
];

/// Device-service states in which no relay is set up for this account (`STATUS_STATES`, device-service-core vocabulary).
pub fn remote_not_set_up(state: Option<&str>) -> bool {
    state.is_none() || state.is_some_and(|value| REMOTE_NOT_SET_UP.contains(&value))
}

/// Resolves an approved reply to a sender whose delivery arrived from `sender_device` (`#14280`, `#14270`).
pub async fn reply_on_device(
    remote: Option<&RemoteThreadSurface>,
    sender_device: &str,
    sender_session_id: &str,
) -> Result<Found, ThreadRefusal> {
    let mut reads = Reads::new(remote);
    pinned(
        remote,
        &Pin {
            device: sender_device,
            thread: sender_session_id,
        },
        &mut reads.status,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts_results::ThreadStatus;

    #[test]
    fn a_row_with_a_thread_id_is_an_entry_keyed_by_it_and_reachable_by_its_listing_key() {
        let row = RemoteSessionRow {
            device: "b7c41e09d2a64f1e".to_owned(),
            session_id: "rk-live".to_owned(),
            durable_id: Some("0bb00001-0000-4000-8000-000000000001".to_owned()),
            title: "beta".to_owned(),
            project: "/work/b".to_owned(),
            path: "/work/b".to_owned(),
            state: "idle".to_owned(),
            updated_ms: 1_700_000_000_000,
            source: None,
            ..RemoteSessionRow::default()
        };
        let entry = relay_address_entry(&row, &["lane-b".to_owned()]);
        assert_eq!(entry.thread_id, "0bb00001-0000-4000-8000-000000000001");
        assert_eq!(entry.session_ids, vec!["rk-live".to_owned()]);
        assert_eq!(entry.name, "beta");
        assert_eq!(entry.status, ThreadStatus::Live);
        assert_eq!(entry.aliases, vec!["lane-b".to_owned()]);
        assert_eq!(
            entry.updated_at.as_deref(),
            Some("2023-11-14T22:13:20.000Z")
        );
        assert_eq!(entry.device.as_deref(), Some("b7c41e09d2a64f1e"));
        assert_eq!(entry.row.as_deref(), Some(&row));
    }

    #[test]
    fn a_row_that_publishes_no_thread_id_has_an_empty_thread_and_no_listing_key_alias() {
        let row = RemoteSessionRow {
            device: "b".to_owned(),
            session_id: "c2Vzc2lvbi1rZXktMDAwMQ".to_owned(),
            ..RemoteSessionRow::default()
        };
        let entry = relay_address_entry(&row, &[]);
        assert_eq!(entry.thread_id, "");
        assert!(entry.session_ids.is_empty());
    }

    #[test]
    fn a_named_device_row_carries_the_id_as_its_listing_key() {
        let row = named_device_row("b", "thread-1");
        assert_eq!(row.session_id, "thread-1");
        assert_eq!(row.durable_id.as_deref(), Some("thread-1"));
        assert_eq!(row.state, "idle");
    }

    #[test]
    fn listing_incomplete_uses_the_partial_round_wording_and_not_set_up_matches_unset_states() {
        let refused = listing_incomplete("b7c4");
        assert_eq!(refused.code.as_str(), "host_unavailable");
        assert!(
            refused
                .message
                .contains("whose threads this listing round did not include in full"),
            "{}",
            refused.message
        );
        assert_eq!(
            refused.details,
            Some(serde_json::json!({ "devices": ["b7c4"], "reason": "listing_incomplete" }))
        );
        assert!(remote_not_set_up(None));
        assert!(remote_not_set_up(Some("signed_out")));
        assert!(!remote_not_set_up(Some("offline")));
    }
}
