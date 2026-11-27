//! Port of `remote-cli.ts`, `remote-local.ts` and `remote-session-verbs.ts`: `mhc remote <verb>`.
//!
//! Every verb but `bootstrap` and `daemon` is local-only (omo#13440): its whole path is this device's
//! local API, so the CLI entry marks the process before it runs one (`is_local_only_verb`).
//! Divergence: `send` returns once the session took the message (omo's `--no-wait`); it does not
//! stream the answer, because mhc-relayd carries no attach stream yet.

use device_service_core::local_api::client::WakeSetting;
use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{CommandOutcome, LocalApiClient, LocalApiError};
use maho_thread_address_core::{
    AddressRefusal, IdPrefixStep, KeyedRow, LadderMatch, LadderView, RefusalCode,
    ResolveDeviceOptions, SilentDevice, SilentReason, devices_silent, match_thread, resolve_device,
    row_address,
};
use serde_json::{Value, json};

use crate::args::{Delivery, Parsed, STDIN_TEXT, SessionArgs, Target, WakeValue, parse};
use crate::context::RemoteContext;
use crate::device::{
    candidate_lines, device_filter_target, enrich_listing_row, exit_code_of, non_empty,
    refusal_details_json, refusal_message, roster, round_names, row_display_name, row_identity_key,
    text as value_text, typed_device, unanswered_devices, unknown_device,
};
use crate::exit::{RemoteExit, UsageError};
use crate::render::{
    control_lines, device_lines, device_name, escape_for_display, person_next_action,
    session_lines, status_lines, wake_lines,
};
use crate::usage::{is_help_flag, is_verb, remote_usage, verb_usage};

/// The verbs whose whole call path is the local API: they open no network connection themselves.
pub const LOCAL_ONLY_VERBS: [&str; 10] = [
    "status",
    "devices",
    "sessions",
    "disable",
    "control",
    "wake",
    "send",
    "model",
    "interrupt",
    "close",
];

pub fn is_local_only_verb(verb: &str) -> bool {
    LOCAL_ONLY_VERBS.contains(&verb)
}

pub const NOT_RUNNING: &str = "The device service is not running on this device. Start it with `mhc remote daemon`, or set it up with `mhc remote bootstrap`.";

/// How a close refused because the session is running a turn says so (`/remote` offers "Close
/// anyway" on it).
pub const TURN_ACTIVE: &str = "is running a turn";

/// Runs `mhc remote <args>`; the process exit code.
pub async fn run_remote(ctx: &RemoteContext, args: &[String]) -> i32 {
    let Some(first) = args.first().map(String::as_str) else {
        ctx.err(&remote_usage());
        return RemoteExit::Usage.code();
    };
    if first == "help" || is_help_flag(first) {
        ctx.out(&remote_usage());
        return RemoteExit::Ok.code();
    }
    let options_end = args
        .iter()
        .skip(1)
        .position(|arg| arg == "--")
        .map_or(args.len(), |index| index + 1);
    if is_verb(first) && args[1..options_end].iter().any(|arg| is_help_flag(arg)) {
        ctx.out(&verb_usage(first));
        return RemoteExit::Ok.code();
    }
    match first {
        "daemon" => return crate::daemon::run_daemon(ctx).await,
        "bootstrap" => {
            return crate::daemon::run_bootstrap(ctx, args.iter().any(|arg| arg == "--json")).await;
        }
        "attach" => {
            ctx.err("mhc remote attach: the device service here carries no attach stream yet; use mhc remote send, interrupt and close.");
            return RemoteExit::Unavailable.code();
        }
        _ => {}
    }
    let parsed = match parse(args) {
        Ok(parsed) if is_verb(&parsed.command) => parsed,
        Ok(parsed) => {
            ctx.err(&escape_for_display(&format!(
                "mhc remote: unknown command {}",
                parsed.command
            )));
            ctx.err(&remote_usage());
            return RemoteExit::Usage.code();
        }
        Err(UsageError(message)) => {
            if !message.is_empty() {
                ctx.err(&escape_for_display(&message));
            }
            ctx.err(&remote_usage());
            return RemoteExit::Usage.code();
        }
    };
    if parsed.command == "open" {
        ctx.err("mhc remote open: opening a session on another device is not served by the device service here yet.");
        return RemoteExit::Unavailable.code();
    }
    let client = match ctx.connect().await {
        Ok(client) => client,
        Err(error) => return report_connect_failure(ctx, &error),
    };
    let code = run_on(ctx, &client, &parsed).await;
    client.close().await;
    code
}

