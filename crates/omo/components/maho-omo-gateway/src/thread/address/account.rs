//! The account as the thread tools read it (omo `thread/address/account.ts`, #14305, #14318): this device, its roster,
//! the listing rows of devices still on the account and how each round listed each device, read at most once per
//! resolution.

use std::sync::Arc;

use maho_thread_address_core::{DeviceListing, DeviceRow, ListingRound, device_listing};
use serde_json::Value;

use crate::thread::remote::address_book::device_listing as one_device_listing;
use crate::thread::remote::ports::{
    EarlierListing, RemoteClient, RemoteSessionRow, RemoteThreadSurface,
};
use crate::thread::remote::remote_ops::{Ready, remote_ready};

use super::device::{AccountDevice, AccountDevices, device_rows};
use super::refusals::ThreadRefusal;

const LOCAL_ONLY: &str = "local";

/// This device's account id (`None` with no device service id) and the account's devices, from ONE `devices.list` round.
/// A client with no device service lists only its local self row: no id, no other device.
pub async fn account_devices(client: &dyn RemoteClient) -> Result<AccountDevices, String> {
    let listed = client.devices_list().await?;
    let here = listed
        .iter()
        .find(|row| row.is_self)
        .map(|row| row.id.clone())
        .filter(|id| id != LOCAL_ONLY);
    let devices = listed
        .iter()
        .filter(|row| row.id != LOCAL_ONLY)
        .map(|row| AccountDevice {
            device: row.id.clone(),
            label: row.name.clone(),
            online: row.online,
            last_seen_ms: None,
        })
        .collect();
    Ok(AccountDevices {
        here,
        devices,
        listing_of: None,
    })
}

/// The roster `read_account` keeps: the device list plus when each device was last seen (a `device_offline` refusal
/// names it).
async fn roster_of(client: &dyn RemoteClient) -> Result<Vec<AccountDevice>, String> {
    let listed = client.devices_list().await?;
    Ok(listed
        .into_iter()
        .filter(|row| row.id != LOCAL_ONLY)
        .map(|row| AccountDevice {
            device: row.id,
            label: row.name,
            online: row.online,
            last_seen_ms: row.last_seen_ms,
        })
        .collect())
}

/// The listing rows whose device is still on the account: in its roster, or this device (the roster may leave this
/// device out). A removed device's cached rows go (#13505); an offline one keeps its rows. With no roster nothing can be
/// judged, so every row stays.
fn on_account(
    rows: &[RemoteSessionRow],
    roster: Option<&[AccountDevice]>,
    here: Option<&str>,
) -> Vec<RemoteSessionRow> {
    match roster {
        None => rows.to_vec(),
        Some(roster) => rows
            .iter()
            .filter(|row| {
                Some(row.device.as_str()) == here
                    || roster.iter().any(|device| device.device == row.device)
            })
            .cloned()
            .collect(),
    }
}

/// The account in one read: this device, its roster, the listing rows of devices still on it, each device's listing.
#[derive(Clone)]
pub struct Account {
    pub here: Option<String>,
    pub devices: Vec<AccountDevice>,
    /// The roster as read; `None` when it could not be read (then no row is judged by it).
    pub roster: Option<Vec<AccountDevice>>,
    pub rows: Vec<RemoteSessionRow>,
    pub incomplete: Vec<String>,
    pub round: ListingRound,
    online: Vec<DeviceRow>,
    earlier: Option<EarlierListing>,
}

impl Account {
    pub fn listing_of(&self, device: &str) -> DeviceListing {
        device_listing(device, &self.round, &self.online)
    }

    /// The device an earlier round listed `id` on (a thread id or listing key), when that device missed this round.
    pub fn missed_holder(&self, id: &str) -> Option<String> {
        let device = self.earlier.as_ref()?.get(id)?;
        let on_here = Some(device.as_str()) == self.here.as_deref();
        let missed = self.incomplete.contains(&device);
        (!on_here && missed).then_some(device)
    }
}

