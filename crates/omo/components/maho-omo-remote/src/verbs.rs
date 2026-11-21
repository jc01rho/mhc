//! Port of `remote-cli.ts`, `remote-local.ts` and `remote-session-verbs.ts`: `mhc remote <verb>`.
//!
//! Every verb but `bootstrap` and `daemon` is local-only (omo#13440): its whole path is this device's
//! local API, so the CLI entry marks the process before it runs one (`is_local_only_verb`).
//! Divergence: `send` returns once the session took the message (omo's `--no-wait`); it does not
//! stream the answer, because mhc-relayd carries no attach stream yet.

use device_service_core::local_api::client::WakeSetting;
use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{CommandOutcome, LocalApiClient, LocalApiError};
use serde_json::{Value, json};

use crate::args::{Delivery, Parsed, SessionArgs, Target, WakeValue, parse};
use crate::context::RemoteContext;
use crate::exit::{RemoteExit, UsageError};
use crate::render::{
    control_lines, device_lines, device_name, session_lines, status_lines, wake_lines,
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
            ctx.err(&format!("mhc remote: unknown command {}", parsed.command));
            ctx.err(&remote_usage());
            return RemoteExit::Usage.code();
        }
        Err(UsageError(message)) => {
            if !message.is_empty() {
                ctx.err(&message);
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
    ctx.err(&format!("mhc remote: {}", error.reason));
    if let Some(action) = &error.action {
        ctx.err(action);
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
                ctx.err(&format!("mhc remote: unknown command {}", parsed.command));
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

fn device_id(row: &Value) -> String {
    row.get("device")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A device by id, by name (case-insensitive; an online one wins a tie), or by a unique id prefix.
pub fn resolve_device<'a>(devices: &'a [Value], query: &str) -> Result<&'a Value, Vec<String>> {
    if let Some(row) = devices.iter().find(|row| device_id(row) == query) {
        return Ok(row);
    }
    let named: Vec<&Value> = devices
        .iter()
        .filter(|row| {
            row.get("label")
                .and_then(Value::as_str)
                .is_some_and(|label| label.eq_ignore_ascii_case(query))
        })
        .collect();
    if let [row] = named.as_slice() {
        return Ok(*row);
    }
    if named.len() > 1 {
        let online: Vec<&Value> = named
            .iter()
            .copied()
            .filter(|row| row.get("online").and_then(Value::as_bool) == Some(true))
            .collect();
        if let [row] = online.as_slice() {
            return Ok(*row);
        }
        return Err(named.iter().map(|row| device_id(row)).collect());
    }
    let prefixed: Vec<&Value> = devices
        .iter()
        .filter(|row| device_id(row).starts_with(query))
        .collect();
    match prefixed.as_slice() {
        [row] if !query.is_empty() => Ok(*row),
        _ => Err(Vec::new()),
    }
}

async fn sessions(ctx: &RemoteContext, client: &LocalApiClient, parsed: &Parsed) -> i32 {
    let json = parsed.json;
    let devices = match client.devices().await {
        Ok(devices) => devices,
        Err(error) => return report_failure(ctx, &error, json),
    };
    let mut filter = serde_json::Map::new();
    if let Some(query) = &parsed.filter.device {
        match resolve_device(&devices, query) {
            Ok(row) => {
                filter.insert("device".into(), row["device"].clone());
            }
            Err(_) => {
                ctx.err(&format!(
                    "mhc remote sessions: no device {query} on this account (see mhc remote devices)"
                ));
                return RemoteExit::Usage.code();
            }
        }
    }
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
    let unanswered: Vec<String> = listing
        .incomplete
        .iter()
        .filter_map(|device| device.as_str().map(str::to_owned))
        .collect();
    let value = json!({
        "rows": listing.rows,
        "devices": listing.devices,
        "incomplete": listing.incomplete,
        "complete": listing.complete,
    });
    show(ctx, json, &value, || {
        session_lines(&listing.rows, &devices, &unanswered, ctx.now())
    })
}

struct Resolved {
    device: String,
    label: String,
    session: String,
}

async fn resolve_target(
    client: &LocalApiClient,
    target: &Target,
) -> Result<Resolved, (i32, String)> {
    let devices = client
        .devices()
        .await
        .map_err(|error| (RemoteExit::Failed.code(), error.reason))?;
    if let Some(query) = &target.device {
        return match resolve_device(&devices, query) {
            Ok(row) => {
                let device = row["device"].as_str().unwrap_or_default().to_owned();
                Ok(Resolved {
                    label: format!("{}/{}", device_name(&devices, &device), target.session),
                    device,
                    session: target.session.clone(),
                })
            }
            Err(candidates) if !candidates.is_empty() => Err((
                RemoteExit::Failed.code(),
                format!(
                    "ambiguous_target: {query} names more than one device: {}",
                    candidates.join(", ")
                ),
            )),
            Err(_) => Err((
                RemoteExit::Usage.code(),
                format!("no device {query} on this account (see mhc remote devices)"),
            )),
        };
    }
    let listing = client
        .sessions_list(None, Some(3))
        .await
        .map_err(|error| (RemoteExit::Failed.code(), error.reason))?;
    let matches: Vec<&Value> = listing
        .rows
        .iter()
        .filter(|row| {
            row["session_id"] == target.session.as_str()
                || row["durable_id"] == target.session.as_str()
        })
        .collect();
    match matches.as_slice() {
        [row] => {
            let device = row["device"].as_str().unwrap_or_default().to_owned();
            let session = row["session_id"].as_str().unwrap_or_default().to_owned();
            Ok(Resolved {
                label: format!("{}/{session}", device_name(&devices, &device)),
                device,
                session,
            })
        }
        [] => Err((
            RemoteExit::Failed.code(),
            format!(
                "no session {} on this account (see mhc remote sessions)",
                target.session
            ),
        )),
        many => Err((
            RemoteExit::Failed.code(),
            format!(
                "ambiguous_target: {} names more than one session; name it as <device>/<session> ({} found)",
                target.session,
                many.len()
            ),
        )),
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
        Err((code, message)) => {
            if json {
                ctx.out(
                    &json!({ "kind": "error", "code": "not_found", "message": message })
                        .to_string(),
                );
            }
            ctx.err(&format!("mhc remote {verb}: {message}"));
            return code;
        }
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
                        "message": format!("{} is running a turn", resolved.label),
                        "details": { "reason": "turn_active" },
                    })
                    .to_string(),
                );
            }
            ctx.err(&format!(
                "mhc remote close: {} is running a turn; `mhc remote close {} --force` ends it anyway.",
                resolved.label, resolved.label
            ));
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
    match (verb, session) {
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
            _ => format!("Sent to {label}."),
        },
        ("model", SessionArgs::Model { model, .. }) => format!("{label} now uses {model}."),
        ("interrupt", _) if receipt["aborted"] == false => {
            format!("Nothing was running on {label}.")
        }
        ("interrupt", _) => format!("Interrupted {label}."),
        ("close", _) if receipt["closed"] == false => format!("{label} was not open."),
        _ => format!("Closed {label}."),
    }
}

fn report_outcome(
    ctx: &RemoteContext,
    verb: &str,
    session: &SessionArgs,
    label: &str,
    outcome: &CommandOutcome,
    json: bool,
) -> i32 {
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
            ctx.err(&match reason {
                "route_unlisted" => format!(
                    "mhc remote {verb}: {label} is no longer listed on its device (it ended or moved); nothing was sent."
                ),
                "peer_unauthorized" => format!(
                    "mhc remote {verb}: remote control is off on that device (mhc remote control on, there)."
                ),
                other => format!("mhc remote {verb}: {label} refused it ({other})."),
            });
            RemoteExit::Failed.code()
        }
        "not delivered: never_sent" | "not confirmed" => {
            ctx.err(&format!(
                "mhc remote {verb}: nothing was sent: the device holding {label} did not answer. Try again."
            ));
            RemoteExit::Unavailable.code()
        }
        _ => {
            ctx.err(&format!(
                "mhc remote {verb}: the outcome is unknown ({reason}); check mhc remote sessions before sending again."
            ));
            RemoteExit::Failed.code()
        }
    }
}
