//! Two devices of one account for the address module (omo `thread/address/testing/two-devices.ts`, #14305): this
//! device (a hex id of its own, so the `here` keyword and this device's id are told apart) and device B, each with a
//! live thread, an ended thread, a thread whose routing key differs from its durable id, one thread name both share and
//! one thread id both hold (a stale copy here, live on B). A third device is offline.

use std::sync::{Arc, Mutex, MutexGuard};

use futures::executor::block_on;

use crate::thread::adapter::BoxFuture;
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadStatus;
use crate::thread::remote::ports::{
    AliasError, AliasHolder, AliasRegistry, ClientDeviceRow, RemoteClient, RemoteFailure,
    RemoteListing, RemoteLocateOutcome, RemoteLookupOutcome, RemoteSessionRow, RemoteSessionsPort,
    RemoteThreadSurface, ServiceStatus,
};

use super::refusals::ThreadRefusal;
use super::resolve::resolve_address;
use super::types::{ResolveContext, ResolveOptions, Resolved};

pub const HERE_ID: &str = "a11ce0de5e1f0123";
pub const HERE_NAME: &str = "this mac";
pub const B_ID: &str = "b7c41e09d2a64f1e";
pub const B_NAME: &str = "Studio Laptop";
pub const B_TWIN_ID: &str = "b7c4ffff00000000";
pub const OFF_ID: &str = "c0ffee5a1d2b3c4d";
pub const OFF_NAME: &str = "attic";
pub const OFF_SEEN: i64 = 1_699_000_000_000;
pub const FIX_ID: &str = "f1c5d00e77a8b9c0";
pub const FIX_NAME: &str = "fix";
pub const QUIET_ID: &str = "c1a55e0000000001";
pub const QUIET_NAME: &str = "closet";

pub const HERE_LIVE: &str = "0aa00001-0000-4000-8000-000000000001";
pub const HERE_ENDED: &str = "0aa00002-0000-4000-8000-000000000002";
pub const HERE_SHARED: &str = "0aa00003-0000-4000-8000-000000000003";
pub const HERE_SLASH: &str = "0aa00004-0000-4000-8000-000000000004";
pub const B_LIVE: &str = "0bb00001-0000-4000-8000-000000000001";
pub const B_ENDED: &str = "0bb00002-0000-4000-8000-000000000002";
pub const B_SHARED: &str = "0bb00003-0000-4000-8000-000000000003";
pub const B_PROBE: &str = "0bb00004-0000-4000-8000-000000000004";
pub const NOWHERE: &str = "0ee00001-0000-4000-8000-000000000001";
pub const STALE: &str = "0dd00001-0000-4000-8000-000000000001";
pub const FIX_LOGIN: &str = "0ff00001-0000-4000-8000-000000000001";

pub const KEY_B_LIVE: &str = "rk-b-live";
pub const KEY_B_ENDED: &str = "rk-b-ended";
pub const KEY_B_SHARED: &str = "rk-b-shared";
pub const KEY_B_PROBE: &str = "rk-b-probe";
pub const KEY_STALE: &str = "rk-stale-on-b";
pub const KEY_FIX_LOGIN: &str = "rk-fix-login";

pub const ROUTE_HERE_LIVE: &str = "route-here-live";
pub const ROUTE_HERE_SHARED: &str = "route-here-shared";
pub const ROUTE_HERE_SLASH: &str = "route-here-slash";

pub const ALIAS_B: &str = "lane-b";
pub const ALIAS_HERE: &str = "lane-here";

pub const WORKSPACE: &str = "/work/a";

#[derive(Clone, Default)]
pub struct Setup {
    /// B misses every listing round: the listing names it incomplete and carries none of its rows.
    pub incomplete: bool,
    /// B answers the first round and misses every later one (#14172).
    pub drops_after_first_round: bool,
    pub b_offline: bool,
    /// A second online device named like B.
    pub twin: bool,
    /// A third online device that misses every round B misses.
    pub quiet_third: bool,
    /// A device named `fix` holding a thread named `login`.
    pub fix: bool,
    /// This device's service makes the single probe and may build an ended row (both omo#13037 gates open).
    pub probe: bool,
    /// Devices that do not answer the single probe.
    pub silent: Vec<String>,
    /// The device service state (default `connected`).
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub unfiltered: usize,
    pub filtered: usize,
    pub statuses: usize,
    pub rosters: usize,
    pub lookups: usize,
}

struct State {
    setup: Setup,
    rows: Vec<RemoteSessionRow>,
    devices: Vec<ClientDeviceRow>,
    aliases: Vec<(String, AliasHolder)>,
    ended: Vec<(String, String, String)>,
    round: usize,
    counters: Counters,
}

#[derive(Clone)]
pub struct Fake {
    state: Arc<Mutex<State>>,
}