pub fn report_connect_failure(ctx: &RemoteContext, error: &LocalApiError) -> i32 {
    if is_service_not_running(error) || error.is("handshake_timeout") || error.is("closed") {
        ctx.err(NOT_RUNNING);
        return RemoteExit::Unavailable.code();
    }
    report_failure(ctx, error, false)
}

/// A local API failure, for a person on stderr and as is on stdout under `--json`.
pub fn report_failure(ctx: &RemoteContext, error: &LocalApiError, json: bool) -> i32 {
    if json {
        ctx.out(
            &json!({
                "kind": "error",
                "code": error.code,
                "message": error.reason,
                "next_action": error.action,
            })
            .to_string(),
        );
    }
    ctx.err(&escape_for_display(&format!(
        "mhc remote: {}",
        error.reason
    )));
    if let Some(action) = &error.action {
        ctx.err(&escape_for_display(&person_next_action(action)));
    }
    match error.code.as_str() {
        "unsupported" | "service_disabled" | "signed_out" | "host_not_answering" => {
            RemoteExit::Unavailable.code()
        }
        _ => RemoteExit::Failed.code(),
    }
}

fn show(
    ctx: &RemoteContext,
    json: bool,
    value: &Value,
    lines: impl FnOnce() -> Vec<String>,
) -> i32 {
    if json {
        ctx.out(&value.to_string());
    } else {
        for line in lines() {
            ctx.out(&line);
        }
    }
    RemoteExit::Ok.code()
}

async fn run_on(ctx: &RemoteContext, client: &LocalApiClient, parsed: &Parsed) -> i32 {
    let json = parsed.json;
    let now = ctx.now();
    let answer = match parsed.command.as_str() {
        "status" => client.status().await.map(|status| {
            let local = local_unit_rows(ctx);
            let mut value = status.clone();
            value["service_version"] = Value::String(client.service_version().to_owned());
            show(ctx, json, &value, || {
                status_lines(&status, client.service_version(), &local)
            })
        }),
        "devices" => client.devices().await.map(|devices| {
            show(ctx, json, &Value::Array(devices.clone()), || {
                device_lines(&devices, now)
            })
        }),
        "sessions" => return sessions(ctx, client, parsed).await,
        "enable" | "disable" => {
            client
                .set_enabled(parsed.command == "enable")
                .await
                .map(|enabled| {
                    show(ctx, json, &json!({ "enabled": enabled }), || {
                        vec![format!(
                            "Remote sessions are {} for this device.",
                            if enabled { "on" } else { "off" }
                        )]
                    })
                })
        }
        "control" => client
            .control_attach(parsed.control)
            .await
            .map(|state| show(ctx, json, &state, || control_lines(&state, now))),
        "wake" => {
            let setting = match parsed.wake {
                None => WakeSetting::Read,
                Some(WakeValue::On) => WakeSetting::Set(Some(true)),
                Some(WakeValue::Off) => WakeSetting::Set(Some(false)),
                Some(WakeValue::Default) => WakeSetting::Set(None),
            };
            client
                .session_wake(setting)
                .await
                .map(|state| show(ctx, json, &state, || wake_lines(&state)))
        }
        _ => match &parsed.session {
            Some(session) => return session_verb(ctx, client, session, json).await,
            None => {
                ctx.err(&escape_for_display(&format!(
                    "mhc remote: unknown command {}",
                    parsed.command
                )));
                return RemoteExit::Usage.code();
            }
        },
    };
    answer.unwrap_or_else(|error| report_failure(ctx, &error, json))
}

