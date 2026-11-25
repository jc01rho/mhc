//! The account surface the thread address module reads (omo `thread/remote/ports.ts`, `alias-port.ts`; omo #14305,
//! #14318, #14328, #14386).
//!
//! The address module never talks to a transport: it reads this device's account through three ports (the device
//! service client, the account's session listing with its single probe, and the alias registry) that a device
//! service implementation fills in. `RemoteThreadSurface` bundles them the way upstream's type of that name does, and
//! carries the memory of which device last listed a thread id (#14172).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use maho_thread_address_core::{KeyedRow, NamedRow, RowName, row_name};
use serde_json::Value;

use crate::thread::adapter::BoxFuture;
use crate::thread::contracts_results::ThreadStatus;
use crate::thread::contracts_state::{ThreadModel, ThreadState};
use crate::thread::thread_state::thread_state;

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
    /// Session transcript file path reported on close/unload (#14170).
    pub session_file: Option<String>,
    /// When the cached row snapshot was taken (#14043).
    pub as_of: Option<i64>,
    /// Active model reported by the device's listing row.
    pub model: Option<ThreadModel>,
    /// Archived flag carried by the listing row (#14304).
    pub archived: bool,
}

/// One account listing: its rows, and the devices whose rows this listing could not include.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteListing {
    pub rows: Vec<RemoteSessionRow>,
    pub incomplete: Vec<String>,
    /// Per-device round states when the service reports them (#14295); every state other than `answered` is incomplete.
    pub devices: Option<BTreeMap<String, String>>,
}

/// One device on the account as `RemoteSessionsPort::devices` reports it (#14305).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteDevice {
    pub device: String,
    pub label: String,
    pub online: bool,
    pub platform: Option<String>,
    pub kind: Option<String>,
    pub last_seen_ms: Option<i64>,
}

/// Options on a remote `session_command` call (`fresh` bypasses catalog cache, `direct` skips attach pool, `wait_ms`
/// bounds the call wait for a graceful close).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCommandOptions {
    pub fresh: bool,
    pub direct: bool,
    pub wait_ms: Option<i64>,
}

/// Outcome of one engine record sent to another device through `RemoteSessionsPort::session_command`.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteCommandOutcome {
    Ok {
        response: Value,
        local: bool,
        located: Option<String>,
    },
    Refused {
        reason: String,
        detail: Option<String>,
    },
    Uncertain {
        reason: String,
    },
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

impl AliasError {
    pub fn signed_out(&self) -> bool {
        matches!(self.code.as_str(), "sign_in_required" | "signed_out")
    }
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

/// The account's session surface, as far as address resolution, listing and relay controls read it.
pub trait RemoteSessionsPort: Send + Sync {
    /// This device's account id when known.
    fn this_device(&self) -> BoxFuture<Option<String>> {
        Box::pin(async { None })
    }

    /// This device's display name on the account when known (#14305).
    fn this_device_name(&self) -> BoxFuture<Option<String>> {
        Box::pin(async { None })
    }

    /// Every device on this account with its platform, kind and liveness (#14305).
    fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
        Box::pin(async { Err("unsupported".to_owned()) })
    }

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
                devices: None,
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

    /// Sends one session-scoped RPC record to `session_id` on `device`.
    fn session_command(
        &self,
        _device: String,
        _session_id: String,
        _rpc: Value,
        _options: SessionCommandOptions,
    ) -> BoxFuture<RemoteCommandOutcome> {
        Box::pin(async {
            RemoteCommandOutcome::Refused {
                reason: "unsupported".to_owned(),
                detail: None,
            }
        })
    }

    /// Stamps a row as closed in this surface's cache until a newer snapshot arrives (`closed-rows.ts`).
    fn closed(&self, _row: &RemoteSessionRow, _file: Option<&str>, _at: Option<i64>) {}