impl Fake {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn row(durable: &str, key: &str, title: &str, state: &str, device: &str) -> RemoteSessionRow {
    RemoteSessionRow {
        device: device.to_owned(),
        session_id: key.to_owned(),
        durable_id: Some(durable.to_owned()),
        title: title.to_owned(),
        project: "/work/b".to_owned(),
        path: "/work/b".to_owned(),
        state: state.to_owned(),
        updated_ms: 1_700_000_000_000,
        ..RemoteSessionRow::default()
    }
}

pub fn device(id: &str, name: &str, online: bool) -> ClientDeviceRow {
    ClientDeviceRow {
        id: id.to_owned(),
        name: name.to_owned(),
        online,
        is_self: false,
        last_seen_ms: None,
    }
}

fn account_rows(setup: &Setup) -> Vec<RemoteSessionRow> {
    let mut rows = vec![
        row(B_LIVE, KEY_B_LIVE, "beta", "idle", B_ID),
        row(B_ENDED, KEY_B_ENDED, "beta old", "ended", B_ID),
        row(B_SHARED, KEY_B_SHARED, "shared", "idle", B_ID),
        row(STALE, KEY_STALE, "stale live", "idle", B_ID),
    ];
    if setup.fix {
        rows.push(row(FIX_LOGIN, KEY_FIX_LOGIN, "login", "idle", FIX_ID));
    }
    rows
}

fn account_devices(setup: &Setup) -> Vec<ClientDeviceRow> {
    let mut devices = vec![
        ClientDeviceRow {
            is_self: true,
            ..device(HERE_ID, HERE_NAME, true)
        },
        device(B_ID, B_NAME, !setup.b_offline),
        ClientDeviceRow {
            last_seen_ms: Some(OFF_SEEN),
            ..device(OFF_ID, OFF_NAME, false)
        },
    ];
    if setup.twin {
        devices.push(device(B_TWIN_ID, B_NAME, true));
    }
    if setup.fix {
        devices.push(device(FIX_ID, FIX_NAME, true));
    }
    if setup.quiet_third {
        devices.push(device(QUIET_ID, QUIET_NAME, true));
    }
    devices
}

fn live(id: &str, route: &str, name: &str) -> ThreadAddressEntry {
    ThreadAddressEntry {
        thread_id: id.to_owned(),
        name: name.to_owned(),
        cwd: WORKSPACE.to_owned(),
        status: ThreadStatus::Live,
        updated_at: None,
        session_ids: vec![route.to_owned()],
        device: None,
        aliases: Vec::new(),
        row: None,
    }
}

fn stored(id: &str) -> ThreadAddressEntry {
    ThreadAddressEntry {
        thread_id: id.to_owned(),
        name: String::new(),
        cwd: WORKSPACE.to_owned(),
        status: ThreadStatus::Resumable,
        updated_at: None,
        session_ids: Vec::new(),
        device: None,
        aliases: Vec::new(),
        row: None,
    }
}

/// This device's address book: live sessions and the session files no engine runs (an ended thread and the stale copy
/// of B's live thread).
pub fn here_entries() -> Vec<ThreadAddressEntry> {
    vec![
        live(HERE_LIVE, ROUTE_HERE_LIVE, "alpha"),
        live(HERE_SHARED, ROUTE_HERE_SHARED, "shared"),
        live(HERE_SLASH, ROUTE_HERE_SLASH, "fix/login"),
        stored(HERE_ENDED),
        stored(STALE),
    ]
}

impl RemoteClient for Fake {
    fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
        let devices = {
            let mut state = self.state();
            state.counters.rosters += 1;
            state.devices.clone()
        };
        Box::pin(async move { Ok(devices) })
    }

    fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
        let status = {
            let mut state = self.state();
            state.counters.statuses += 1;
            ServiceStatus {
                state: state
                    .setup
                    .plan
                    .clone()
                    .unwrap_or_else(|| "connected".to_owned()),
                device_id: Some(HERE_ID.to_owned()),
            }
        };
        Box::pin(async move { Ok(status) })
    }
}

fn rows_of(state: &State, device: &Option<String>) -> Vec<RemoteSessionRow> {
    state
        .rows
        .iter()
        .filter(|row| device.as_ref().is_none_or(|wanted| *wanted == row.device))
        .cloned()
        .collect()
}