fn local_unit_rows(ctx: &RemoteContext) -> Vec<(String, String)> {
    use device_service_core::cold_wake::{ManagedUnit, ServicePlatform, linux, unit_path};
    let Some(host) = &ctx.cold_wake else {
        return Vec::new();
    };
    let describe = |unit: ManagedUnit, name: &str| -> String {
        if !unit_path(host, unit).exists() {
            return format!("not installed ({name})");
        }
        match host.platform {
            ServicePlatform::Linux => match linux::unit_state(host, unit) {
                Some((_, active)) if active == "active" || active == "activating" => {
                    format!("running ({name})")
                }
                Some((file, _)) if file == "enabled" => format!("enabled, not running ({name})"),
                Some(_) => format!("stopped ({name})"),
                None => format!("unknown ({name})"),
            },
            ServicePlatform::Darwin => format!("installed ({name})"),
        }
    };
    let (session_host, device_service) = match host.platform {
        ServicePlatform::Linux => (
            device_service_core::cold_wake::SESSION_HOST_SYSTEMD_UNIT,
            device_service_core::cold_wake::DEVICE_SERVICE_SYSTEMD_UNIT,
        ),
        ServicePlatform::Darwin => (
            device_service_core::cold_wake::SESSION_HOST_LAUNCHD_LABEL,
            device_service_core::cold_wake::DEVICE_SERVICE_LAUNCHD_LABEL,
        ),
    };
    vec![
        (
            "Session host".into(),
            describe(ManagedUnit::SessionHost, session_host),
        ),
        (
            "Service unit".into(),
            describe(ManagedUnit::DeviceService, device_service),
        ),
        (
            "Agent dir".into(),
            ctx.agent_dir.to_string_lossy().into_owned(),
        ),
    ]
}

fn report_address_refusal(
    ctx: &RemoteContext,
    verb: &str,
    refusal: &AddressRefusal,
    typed_miss: Option<&str>,
    json: bool,
) -> i32 {
    let message = match (&refusal.details, refusal.code) {
        (
            maho_thread_address_core::RefusalDetails::Silent {
                reason: SilentReason::ListingIncomplete,
                devices,
                ..
            },
            _,
        ) => format!(
            "no session matched on answered devices; {} did not answer this round",
            devices.join(", ")
        ),
        _ => refusal_message(refusal),
    };
    let details = refusal_details_json(refusal, typed_miss);
    if json {
        ctx.out(
            &json!({
                "kind": "error",
                "code": refusal.code.as_str(),
                "message": message,
                "details": details,
            })
            .to_string(),
        );
    }
    ctx.err(&escape_for_display(&format!(
        "mhc remote {verb}: {message}"
    )));
    for line in candidate_lines(refusal.code, &refusal.details) {
        ctx.err(&line);
    }
    exit_code_of(refusal.code)
}

async fn sessions(ctx: &RemoteContext, client: &LocalApiClient, parsed: &Parsed) -> i32 {
    let json = parsed.json;
    let devices = match client.devices().await {
        Ok(devices) => devices,
        Err(error) => return report_failure(ctx, &error, json),
    };
    let mut filter = serde_json::Map::new();
    let filter_device = if let Some(query) = &parsed.filter.device {
        match device_filter_target(&devices, query) {
            Ok(id) => {
                filter.insert("device".into(), Value::String(id.clone()));
                Some(id)
            }
            Err(refusal) => return report_address_refusal(ctx, "sessions", &refusal, None, json),
        }
    } else {
        None
    };
    if let Some(project) = &parsed.filter.project {
        filter.insert("project".into(), Value::String(project.clone()));
    }
    if let Some(state) = &parsed.filter.state {
        filter.insert("state".into(), Value::String(state.clone()));
    }
    let filter = (!filter.is_empty()).then_some(Value::Object(filter));
    let listing = match client.sessions_list(filter, Some(3)).await {
        Ok(listing) => listing,
        Err(error) => return report_failure(ctx, &error, json),
    };
    if let (Some(query), Some(device_id)) = (&parsed.filter.device, &filter_device)
        && let Some(refusal) = unknown_device(
            device_id,
            &devices,
            &listing.rows,
            &listing.incomplete,
            &listing.devices,
        )
    {
        if json {
            let details = refusal_details_json(&refusal, Some(query));
            ctx.out(
                &json!({
                    "kind": "error",
                    "code": "not_found",
                    "message": format!("No device {query} on this account."),
                    "details": details,
                })
                .to_string(),
            );
        }
        ctx.err(&escape_for_display(&format!(
            "mhc remote sessions: no device {query} on this account (see mhc remote devices)"
        )));
        return RemoteExit::Usage.code();
    }
    let unanswered = unanswered_devices(&devices, &listing.incomplete, &listing.devices);
    let rows: Vec<Value> = listing.rows.iter().map(enrich_listing_row).collect();
    let value = json!({
        "rows": rows,
        "devices": listing.devices,
        "incomplete": listing.incomplete,
        "complete": listing.complete,
    });
    show(ctx, json, &value, || {
        session_lines(&rows, &devices, &unanswered, ctx.now())
    })
}

