//! The account surface the thread address module reads (omo `thread/remote/ports.ts`, `alias-port.ts`; omo #14305,
//! #14318, #14328, #14386).
//!
//! The address module never talks to a transport: it reads this device's account through three ports (the device
//! service client, the account's session listing with its single probe, and the alias registry) that a device
//! service implementation fills in. `RemoteThreadSurface` bundles them the way upstream's type of that name does, and
//! carries the memory of which device last listed a thread id (#14172).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use maho_thread_address_core::KeyedRow;

use crate::thread::adapter::BoxFuture;
use crate::thread::contracts_results::ThreadStatus;

/// `live`: the device answered this listing round. `cache`: it did not and the relay served its last snapshot (#14043).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSource {
    Live,
    Cache,
}

/// One row of the account-scoped session list (the device service's `sessions.list` listing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteSessionRow {
    pub device: String,
    /// The listing key the device's connector assigned to the session.
    pub session_id: String,
    /// The session's durable id when its device publishes it (#12996).
    pub durable_id: Option<String>,
    pub title: String,
    pub project: String,
    /// The session's place on its device as the device service lists it (its working folder, else `unknown`).
    pub path: String,
    pub state: String,
    pub updated_ms: i64,
    /// Absent (an older service, a row built here): treated as `live`.
    pub source: Option<RowSource>,
}

/// One account listing: its rows, and the devices whose rows this listing could not include.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteListing {
    pub rows: Vec<RemoteSessionRow>,
    pub incomplete: Vec<String>,
}

/// One single-probe look for a durable id on one device (omo#13037 A4.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteLookupOutcome {
    /// The device's stable key for the session it holds.
    Found { key: String },
    /// The device answered and holds no such session.
    NotHeld,
    /// The device did not answer within the bound; `reason` is the service's own.
    Silent { reason: String },
    /// This device's own service cannot make the single probe.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteLocateOutcome {
    Located { session_id: String },
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasHolder {
    pub session_id: String,
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasError {
    pub code: String,
    pub message: String,
}

/// The device service's state (`connected`, `plan_required`, ...) and this device's account id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub state: String,
    pub device_id: Option<String>,
}

/// The device service answered a status read with a failure. `service_not_installed`: there is no device service at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFailure {
    pub message: String,
    pub service_not_installed: bool,
}

/// One row of the device service's device list, this device's own (`is_self`) included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDeviceRow {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub is_self: bool,
    pub last_seen_ms: Option<i64>,
}

/// The one remote client (#13414): the account's state, this device and the device list are read through it.
pub trait RemoteClient: Send + Sync {
    fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>>;
    fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>>;
}

/// The account's session surface, as far as address resolution reads it.
pub trait RemoteSessionsPort: Send + Sync {
    /// The account's session list; with `device`, that one device's rows only (no account-wide round).
    fn sessions(&self, device: Option<String>) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>>;

    /// The account-wide list with the devices it could not include; with `device`, that one device's rows and whether
    /// it missed the round. The default is a port with no `listing`: every device counts as listed.
    fn listing(&self, device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
        let rows = self.sessions(device);
        Box::pin(async move {
            rows.await.map(|rows| RemoteListing {
                rows,
                incomplete: Vec::new(),
            })
        })
    }

    /// One single-probe locate of `engine_id` on `device`: the id the device addresses that session by.
    fn locate(&self, device: String, engine_id: String) -> BoxFuture<RemoteLocateOutcome>;

    /// omo#13037 A4.2/A4.3: one single-probe locate of `durable_id` on `device`. The default makes no lookup.
    fn lookup(&self, _device: String, _durable_id: String) -> BoxFuture<RemoteLookupOutcome> {
        Box::pin(async { RemoteLookupOutcome::Unavailable })
    }

    /// omo#13037: this device's own service can make the single probe (read from its connection; no round trip).
    fn can_lookup(&self) -> BoxFuture<bool> {
        Box::pin(async { false })
    }

    /// Whether `wake` can be sent at all. Absent: false.
    fn can_wake(&self) -> BoxFuture<bool> {
        Box::pin(async { false })
    }
}