impl RemoteSessionsPort for Fake {
    fn sessions(&self, device: Option<String>) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
        let rows = {
            let mut state = self.state();
            if device.is_none() {
                state.counters.unfiltered += 1;
            } else {
                state.counters.filtered += 1;
            }
            rows_of(&state, &device)
        };
        Box::pin(async move { Ok(rows) })
    }

    fn listing(&self, device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
        let listing = {
            let mut state = self.state();
            if device.is_none() {
                state.counters.unfiltered += 1;
            } else {
                state.counters.filtered += 1;
            }
            let tracks_rounds = state.setup.incomplete || state.setup.drops_after_first_round;
            if tracks_rounds {
                state.round += 1;
            }
            let missed =
                state.setup.incomplete || (state.setup.drops_after_first_round && state.round > 1);
            let rows = rows_of(&state, &device);
            if missed {
                let mut incomplete = vec![B_ID.to_owned()];
                if state.setup.quiet_third {
                    incomplete.push(QUIET_ID.to_owned());
                }
                RemoteListing {
                    rows: rows.into_iter().filter(|row| row.device != B_ID).collect(),
                    incomplete,
                    devices: None,
                }
            } else {
                RemoteListing {
                    rows,
                    incomplete: Vec::new(),
                    devices: None,
                }
            }
        };
        Box::pin(async move { Ok(listing) })
    }

    fn locate(&self, device: String, engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
        let key = {
            let state = self.state();
            state
                .rows
                .iter()
                .find(|row| {
                    row.device == device && row.durable_id.as_deref() == Some(engine_id.as_str())
                })
                .map(|row| row.session_id.clone())
        };
        Box::pin(async move {
            match key {
                Some(session_id) => RemoteLocateOutcome::Located { session_id },
                None => RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                },
            }
        })
    }

    fn lookup(&self, device: String, durable_id: String) -> BoxFuture<RemoteLookupOutcome> {
        let outcome = {
            let mut state = self.state();
            state.counters.lookups += 1;
            let held = state
                .ended
                .iter()
                .find(|(holder, thread, _)| *holder == device && *thread == durable_id)
                .map(|(_, _, key)| key.clone());
            if state.setup.silent.contains(&device) {
                RemoteLookupOutcome::Silent {
                    reason: "host_not_answering".to_owned(),
                }
            } else if let Some(key) = held {
                RemoteLookupOutcome::Found { key }
            } else {
                RemoteLookupOutcome::NotHeld
            }
        };
        Box::pin(async move { outcome })
    }

    fn can_lookup(&self) -> BoxFuture<bool> {
        let probe = self.state().setup.probe;
        Box::pin(async move { probe })
    }

    fn can_wake(&self) -> BoxFuture<bool> {
        let probe = self.state().setup.probe;
        Box::pin(async move { probe })
    }
}

impl AliasRegistry for Fake {
    fn resolve(&self, alias: String) -> BoxFuture<Result<AliasHolder, AliasError>> {
        let held = self
            .state()
            .aliases
            .iter()
            .find(|(name, _)| *name == alias)
            .map(|(_, holder)| holder.clone());
        Box::pin(async move {
            held.ok_or_else(|| AliasError {
                code: "alias_not_found".to_owned(),
                message: format!("No alias {alias}."),
            })
        })
    }
}

pub struct TwoDevices {
    pub fake: Fake,
    pub surface: RemoteThreadSurface,
    pub entries: Vec<ThreadAddressEntry>,
}

impl TwoDevices {
    pub fn new(setup: Setup) -> Self {
        let rows = account_rows(&setup);
        let devices = account_devices(&setup);
        Self::from_parts(setup, rows, devices, here_entries())
    }

    pub fn from_parts(
        setup: Setup,
        rows: Vec<RemoteSessionRow>,
        devices: Vec<ClientDeviceRow>,
        entries: Vec<ThreadAddressEntry>,
    ) -> Self {
        let ended = if setup.probe {
            vec![(B_ID.to_owned(), B_PROBE.to_owned(), KEY_B_PROBE.to_owned())]
        } else {
            Vec::new()
        };
        let aliases = vec![
            (
                ALIAS_B.to_owned(),
                AliasHolder {
                    session_id: B_LIVE.to_owned(),
                    device: B_ID.to_owned(),
                },
            ),
            (
                ALIAS_HERE.to_owned(),
                AliasHolder {
                    session_id: HERE_LIVE.to_owned(),
                    device: HERE_ID.to_owned(),
                },
            ),
        ];
        let fake = Fake {
            state: Arc::new(Mutex::new(State {
                setup,
                rows,
                devices,
                aliases,
                ended,
                round: 0,
                counters: Counters::default(),
            })),
        };
        let surface = RemoteThreadSurface::new(
            Arc::new(fake.clone()),
            Arc::new(fake.clone()),
            Some(Arc::new(fake.clone())),
        );
        Self {
            fake,
            surface,
            entries,
        }
    }

    pub fn counters(&self) -> Counters {
        self.fake.state().counters
    }

    pub fn resolve_as(
        &self,
        text: &str,
        opts: &ResolveOptions<'_>,
        caller_id: &str,
        root: &str,
    ) -> Result<Resolved, ThreadRefusal> {
        let ctx = ResolveContext {
            remote: Some(&self.surface),
            caller_workspace_root: root,
            local: &self.entries,
            local_for: None,
            caller_id,
        };
        block_on(resolve_address(&ctx, text, opts))
    }

    pub fn resolve(
        &self,
        text: &str,
        opts: &ResolveOptions<'_>,
    ) -> Result<Resolved, ThreadRefusal> {
        self.resolve_as(text, opts, HERE_LIVE, WORKSPACE)
    }
}