    /// Stamps a reopened session's new listing key until a newer snapshot arrives (`closed-rows.ts`).
    fn reopened(&self, _row: &RemoteSessionRow, _key: &str) {}
}

/// The account service's alias registry (`alias-port.v1`).
pub trait AliasRegistry: Send + Sync {
    fn resolve(&self, alias: String) -> BoxFuture<Result<AliasHolder, AliasError>>;

    /// Bulk lookup of aliases held by `thread_ids`, for `thread_list` across devices (#14305).
    fn aliases_of(
        &self,
        _thread_ids: Vec<String>,
    ) -> BoxFuture<Result<BTreeMap<String, String>, AliasError>> {
        Box::pin(async { Ok(BTreeMap::new()) })
    }
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

/// The epoch milliseconds a UUIDv7 durable id carries in its first 48 bits (RFC 9562 section 5.7), when `id` is one.
fn uuid_v7_millis(id: &str) -> Option<i64> {
    let groups: Vec<&str> = id.split('-').collect();
    let lengths = [8usize, 4, 4, 4, 12];
    if groups.len() != lengths.len()
        || !groups
            .iter()
            .zip(lengths)
            .all(|(group, length)| group.len() == length)
    {
        return None;
    }
    let version = groups.get(2)?.chars().next()?;
    let variant = groups.get(3)?.chars().next()?;
    if version != '7' || !matches!(variant, '8'..='b') {
        return None;
    }
    let hex = |group: &str| {
        (!group.is_empty() && group.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(group, 16).ok())
            .flatten()
    };
    // `time_high` (32 bits) then `time_mid` (16 bits): the 48-bit timestamp.
    let high = hex(groups.first()?)?;
    let low = hex(groups.get(1)?)?;
    i64::try_from((high << 16) | low).ok()
}

/// A listing row's creation time. The row carries none and its `updated_ms` is not one (#14474), so it is the time a
/// UUIDv7 durable id encodes; a row without such an id keeps `updated_ms`.
pub fn row_created_at(row: &RemoteSessionRow) -> String {
    iso_from_ms(
        row.durable_id
            .as_deref()
            .and_then(uuid_v7_millis)
            .unwrap_or(row.updated_ms),
    )
}

/// Another device's row state (upstream `thread/tools/thread-state.ts` `rowState`): an archived row is `archived`,
/// an ended row or one its device's cached listing still names (the device did not answer, #14043) is `inactive`, else
/// the row's own running or idle.
pub fn row_state(row: &RemoteSessionRow) -> ThreadState {
    thread_state(
        row.archived,
        row.state != "ended" && row.source != Some(RowSource::Cache),
        Some(row.state.as_str()),
    )
}

/// A row's name by the one naming function: its own name, else the folder's name marked `unnamed`, because only a real
/// name is an address.
pub fn row_name_of(row: &RemoteSessionRow) -> RowName {
    row_name(&NamedRow {
        name: Some(row.title.clone()).filter(|title| !title.trim().is_empty()),
        first_message: None,
        folder: row_folder(row).to_owned(),
    })
}

/// Text shown to a person or a model that may carry characters able to change how a line reads (upstream
/// `escape-for-display.ts`, #13304): control characters, the format characters that reorder or hide text, and the line
/// and paragraph separators. Each is shown as `\uXXXX` (`\u{XXXXX}` above the basic plane); nothing is deleted.
pub fn escape_for_display(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if !unsafe_for_display(character) {
            escaped.push(character);
            continue;
        }
        let code = character as u32;
        if code > 0xffff {
            escaped.push_str(&format!("\\u{{{code:x}}}"));
        } else {
            escaped.push_str(&format!("\\u{code:04x}"));
        }
    }
    escaped
}