/// The account service's alias registry, resolve side (`alias-port.v1`).
pub trait AliasRegistry: Send + Sync {
    fn resolve(&self, alias: String) -> BoxFuture<Result<AliasHolder, AliasError>>;
}

const LAST_LISTED_MAX: usize = 4096;

#[derive(Default)]
struct LastListedState {
    devices: HashMap<String, (String, u64)>,
    order: BTreeMap<u64, String>,
    next: u64,
}

impl LastListedState {
    fn keep(&self, before: &mut HashMap<String, Option<String>>, id: &str) {
        before
            .entry(id.to_owned())
            .or_insert_with(|| self.devices.get(id).map(|(device, _)| device.clone()));
    }

    fn forget(&mut self, id: &str) {
        if let Some((_, stamp)) = self.devices.remove(id) {
            self.order.remove(&stamp);
        }
    }
}

/// The device each thread id (durable id and listing key) was last listed on, per surface, so a thread whose device
/// misses one round is still tied to it (#14172). A name is never remembered: names change. Bounded; the id listed
/// longest ago leaves first.
#[derive(Clone, Default)]
pub struct LastListed {
    state: Arc<Mutex<LastListedState>>,
}

impl LastListed {
    fn lock(&self) -> MutexGuard<'_, LastListedState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn get(&self, id: &str) -> Option<String> {
        self.lock()
            .devices
            .get(id)
            .map(|(device, _)| device.clone())
    }

    /// Updates the memory in place and returns how it read BEFORE this round: only the entries this round overwrote,
    /// added or evicted are kept aside, so a resolution never copies the whole map.
    pub fn remember(&self, rows: &[RemoteSessionRow]) -> EarlierListing {
        let mut state = self.lock();
        let mut before: HashMap<String, Option<String>> = HashMap::new();
        for row in rows {
            let durable = row.durable_id.as_deref().unwrap_or_default();
            let mut ids = vec![durable, row.session_id.as_str()];
            ids.dedup();
            for id in ids {
                if id.is_empty() {
                    continue;
                }
                state.keep(&mut before, id);
                state.forget(id);
                state.next += 1;
                let stamp = state.next;
                state
                    .devices
                    .insert(id.to_owned(), (row.device.clone(), stamp));
                state.order.insert(stamp, id.to_owned());
                if state.devices.len() > LAST_LISTED_MAX {
                    let oldest = state.order.values().next().cloned();
                    if let Some(oldest_id) = oldest {
                        state.keep(&mut before, &oldest_id);
                        state.forget(&oldest_id);
                    }
                }
            }
        }
        EarlierListing {
            before,
            memory: self.clone(),
        }
    }
}

/// What the memory said about an id before one listing round updated it.
#[derive(Clone)]
pub struct EarlierListing {
    before: HashMap<String, Option<String>>,
    memory: LastListed,
}

impl EarlierListing {
    pub fn get(&self, id: &str) -> Option<String> {
        match self.before.get(id) {
            Some(device) => device.clone(),
            None => self.memory.get(id),
        }
    }
}

/// Everything the address module needs to reach past this machine. Absent from a resolution: this device only.
#[derive(Clone)]
pub struct RemoteThreadSurface {
    pub sessions: Arc<dyn RemoteSessionsPort>,
    pub client: Arc<dyn RemoteClient>,
    /// Absent until the account service's alias registry ships: an alias is then never resolved, never guessed.
    pub aliases: Option<Arc<dyn AliasRegistry>>,
    pub last_listed: LastListed,
}

impl RemoteThreadSurface {
    pub fn new(
        sessions: Arc<dyn RemoteSessionsPort>,
        client: Arc<dyn RemoteClient>,
        aliases: Option<Arc<dyn AliasRegistry>>,
    ) -> Self {
        Self {
            sessions,
            client,
            aliases,
            last_listed: LastListed::default(),
        }
    }
}

/// A listing row's thread status. Live only when its device answered this round with a non-ended state: a cached row of
/// a device that did not answer is a snapshot, never proof the session runs (#14043).
pub fn row_status(row: &RemoteSessionRow) -> ThreadStatus {
    if row.state == "ended" || row.source == Some(RowSource::Cache) {
        ThreadStatus::Resumable
    } else {
        ThreadStatus::Live
    }
}

