//! Cross-device `thread_list` merging, device listing state and alias enrichment (`thread/remote/cross-device.ts`;
//! omo #14305, #14304, #14308, #14295).

use std::collections::{BTreeSet, HashMap};

use maho_thread_address_core::{DeviceListing, MergeRow, merge_rows, row_key};
use serde_json::Value;

use crate::thread::address::{AccountDevice, AccountRead, AccountReads, read_account};
use crate::thread::contracts_results::{ThreadDevice, ThreadListOk};
use crate::thread::contracts_state::ThreadState;
use crate::thread::control::relay_session_host::displayed_thread;
use crate::thread::thread_state::listed_states;

use super::address_book::remote_not_set_up;
use super::ports::{
    RemoteDevice, RemoteSessionRow, RemoteThreadSurface, escape_for_display, iso_from_ms, keyed,
    row_state,
};
use super::remote_ops::remote_ready;

#[derive(Clone)]
struct MergeEntry {
    key: String,
    device: String,
    row: Option<RemoteSessionRow>,
}

impl MergeRow for MergeEntry {
    fn key(&self) -> &str {
        &self.key
    }

    fn device(&self) -> &str {
        &self.device
    }
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

async fn with_aliases(
    remote: &RemoteThreadSurface,
    mut listed: ThreadListOk,
    ids: &[String],
    aliased: &[String],
) -> ThreadListOk {
    let Some(registry) = &remote.aliases else {
        return listed;
    };
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    for id in ids.iter().chain(aliased.iter()) {
        if !id.is_empty() && seen.insert(id.clone()) {
            all.push(id.clone());
        }
    }
    if all.is_empty() {
        return listed;
    }
    if listed
        .remote_unavailable
        .as_deref()
        .is_some_and(|reason| reason != "devices_unavailable")
    {
        listed.aliases_unavailable = Some("not_asked".to_owned());
        return listed;
    }
    match registry.aliases_of(all).await {
        Ok(map) => {
            for thread in &mut listed.threads {
                if let Some(alias) = map.get(&thread.thread) {
                    thread.alias = Some(escape_for_display(alias));
                }
            }
            listed
        }
        Err(error) => {
            listed.aliases_unavailable = Some(if error.signed_out() {
                "sign_in".to_owned()
            } else {
                "host_unavailable".to_owned()
            });
            listed
        }
    }
}

/// Merges this device's local `thread_list` with the account's other devices, reporting `devices[]`,
/// `devices[].listing` (`listed`/`incomplete`/`offline`) and `devices[].dropped` (#14305, #14304, #14308, #14295).
pub async fn list_across(
    remote: Option<&RemoteThreadSurface>,
    mut local: ThreadListOk,
    all_scope: bool,
    states: Option<&[ThreadState]>,
) -> ThreadListOk {
    let Some(remote) = remote else {
        return local;
    };
    let ids: Vec<String> = local.threads.iter().map(|t| t.thread.clone()).collect();
    let ready = match remote_ready(remote).await {
        Ok(ready) => ready,
        Err(refusal) => {
            let state = refusal
                .details
                .as_ref()
                .and_then(|details| details.get("state"))
                .and_then(Value::as_str);
            if !all_scope && refusal.code.as_str() == "host_unavailable" && remote_not_set_up(state)
            {
                return local;
            }
            local.remote_unavailable = Some(refusal.code.as_str().to_owned());
            return with_aliases(remote, local, &ids, &[]).await;
        }
    };

    let (here_name, roster_res) = futures::future::join(
        remote.sessions.this_device_name(),
        remote.sessions.devices(),
    )
    .await;
    let passed_roster = roster_res.as_ref().ok().map(|devices| {
        Some(
            devices
                .iter()
                .map(|device| AccountDevice {
                    device: device.device.clone(),
                    label: device.label.clone(),
                    online: device.online,
                    last_seen_ms: device.last_seen_ms,
                })
                .collect::<Vec<_>>(),
        )
    });
    let account_read = read_account(
        remote,
        AccountReads {
            ready: Some(Ok(ready)),
            roster: passed_roster,
            device: None,
        },
    )
    .await;
    let account = match account_read {
        Ok(AccountRead::Account(account)) if account.here.is_some() => account,
        _ => {
            local.remote_unavailable = Some("host_unavailable".to_owned());
            return with_aliases(remote, local, &ids, &[]).await;
        }
    };
    let here = account.here.clone().unwrap_or_default();

    if let Some(name) = here_name.as_deref().filter(|name| !name.is_empty()) {
        let escaped = escape_for_display(name);
        for thread in &mut local.threads {
            if thread.machine.is_none() {
                thread.machine = Some(escaped.clone());
            }
        }
    }

    let mut machines: HashMap<String, String> = HashMap::new();
    match &roster_res {
        Ok(devices) => {
            for device in devices {
                if !device.label.is_empty() {
                    machines.insert(device.device.clone(), device.label.clone());
                }
            }
        }
        Err(_) => {
            for device in &account.devices {
                if !device.label.is_empty() {
                    machines.insert(device.device.clone(), device.label.clone());
                }
            }
        }
    }

    let local_items: Vec<MergeEntry> = local
        .threads
        .iter()
        .map(|thread| MergeEntry {
            key: thread.thread.clone(),
            device: here.clone(),
            row: None,
        })
        .collect();
    let remote_items: Vec<MergeEntry> = account
        .rows
        .iter()
        .map(|row| MergeEntry {
            key: row_key(&keyed(row)),
            device: row.device.clone(),
            row: Some(row.clone()),
        })
        .collect();
    let merged = merge_rows(&local_items, &remote_items, Some(&here));
    let shown = listed_states(states);
    let mut aliased = Vec::new();
    for entry in merged.into_iter().skip(local_items.len()) {
        let Some(row) = entry.row else {
            continue;
        };
        if !shown.contains(&row_state(&row)) {
            continue;
        }
        if let Some(durable_id) = row.durable_id.as_deref().filter(|id| !id.is_empty()) {
            aliased.push(durable_id.to_owned());
        }
        let machine = machines.get(&row.device).map(String::as_str);
        local.threads.push(displayed_thread(&row, machine));
    }

    match roster_res {
        Ok(mut roster) => {
            if !roster.iter().any(|device| device.device == here) {
                roster.insert(
                    0,
                    RemoteDevice {
                        device: here.clone(),
                        label: here_name.clone().unwrap_or_else(|| here.clone()),
                        online: true,
                        platform: Some(current_platform().to_owned()),
                        kind: None,
                        last_seen_ms: None,
                    },
                );
            }
            let devices = roster
                .into_iter()
                .map(|device| {
                    let this_device = device.device == here;
                    let listing = if this_device {
                        if local.listing.as_deref() == Some("incomplete") {
                            DeviceListing::Incomplete
                        } else {
                            DeviceListing::Listed
                        }
                    } else {
                        account.listing_of(&device.device)
                    };
                    let threads = local
                        .threads
                        .iter()
                        .filter(|thread| {
                            thread.device.as_deref().unwrap_or(here.as_str()) == device.device
                        })
                        .count();
                    ThreadDevice {
                        device: device.device,
                        name: escape_for_display(&device.label),
                        platform: device.platform,
                        kind: device.kind,
                        online: this_device || device.online,
                        last_seen: device.last_seen_ms.map(iso_from_ms),
                        this_device,
                        listing,
                        threads,
                        dropped: if this_device {
                            local.inactive_dropped
                        } else {
                            None
                        },
                    }
                })
                .collect();
            local.devices = Some(devices);
        }
        Err(_) => {
            local.remote_unavailable = Some("devices_unavailable".to_owned());
        }
    }

    with_aliases(remote, local, &ids, &aliased).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use futures::executor::block_on;

    use super::*;
    use crate::thread::adapter::BoxFuture;
    use crate::thread::contracts_results::{ThreadResultSummary, ThreadStatus};
    use crate::thread::remote::ports::{
        AliasError, AliasHolder, AliasRegistry, ClientDeviceRow, RemoteClient, RemoteFailure,
        RemoteListing, RemoteLocateOutcome, RemoteSessionsPort, RowSource, ServiceStatus,
    };

    struct TestSurface {
        status: Result<ServiceStatus, RemoteFailure>,
        devices: Result<Vec<RemoteDevice>, String>,
        listing: Result<RemoteListing, String>,
        here_name: Option<String>,
        aliases: Option<Result<BTreeMap<String, String>, AliasError>>,
    }

    impl RemoteClient for TestSurface {
        fn devices_list(&self) -> BoxFuture<Result<Vec<ClientDeviceRow>, String>> {
            let res = self.devices.as_ref().map(|devs| {
                devs.iter()
                    .map(|d| ClientDeviceRow {
                        id: d.device.clone(),
                        name: d.label.clone(),
                        online: d.online,
                        is_self: d.device == "here",
                        last_seen_ms: d.last_seen_ms,
                    })
                    .collect()
            });
            let owned = res.map_err(Clone::clone);
            Box::pin(async move { owned })
        }

        fn service_status(&self) -> BoxFuture<Result<ServiceStatus, RemoteFailure>> {
            let status = self.status.clone();
            Box::pin(async move { status })
        }
    }

    impl RemoteSessionsPort for TestSurface {
        fn this_device_name(&self) -> BoxFuture<Option<String>> {
            let name = self.here_name.clone();
            Box::pin(async move { name })
        }

        fn devices(&self) -> BoxFuture<Result<Vec<RemoteDevice>, String>> {
            let devices = self.devices.clone();
            Box::pin(async move { devices })
        }

        fn sessions(
            &self,
            _device: Option<String>,
        ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
            let rows = self
                .listing
                .as_ref()
                .map(|l| l.rows.clone())
                .map_err(Clone::clone);
            Box::pin(async move { rows })
        }

        fn listing(&self, _device: Option<String>) -> BoxFuture<Result<RemoteListing, String>> {
            let listing = self.listing.clone();
            Box::pin(async move { listing })
        }

        fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
            Box::pin(async {
                RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                }
            })
        }
    }

    struct TestAliases(Mutex<Result<BTreeMap<String, String>, AliasError>>);

    impl AliasRegistry for TestAliases {
        fn resolve(&self, _alias: String) -> BoxFuture<Result<AliasHolder, AliasError>> {
            Box::pin(async {
                Err(AliasError {
                    code: "not_found".to_owned(),
                    message: "missing".to_owned(),
                })
            })
        }

        fn aliases_of(
            &self,
            _thread_ids: Vec<String>,
        ) -> BoxFuture<Result<BTreeMap<String, String>, AliasError>> {
            let res = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Box::pin(async move { res })
        }
    }

    fn local_summary(id: &str, name: &str) -> ThreadResultSummary {
        ThreadResultSummary {
            thread: id.to_owned(),
            name: name.to_owned(),
            unnamed: None,
            status: ThreadStatus::Live,
            state: ThreadState::Idle,
            model: None,
            activity: None,
            error_code: None,
            turn_id: None,
            waiting_on: None,
            engine_version: None,
            backlog: None,
            as_of: None,
            machine: None,
            cwd: "/work/app".to_owned(),
            created_at: "2026-11-26T00:00:00.000Z".to_owned(),
            updated_at: Some("2026-11-26T00:00:00.000Z".to_owned()),
            link: format!("[{name}](omo-thread://v1/{id})"),
            device: None,
            alias: None,
            surface: None,
            controls: None,
            error_note: None,
            skills: None,
        }
    }

    fn base_local(
        threads: Vec<ThreadResultSummary>,
        incomplete: bool,
        dropped: Option<i64>,
    ) -> ThreadListOk {
        ThreadListOk {
            threads,
            scope: "all".to_owned(),
            inactive_truncated: dropped.map(|_| true),
            inactive_dropped: dropped,
            listing: incomplete.then(|| "incomplete".to_owned()),
            unanswered_endpoints: incomplete.then_some(1),
            backlog_error: None,
            devices: None,
            remote_unavailable: None,
            aliases_unavailable: None,
        }
    }

    fn remote_row(device: &str, key: &str, durable: Option<&str>, state: &str) -> RemoteSessionRow {
        RemoteSessionRow {
            device: device.to_owned(),
            session_id: key.to_owned(),
            durable_id: durable.map(str::to_owned),
            title: format!("on-{device}"),
            project: "/work/remote".to_owned(),
            path: "/work/remote".to_owned(),
            state: state.to_owned(),
            updated_ms: 1_700_000_000_000,
            ..RemoteSessionRow::default()
        }
    }

    fn build_surface(spec: TestSurface) -> RemoteThreadSurface {
        let aliases = spec
            .aliases
            .as_ref()
            .map(|res| Arc::new(TestAliases(Mutex::new(res.clone()))) as Arc<dyn AliasRegistry>);
        let arc = Arc::new(spec);
        RemoteThreadSurface::new(arc.clone(), arc, aliases)
    }

    #[test]
    fn list_across_merges_rows_reports_device_listing_states_dropped_and_aliases() {
        let mut device_states = BTreeMap::new();
        device_states.insert("b".to_owned(), "answered".to_owned());
        device_states.insert("c".to_owned(), "unanswered".to_owned());
        let mut alias_map = BTreeMap::new();
        alias_map.insert("dur-local".to_owned(), "main-lane".to_owned());
        alias_map.insert("dur-b".to_owned(), "remote-lane".to_owned());

        let surface = build_surface(TestSurface {
            status: Ok(ServiceStatus {
                state: "connected".to_owned(),
                device_id: Some("here".to_owned()),
            }),
            here_name: Some("this-box".to_owned()),
            devices: Ok(vec![
                RemoteDevice {
                    device: "b".to_owned(),
                    label: "Laptop".to_owned(),
                    online: true,
                    platform: Some("darwin".to_owned()),
                    kind: Some("laptop".to_owned()),
                    last_seen_ms: Some(1_700_000_000_000),
                },
                RemoteDevice {
                    device: "c".to_owned(),
                    label: "Box C".to_owned(),
                    online: true,
                    platform: Some("linux".to_owned()),
                    kind: None,
                    last_seen_ms: None,
                },
                RemoteDevice {
                    device: "d".to_owned(),
                    label: "Offline D".to_owned(),
                    online: false,
                    platform: None,
                    kind: None,
                    last_seen_ms: Some(1_699_000_000_000),
                },
            ]),
            listing: Ok(RemoteListing {
                rows: vec![
                    remote_row("here", "rk-dup", Some("dur-here-only"), "idle"),
                    remote_row("b", "rk-shadow", Some("dur-local"), "idle"),
                    remote_row("b", "rk-b", Some("dur-b"), "idle"),
                    remote_row("b", "rk-anon", None, "running"),
                    RemoteSessionRow {
                        source: Some(RowSource::Cache),
                        ..remote_row("c", "rk-cached", Some("dur-cached"), "running")
                    },
                    RemoteSessionRow {
                        archived: true,
                        ..remote_row("c", "rk-arch", Some("dur-archived"), "idle")
                    },
                ],
                incomplete: Vec::new(),
                devices: Some(device_states),
            }),
            aliases: Some(Ok(alias_map)),
        });

        let local = base_local(vec![local_summary("dur-local", "Local")], true, Some(3));
        let listed = block_on(list_across(Some(&surface), local.clone(), true, None));

        let ids: Vec<&str> = listed.threads.iter().map(|t| t.thread.as_str()).collect();
        assert_eq!(ids, vec!["dur-local", "dur-b", "b/rk-anon", "dur-cached"]);
        assert_eq!(listed.threads[0].machine.as_deref(), Some("this-box"));
        assert_eq!(listed.threads[0].alias.as_deref(), Some("main-lane"));
        assert_eq!(listed.threads[1].alias.as_deref(), Some("remote-lane"));
        assert_eq!(listed.threads[1].machine.as_deref(), Some("Laptop"));
        assert_eq!(listed.threads[3].state, ThreadState::Inactive);

        let devices = listed.devices.expect("devices");
        assert_eq!(devices.len(), 4);
        assert_eq!(devices[0].device, "here");
        assert!(devices[0].this_device);
        assert_eq!(devices[0].listing, DeviceListing::Incomplete);
        assert_eq!(devices[0].dropped, Some(3));
        assert_eq!(devices[0].threads, 1);

        assert_eq!(devices[1].device, "b");
        assert_eq!(devices[1].listing, DeviceListing::Listed);
        assert_eq!(devices[1].threads, 2);
        assert_eq!(devices[1].dropped, None);

        assert_eq!(devices[2].device, "c");
        assert_eq!(devices[2].listing, DeviceListing::Incomplete);
        assert_eq!(devices[2].threads, 1);

        assert_eq!(devices[3].device, "d");
        assert_eq!(devices[3].listing, DeviceListing::Offline);

        let live_only = block_on(list_across(
            Some(&surface),
            local,
            true,
            Some(&[ThreadState::Running, ThreadState::Idle]),
        ));
        let live_ids: Vec<&str> = live_only
            .threads
            .iter()
            .map(|t| t.thread.as_str())
            .collect();
        assert_eq!(live_ids, vec!["dur-local", "dur-b", "b/rk-anon"]);
    }

    #[test]
    fn list_across_reports_unset_not_configured_quietly_unless_all_scope_and_reports_alias_errors()
    {
        let unset_surface = build_surface(TestSurface {
            status: Ok(ServiceStatus {
                state: "not_configured".to_owned(),
                device_id: None,
            }),
            here_name: None,
            devices: Ok(Vec::new()),
            listing: Ok(RemoteListing::default()),
            aliases: Some(Ok(BTreeMap::new())),
        });
        let local = base_local(vec![local_summary("dur-1", "One")], false, None);
        let quiet = block_on(list_across(
            Some(&unset_surface),
            local.clone(),
            false,
            None,
        ));
        assert_eq!(quiet.remote_unavailable, None);
        assert_eq!(quiet.aliases_unavailable, None);

        let loud = block_on(list_across(Some(&unset_surface), local.clone(), true, None));
        assert_eq!(loud.remote_unavailable.as_deref(), Some("host_unavailable"));
        assert_eq!(loud.aliases_unavailable.as_deref(), Some("not_asked"));

        let devices_err_surface = build_surface(TestSurface {
            status: Ok(ServiceStatus {
                state: "connected".to_owned(),
                device_id: Some("here".to_owned()),
            }),
            here_name: None,
            devices: Err("devices failed".to_owned()),
            listing: Ok(RemoteListing::default()),
            aliases: Some(Err(AliasError {
                code: "sign_in_required".to_owned(),
                message: "sign in".to_owned(),
            })),
        });
        let partial = block_on(list_across(Some(&devices_err_surface), local, true, None));
        assert_eq!(
            partial.remote_unavailable.as_deref(),
            Some("devices_unavailable")
        );
        assert!(partial.devices.is_none());
        assert_eq!(partial.aliases_unavailable.as_deref(), Some("sign_in"));
    }
}