/// Unicode general categories Cc, Cf, Zl and Zp, which `char` does not expose: `is_control` is Cc, and these are every
/// Cf, Zl and Zp code point.
fn unsafe_for_display(character: char) -> bool {
    character.is_control()
        || matches!(
            character as u32,
            0x00ad
                | 0x0600..=0x0605
                | 0x061c
                | 0x06dd
                | 0x070f
                | 0x0890..=0x0891
                | 0x08e2
                | 0x180e
                | 0x200b..=0x200f
                | 0x2028..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x110bd
                | 0x110cd
                | 0x13430..=0x1343f
                | 0x1bca0..=0x1bca3
                | 0x1d173..=0x1d17a
                | 0xe0001
                | 0xe0020..=0xe007f
        )
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

    #[test]
    fn a_uuid_v7_thread_id_names_the_rows_creation_time_and_any_other_id_keeps_the_rows_own() {
        let mut listed = row("b", "key-v7", "01932bf3-0280-7abc-8def-0123456789ab");
        listed.updated_ms = 1_700_000_000_000;
        assert_eq!(row_created_at(&listed), "2024-11-14T18:32:46.464Z");

        // A v4 id, a v7 version nibble with a bad variant, an ungrouped id and junk are all no UUIDv7.
        for id in [
            "019a8d7c-5e00-4abc-8def-0123456789ab",
            "019a8d7c-5e00-7abc-0def-0123456789ab",
            "019a8d7c5e007abc8def0123456789ab",
            "not-an-id",
        ] {
            let mut other = row("b", "key", id);
            other.updated_ms = 1_700_000_000_000;
            assert_eq!(row_created_at(&other), "2023-11-14T22:13:20.000Z", "{id}");
        }
    }

    #[test]
    fn a_cache_or_an_end_makes_a_row_inactive_and_its_own_activity_decides_otherwise() {
        let mut listed = row("b", "k", "t");
        assert_eq!(row_state(&listed), ThreadState::Idle);
        listed.state = "running".to_owned();
        assert_eq!(row_state(&listed), ThreadState::Running);
        for running in ["queued", "awaiting_approval"] {
            listed.state = running.to_owned();
            assert_eq!(row_state(&listed), ThreadState::Running, "{running}");
        }
        listed.state = "errored".to_owned();
        assert_eq!(row_state(&listed), ThreadState::Idle);
        listed.source = Some(RowSource::Cache);
        assert_eq!(row_state(&listed), ThreadState::Inactive);
        listed.source = None;
        listed.state = "ended".to_owned();
        assert_eq!(row_state(&listed), ThreadState::Inactive);
        listed.state = "idle".to_owned();
        listed.archived = true;
        assert_eq!(row_state(&listed), ThreadState::Archived);
    }

    #[test]
    fn a_rows_name_is_its_title_else_the_folders_name_marked_unnamed() {
        let mut listed = row("b", "k", "t");
        listed.title = "on b".to_owned();
        assert_eq!(
            row_name_of(&listed),
            RowName {
                name: "on b".to_owned(),
                unnamed: false
            }
        );
        listed.title = String::new();
        listed.path = "/srv/runs/legacy-tooling".to_owned();
        listed.project = "renamed-tooling".to_owned();
        assert_eq!(
            row_name_of(&listed),
            RowName {
                name: "legacy-tooling".to_owned(),
                unnamed: true
            }
        );
    }

    #[test]
    fn display_text_escapes_control_format_and_separator_characters_and_keeps_the_rest() {
        assert_eq!(escape_for_display("plain"), "plain");
        assert_eq!(escape_for_display("a\nb"), "a\\u000ab");
        assert_eq!(escape_for_display("a\u{1b}[31m"), "a\\u001b[31m");
        assert_eq!(escape_for_display("a\u{200b}b"), "a\\u200bb");
        assert_eq!(escape_for_display("a\u{202e}b"), "a\\u202eb");
        assert_eq!(escape_for_display("a\u{2028}b"), "a\\u2028b");
        assert_eq!(escape_for_display("a\u{e0041}b"), "a\\u{e0041}b");
        assert_eq!(escape_for_display("한글 그대로"), "한글 그대로");
    }
}