/// A row's working folder: its `path` (the folder the device lists), else the project when that is unknown (#14429).
pub fn row_folder(row: &RemoteSessionRow) -> &str {
    if !row.path.is_empty() && row.path != "unknown" {
        &row.path
    } else {
        &row.project
    }
}

/// What row identity reads of a listing row.
pub fn keyed(row: &RemoteSessionRow) -> KeyedRow {
    KeyedRow {
        device: row.device.clone(),
        durable_id: row.durable_id.clone(),
        session_id: row.session_id.clone(),
    }
}

/// The one rule for a probe-built `ended` row (#13238 review): a probe may build one only while this device's service
/// can carry the wake, since an ended row goes down the wake path.
pub async fn probe_may_build_ended_row(sessions: &dyn RemoteSessionsPort) -> bool {
    sessions.can_wake().await
}

/// A session a device holds on disk but no engine runs (omo#13037 A4.2): built from that device's own probe answer, its
/// stable key and the durable id. It carries no name and no folder, and its state `ended` sends a delivery down the
/// wake path.
pub fn ended_row(device: &str, key: &str, durable_id: &str) -> RemoteSessionRow {
    RemoteSessionRow {
        device: device.to_owned(),
        session_id: key.to_owned(),
        durable_id: Some(durable_id.to_owned()),
        state: "ended".to_owned(),
        ..RemoteSessionRow::default()
    }
}

/// An epoch-millisecond instant as `toISOString()` writes it (`1970-01-01T00:00:00.000Z`).
pub fn iso_from_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let of_day = ms.rem_euclid(86_400_000);
    let hour = of_day / 3_600_000;
    let minute = of_day % 3_600_000 / 60_000;
    let second = of_day % 60_000 / 1000;
    let milli = of_day % 1000;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(device: &str, key: &str, durable: &str) -> RemoteSessionRow {
        RemoteSessionRow {
            device: device.to_owned(),
            session_id: key.to_owned(),
            durable_id: (!durable.is_empty()).then(|| durable.to_owned()),
            ..RemoteSessionRow::default()
        }
    }

    #[test]
    fn iso_from_ms_writes_to_iso_string_form() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(iso_from_ms(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }

    #[test]
    fn a_cached_or_ended_row_is_never_live() {
        let mut listed = row("b", "k", "t");
        assert_eq!(row_status(&listed), ThreadStatus::Live);
        listed.source = Some(RowSource::Cache);
        assert_eq!(row_status(&listed), ThreadStatus::Resumable);
        listed.source = None;
        listed.state = "ended".to_owned();
        assert_eq!(row_status(&listed), ThreadStatus::Resumable);
    }

    #[test]
    fn a_folder_that_is_unknown_falls_back_to_the_project() {
        let mut listed = row("b", "k", "t");
        listed.path = "unknown".to_owned();
        listed.project = "app".to_owned();
        assert_eq!(row_folder(&listed), "app");
        listed.path = "/work/app".to_owned();
        assert_eq!(row_folder(&listed), "/work/app");
    }

    #[test]
    fn the_memory_reports_what_it_said_before_the_round_and_evicts_the_oldest() {
        let memory = LastListed::default();
        let first = memory.remember(&[row("b", "key-1", "thread-1")]);
        assert_eq!(first.get("thread-1"), None);
        assert_eq!(first.get("key-1"), None);
        let second = memory.remember(&[row("c", "key-1", "thread-1")]);
        assert_eq!(second.get("thread-1"), Some("b".to_owned()));
        assert_eq!(second.get("never-listed"), None);
        let third = memory.remember(&[]);
        assert_eq!(third.get("thread-1"), Some("c".to_owned()));
    }

    #[test]
    fn the_memory_is_bounded() {
        let memory = LastListed::default();
        let rows: Vec<RemoteSessionRow> = (0..=LAST_LISTED_MAX)
            .map(|index| row("b", &format!("key-{index}"), ""))
            .collect();
        let _ = memory.remember(&rows);
        assert_eq!(memory.get("key-0"), None);
        assert_eq!(
            memory.get(&format!("key-{LAST_LISTED_MAX}")),
            Some("b".to_owned())
        );
    }
}