struct Resolved {
    device: String,
    label: String,
    session: String,
}

enum TargetFailure {
    Address {
        refusal: Box<AddressRefusal>,
        typed_miss: Option<String>,
    },
    AmbiguousSession {
        message: String,
        candidates: Vec<Value>,
    },
    Plain {
        code: i32,
        error_code: &'static str,
        message: String,
    },
}

fn match_session_rows<'a>(rows: &'a [Value], query: &str) -> Vec<&'a Value> {
    // Build one LadderView per listing row (upstream resolve.ts viewOf), then run thread-address-core's match_thread:
    // exact id > listing key / row identity key > alias > name > unique 8+ hex id prefix.
    // mhc's remote client has no account alias registry (grep "alias" in maho-omo-remote -> none), so no row carries
    // aliases; the alias step therefore never answers here.
    // The views borrow the ids, keys and names owned below, which live to the end of this function.
    let mut ids: Vec<String> = Vec::with_capacity(rows.len());
    let mut keys_storage: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    let mut names: Vec<String> = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(non_empty(row, "durable_id").unwrap_or_default().to_owned());
        let listing_key = value_text(row, "session_id");
        let identity_key = row_identity_key(row);
        let mut keys: Vec<String> = Vec::new();
        if !listing_key.is_empty() {
            keys.push(listing_key.to_owned());
        }
        if !identity_key.is_empty() && identity_key != listing_key {
            keys.push(identity_key);
        }
        keys_storage.push(keys);
        let unnamed = row.get("unnamed").and_then(Value::as_bool) == Some(true)
            || non_empty(row, "title").is_none();
        names.push(if unnamed {
            String::new()
        } else {
            non_empty(row, "title").unwrap_or_default().to_owned()
        });
    }
    let views: Vec<LadderView<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, _)| LadderView {
            id: &ids[index],
            keys: &keys_storage[index],
            aliases: &[],
            name: &names[index],
        })
        .collect();
    let hit = match_thread(&views, query, |_| true, IdPrefixStep::On);
    match hit {
        LadderMatch::One { index, .. } => vec![&rows[index]],
        LadderMatch::Many { indices, .. } => indices.iter().map(|&i| &rows[i]).collect(),
        LadderMatch::NoMatch => Vec::new(),
    }
}

