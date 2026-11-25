//! The relay transport's address book (omo `thread/remote/address-book.ts`, decision 0027 section 1): the account
//! listing becomes entries of the same shape the local book yields, so one resolver judges local and remote threads
//! together.

use crate::thread::addressing::ThreadAddressEntry;

use super::ports::{
    RemoteListing, RemoteSessionRow, RemoteThreadSurface, iso_from_ms, row_folder, row_status,
};
use std::sync::Arc;

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

/// One device's listing with whether it missed the round (#14280).
pub async fn device_listing(
    remote: &RemoteThreadSurface,
    device: &str,
) -> Result<RemoteListing, String> {
    remote.sessions.listing(Some(device.to_owned())).await
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
}