/// The account from a roster alone, no listing read: what a resolution on this device's own book needs.
pub fn roster_account(here: Option<String>, roster: Option<Vec<AccountDevice>>) -> Account {
    let online = device_rows(roster.as_deref().unwrap_or(&[]));
    Account {
        here,
        devices: roster.clone().unwrap_or_default(),
        roster,
        rows: Vec::new(),
        incomplete: Vec::new(),
        round: ListingRound::default(),
        online,
        earlier: None,
    }
}

/// Device-service states in which no relay is set up for this account: a local address miss stays a local miss.
const NOT_SET_UP: [&str; 4] = [
    "not_configured",
    "disabled",
    "signed_out",
    "sign_in_required",
];

fn not_set_up(state: Option<&str>) -> bool {
    match state {
        None => true,
        Some(state) => NOT_SET_UP.contains(&state),
    }
}

pub enum AccountRead {
    Account(Arc<Account>),
    /// No relay is set up here: every address resolves as on a device with no account.
    None,
    /// A configured relay that is failing, or a plan without one: answered before not_found.
    Unavailable(ThreadRefusal),
}

/// An account read, or the message of the error it threw (a listing that could not be reached).
#[derive(Clone)]
pub enum AccountAttempt {
    Account(Arc<Account>),
    None,
    Unavailable(ThreadRefusal),
    Failed(String),
}

impl From<AccountRead> for AccountAttempt {
    fn from(read: AccountRead) -> Self {
        match read {
            AccountRead::Account(account) => Self::Account(account),
            AccountRead::None => Self::None,
            AccountRead::Unavailable(refusal) => Self::Unavailable(refusal),
        }
    }
}

fn not_ready(refusal: ThreadRefusal) -> AccountRead {
    let state = refusal
        .details
        .as_ref()
        .and_then(|details| details.get("state"))
        .and_then(Value::as_str);
    let unset = refusal.code.as_str() == "host_unavailable" && not_set_up(state);
    if unset {
        AccountRead::None
    } else {
        AccountRead::Unavailable(refusal)
    }
}

async fn roster_read(remote: &RemoteThreadSurface) -> Option<Vec<AccountDevice>> {
    roster_of(remote.client.as_ref()).await.ok()
}

/// The status round of one resolution, read once whichever step asks first.
pub struct StatusRound<'a> {
    remote: Option<&'a RemoteThreadSurface>,
    read: Option<Result<Ready, ThreadRefusal>>,
}

impl StatusRound<'_> {
    pub async fn ready(&mut self) -> Result<Ready, ThreadRefusal> {
        if let Some(read) = &self.read {
            return read.clone();
        }
        let read = match self.remote {
            Some(remote) => remote_ready(remote).await,
            None => Err(super::refusals::refusal(
                "host_unavailable",
                "This device has no connection to the account's other devices.",
                "Retry from a session whose device service is set up.",
                None,
            )),
        };
        self.read = Some(read.clone());
        read
    }
}

/// What a resolution already read and hands in, so it reads each thing once.
#[derive(Default)]
pub struct AccountReads {
    /// The resolution's one status round.
    pub ready: Option<Result<Ready, ThreadRefusal>>,
    /// The roster as already read (`Some(None)`: it could not be read).
    pub roster: Option<Option<Vec<AccountDevice>>>,
    /// Read only this device's listing (#12709): a device-qualified address never waits on the whole round.
    pub device: Option<String>,
}

/// The status round and the roster, no listing: enough to resolve a device part (#12709).
pub async fn read_roster(
    remote: &RemoteThreadSurface,
    ready: Result<Ready, ThreadRefusal>,
) -> AccountRead {
    match ready {
        Err(refusal) => not_ready(refusal),
        Ok(read) => {
            let roster = roster_read(remote).await;
            AccountRead::Account(Arc::new(roster_account(read.here, roster)))
        }
    }
}