async fn resolve_target(
    client: &LocalApiClient,
    target: &Target,
) -> Result<Resolved, TargetFailure> {
    let devices = client
        .devices()
        .await
        .map_err(|error| TargetFailure::Plain {
            code: RemoteExit::Failed.code(),
            error_code: "host_unavailable",
            message: error.reason,
        })?;
    if let Some(query) = &target.device {
        return match typed_device(&devices, query) {
            Ok(device) => Ok(Resolved {
                label: format!("{}/{}", device_name(&devices, &device.id), target.session),
                device: device.id,
                session: target.session.clone(),
            }),
            Err(refusal) if refusal.reason == "no_device" => {
                let listed = client
                    .sessions_list(Some(json!({ "device": query })), Some(3))
                    .await
                    .ok();
                if let Some(listing) = listed
                    && round_names(query, &listing.rows, &listing.incomplete, &listing.devices)
                {
                    let session = listing
                        .rows
                        .iter()
                        .find(|row| {
                            value_text(row, "device") == query
                                && (value_text(row, "session_id") == target.session
                                    || value_text(row, "durable_id") == target.session)
                        })
                        .and_then(|row| row["session_id"].as_str())
                        .unwrap_or(&target.session)
                        .to_owned();
                    return Ok(Resolved {
                        label: format!("{query}/{session}"),
                        device: query.clone(),
                        session,
                    });
                }
                Err(TargetFailure::Address {
                    refusal,
                    typed_miss: Some(query.clone()),
                })
            }
            Err(refusal) => Err(TargetFailure::Address {
                refusal,
                typed_miss: None,
            }),
        };
    }
    let listing =
        client
            .sessions_list(None, Some(3))
            .await
            .map_err(|error| TargetFailure::Plain {
                code: RemoteExit::Failed.code(),
                error_code: "host_unavailable",
                message: error.reason,
            })?;
    let matches = match_session_rows(&listing.rows, &target.session);
    match matches.as_slice() {
        [row] => {
            let device = value_text(row, "device").to_owned();
            let session = value_text(row, "session_id").to_owned();
            if let maho_thread_address_core::DeviceResolution::Refusal(refusal) = resolve_device(
                &roster(&devices),
                &device,
                ResolveDeviceOptions {
                    online: true,
                    exact_id: true,
                },
            ) && refusal.code == RefusalCode::DeviceOffline
            {
                return Err(TargetFailure::Address {
                    refusal: Box::new(refusal),
                    typed_miss: None,
                });
            }
            Ok(Resolved {
                label: format!("{}/{session}", device_name(&devices, &device)),
                device,
                session,
            })
        }
        [] => {
            let unanswered = unanswered_devices(&devices, &listing.incomplete, &listing.devices);
            if !unanswered.is_empty() {
                let silent: Vec<SilentDevice> = unanswered
                    .into_iter()
                    .map(|id| {
                        let name = device_name(&devices, &id).to_owned();
                        SilentDevice {
                            id,
                            name,
                            reason: None,
                        }
                    })
                    .collect();
                return Err(TargetFailure::Address {
                    refusal: Box::new(devices_silent(SilentReason::ListingIncomplete, &silent)),
                    typed_miss: None,
                });
            }
            Err(TargetFailure::Plain {
                code: RemoteExit::Failed.code(),
                error_code: "not_found",
                message: format!(
                    "no session {} on this account (see mhc remote sessions)",
                    target.session
                ),
            })
        }
        many => {
            let candidates: Vec<Value> = many
                .iter()
                .map(|row| {
                    let dev_id = value_text(row, "device").to_owned();
                    let session_id = value_text(row, "session_id").to_owned();
                    let durable_id = non_empty(row, "durable_id").map(str::to_owned);
                    let display = row_display_name(row);
                    let thread = row_address(&KeyedRow {
                        device: dev_id.clone(),
                        durable_id,
                        session_id,
                    });
                    let mut entry = json!({
                        "thread": thread,
                        "name": display.name,
                        "device": device_name(&devices, &dev_id),
                    });
                    if display.unnamed
                        && let Some(map) = entry.as_object_mut()
                    {
                        map.insert("unnamed".into(), Value::Bool(true));
                    }
                    entry
                })
                .collect();
            Err(TargetFailure::AmbiguousSession {
                message: format!(
                    "ambiguous_target: {} names more than one session; name it as <device>/<session> ({} found)",
                    target.session,
                    many.len()
                ),
                candidates,
            })
        }
    }
}

fn record_of(session: &SessionArgs) -> Option<(&'static str, Value)> {
    match session {
        SessionArgs::Send { text, delivery, .. } => {
            let method = match delivery {
                Delivery::Auto => "prompt",
                Delivery::Steer => "steer",
                Delivery::FollowUp => "follow_up",
            };
            Some(("send", json!({ "type": method, "message": text })))
        }
        SessionArgs::Model { model, .. } => {
            let (provider, id) = model.split_once('/')?;
            Some((
                "model",
                json!({ "type": "set_model", "provider": provider, "modelId": id }),
            ))
        }
        SessionArgs::Interrupt { .. } => Some(("interrupt", json!({ "type": "abort" }))),
        SessionArgs::Close { .. } => Some(("close", json!({ "type": "close_session" }))),
        SessionArgs::Open { .. } => None,
    }
}

async fn session_verb(
    ctx: &RemoteContext,
    client: &LocalApiClient,
    session: &SessionArgs,
    json: bool,
) -> i32 {
    let target = match session {
        SessionArgs::Send { target, .. }
        | SessionArgs::Model { target, .. }
        | SessionArgs::Interrupt { target }
        | SessionArgs::Close { target, .. } => target,
        SessionArgs::Open { .. } => return RemoteExit::Unavailable.code(),
    };
    let Some((verb, record)) = record_of(session) else {
        return RemoteExit::Usage.code();
    };
    let resolved = match resolve_target(client, target).await {
        Ok(resolved) => resolved,
        Err(TargetFailure::Address {
            refusal,
            typed_miss,
        }) => {
            return report_address_refusal(ctx, verb, &refusal, typed_miss.as_deref(), json);
        }
        Err(TargetFailure::AmbiguousSession {
            message,
            candidates,
        }) => {
            if json {
                ctx.out(
                    &json!({
                        "kind": "error",
                        "code": "ambiguous_target",
                        "message": message,
                        "details": { "candidates": candidates },
                    })
                    .to_string(),
                );
            }
            ctx.err(&escape_for_display(&format!(
                "mhc remote {verb}: {message}"
            )));
            for candidate in &candidates {
                if let Some(thread) = candidate.get("thread").and_then(Value::as_str) {
                    let name = candidate.get("name").and_then(Value::as_str).unwrap_or("");
                    let line = if name.is_empty() {
                        format!("  {thread}")
                    } else {
                        format!("  {thread}  {name}")
                    };
                    ctx.err(&escape_for_display(&line));
                }
            }
            return RemoteExit::Failed.code();
        }
        Err(TargetFailure::Plain {
            code,
            error_code,
            message,
        }) => {
            if json {
                ctx.out(
                    &json!({ "kind": "error", "code": error_code, "message": message }).to_string(),
                );
            }
            ctx.err(&escape_for_display(&format!(
                "mhc remote {verb}: {message}"
            )));
            return code;
        }
    };
    let record = if let SessionArgs::Send { text, delivery, .. } = session
        && text == STDIN_TEXT
    {
        let stdin_text = ctx.read_stdin();
        if stdin_text.is_empty() {
            ctx.err(
                "mhc remote send: stdin was empty; pipe the message text in, or pass it as an argument",
            );
            return RemoteExit::Usage.code();
        }
        let method = match delivery {
            Delivery::Auto => "prompt",
            Delivery::Steer => "steer",
            Delivery::FollowUp => "follow_up",
        };
        json!({ "type": method, "message": stdin_text })
    } else {
        record
    };
    let attach = match client.attach(&resolved.device, &resolved.session).await {
        Ok(attach) => attach,
        Err(error) => return report_failure(ctx, &error, json),
    };
    if let SessionArgs::Close { force: false, .. } = session {
        let state = client
            .command(&attach, json!({ "type": "get_state" }), None)
            .await;
        let running = matches!(&state, Ok(outcome) if outcome.applied()
            && outcome.receipt.as_ref().is_some_and(|receipt| receipt["state"]["isStreaming"] == true));
        if running {
            let _ = client.detach(&attach).await;
            if json {
                ctx.out(
                    &json!({
                        "kind": "error",
                        "code": "refused",
                        "message": format!("{} {TURN_ACTIVE}", resolved.label),
                        "details": { "reason": "turn_active" },
                    })
                    .to_string(),
                );
            }
            ctx.err(&escape_for_display(&format!(
                "mhc remote close: {} {TURN_ACTIVE}; `mhc remote close {} --force` ends it anyway.",
                resolved.label, resolved.label
            )));
            return RemoteExit::Failed.code();
        }
    }
    let outcome = client.command(&attach, record, None).await;
    let _ = client.detach(&attach).await;
    match outcome {
        Ok(outcome) => report_outcome(ctx, verb, session, &resolved.label, &outcome, json),
        Err(error) => report_failure(ctx, &error, json),
    }
}