/// ONE status read, ONE roster read and ONE listing round (design section 1): this device's id, the account's devices,
/// the listing rows of devices still on it, and how this round listed each device. A roster that cannot be read judges
/// no row (every row stays). `given` hands in what the resolution already read, and `device` narrows the listing to
/// that one device. The error is the message of a listing that could not be reached.
pub async fn read_account(
    remote: &RemoteThreadSurface,
    given: AccountReads,
) -> Result<AccountRead, String> {
    let AccountReads {
        ready,
        roster,
        device,
    } = given;
    let ready = match ready {
        Some(ready) => ready,
        None => remote_ready(remote).await,
    };
    let ready = match ready {
        Ok(ready) => ready,
        Err(refusal) => return Ok(not_ready(refusal)),
    };
    let listing_future = async {
        match device.as_deref() {
            Some(device) => one_device_listing(remote, device).await,
            None => remote.sessions.listing(None).await,
        }
    };
    let roster_future = async {
        match roster {
            Some(roster) => roster,
            None => roster_read(remote).await,
        }
    };
    let (listing, roster) = futures::future::join(listing_future, roster_future).await;
    let listing = listing?;
    let earlier = remote.last_listed.remember(&listing.rows);
    let here = ready.here;
    let rows = on_account(&listing.rows, roster.as_deref(), here.as_deref());
    let online = device_rows(roster.as_deref().unwrap_or(&[]));
    Ok(AccountRead::Account(Arc::new(Account {
        here,
        devices: roster.clone().unwrap_or_default(),
        roster,
        rows,
        incomplete: listing.incomplete.clone(),
        round: ListingRound {
            incomplete: listing.incomplete,
            states: listing.devices,
        },
        online,
        earlier: Some(earlier),
    })))
}

/// The reads of one resolution: the status round, and the account at most once whichever step asks first.
pub struct Reads<'a> {
    remote: Option<&'a RemoteThreadSurface>,
    pub status: StatusRound<'a>,
    attempt: Option<AccountAttempt>,
}

impl<'a> Reads<'a> {
    pub fn new(remote: Option<&'a RemoteThreadSurface>) -> Self {
        Self {
            remote,
            status: StatusRound { remote, read: None },
            attempt: None,
        }
    }

    pub async fn account(&mut self) -> AccountAttempt {
        if let Some(attempt) = &self.attempt {
            return attempt.clone();
        }
        let attempt = match self.remote {
            None => AccountAttempt::None,
            Some(remote) => {
                let ready = self.status.ready().await;
                let given = AccountReads {
                    ready: Some(ready),
                    ..AccountReads::default()
                };
                match read_account(remote, given).await {
                    Ok(read) => AccountAttempt::from(read),
                    Err(message) => AccountAttempt::Failed(message),
                }
            }
        };
        self.attempt = Some(attempt.clone());
        attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, online: bool) -> AccountDevice {
        AccountDevice {
            device: id.to_owned(),
            label: id.to_owned(),
            online,
            last_seen_ms: None,
        }
    }

    fn row(device: &str, key: &str) -> RemoteSessionRow {
        RemoteSessionRow {
            device: device.to_owned(),
            session_id: key.to_owned(),
            ..RemoteSessionRow::default()
        }
    }

    #[test]
    fn a_removed_devices_rows_go_and_an_offline_ones_stay() {
        let rows = vec![row("here", "a"), row("b", "b"), row("gone", "c")];
        let roster = vec![device("b", false)];
        let kept = on_account(&rows, Some(roster.as_slice()), Some("here"));
        let keys: Vec<&str> = kept.iter().map(|row| row.session_id.as_str()).collect();
        assert_eq!(keys, vec!["a", "b"]);
        assert_eq!(on_account(&rows, None, Some("here")).len(), 3);
    }

    #[test]
    fn only_an_unconfigured_relay_is_not_set_up() {
        assert!(not_set_up(None));
        assert!(not_set_up(Some("signed_out")));
        assert!(!not_set_up(Some("offline")));
        assert!(!not_set_up(Some("connecting")));
    }

    #[test]
    fn a_roster_account_lists_an_offline_device_offline_and_the_rest_listed() {
        let account = roster_account(
            Some("here".to_owned()),
            Some(vec![device("b", true), device("c", false)]),
        );
        assert_eq!(account.listing_of("b"), DeviceListing::Listed);
        assert_eq!(account.listing_of("c"), DeviceListing::Offline);
        assert_eq!(account.missed_holder("anything"), None);
    }
}