fn applied_line(verb: &str, session: &SessionArgs, label: &str, receipt: &Value) -> String {
    let raw = match (verb, session) {
        (
            "send",
            SessionArgs::Send {
                delivery: Delivery::Steer,
                ..
            },
        ) => {
            format!("Steered the running turn on {label}.")
        }
        ("send", _) => match receipt["disposition"].as_str() {
            Some("started") => format!("Sent to {label}: turn started."),
            Some("queued") => format!("Queued on {label}."),
            Some("held") => format!("Queued on {label} (held)."),
            _ => format!("Sent to {label}."),
        },
        ("model", SessionArgs::Model { model, .. }) => format!("{label} now uses {model}."),
        ("interrupt", _) if receipt["aborted"] == false => {
            format!("Nothing was running on {label}.")
        }
        ("interrupt", _) => format!("Interrupted {label}."),
        _ => format!("Closed {label}."),
    };
    escape_for_display(&raw)
}

fn report_outcome(
    ctx: &RemoteContext,
    verb: &str,
    session: &SessionArgs,
    label: &str,
    outcome: &CommandOutcome,
    json: bool,
) -> i32 {
    // omo#13905: the shared close body answers a session its device does not hold as ok with closed:false. For the CLI
    // that is a failed close: a script must never read an id or route miss as success while the session may still run.
    if verb == "close"
        && outcome.state == "engine_applied"
        && outcome
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.get("closed"))
            .and_then(Value::as_bool)
            == Some(false)
    {
        let message = format!("{label} is not open on its device, so nothing was closed.");
        let next_action = "Run mhc remote sessions to see the sessions each device holds, then close one by its address.";
        if json {
            ctx.out(
                &json!({
                    "kind": "error",
                    "code": "not_found",
                    "message": message,
                    "next_action": next_action,
                    "details": { "thread": label },
                    "error": {
                        "code": "not_found",
                        "message": message,
                        "next_action": next_action,
                        "details": { "thread": label },
                    },
                })
                .to_string(),
            );
        }
        ctx.err(&escape_for_display(&format!("mhc remote close: {message}")));
        ctx.err(next_action);
        return RemoteExit::Failed.code();
    }
    if json {
        ctx.out(
            &json!({
                "kind": if outcome.applied() { "ok" } else { "error" },
                "verb": verb,
                "target": label,
                "command_id": outcome.command_id,
                "state": outcome.state,
                "reason": outcome.reason,
                "retryable": outcome.retryable,
                "receipt": outcome.receipt,
            })
            .to_string(),
        );
    }
    let reason = outcome.reason.as_deref().unwrap_or("unknown");
    match outcome.state.as_str() {
        "engine_applied" => {
            if !json {
                let receipt = outcome.receipt.clone().unwrap_or(Value::Null);
                ctx.out(&applied_line(verb, session, label, &receipt));
            }
            RemoteExit::Ok.code()
        }
        "engine_refused" => {
            let line = match reason {
                "route_unlisted" => format!(
                    "mhc remote {verb}: {label} is no longer listed on its device (it ended or moved); nothing was sent."
                ),
                "peer_unauthorized" => format!(
                    "mhc remote {verb}: remote control is off on that device (mhc remote control on, there)."
                ),
                "turn_conflict" | "not_steerable" => {
                    format!("mhc remote {verb}: {label} refused the steer ({reason}).")
                }
                other => format!("mhc remote {verb}: {label} refused it ({other})."),
            };
            ctx.err(&escape_for_display(&line));
            RemoteExit::Failed.code()
        }
        "not delivered: never_sent" | "not confirmed" => {
            ctx.err(&escape_for_display(&format!(
                "mhc remote {verb}: nothing was sent: the device holding {label} did not answer. Try again."
            )));
            RemoteExit::Unavailable.code()
        }
        _ => {
            ctx.err(&escape_for_display(&format!(
                "mhc remote {verb}: the outcome is unknown ({reason}); check mhc remote sessions before sending again."
            )));
            RemoteExit::Failed.code()
        }
    }
}

#[cfg(test)]
#[path = "verbs_resolve_tests.rs"]
mod resolve_tests;
