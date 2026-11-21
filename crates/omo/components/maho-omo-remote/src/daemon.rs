//! `mhc remote daemon` (mhc-relayd in the foreground) and `mhc remote bootstrap` (this device's key,
//! `relayd.json`, and the always-on units, port of `remote-bootstrap.ts` without the closed service's
//! download).

use std::sync::Arc;

use device_service_core::cold_wake::installed_unit::read_installed_session_host_unit;
use device_service_core::cold_wake::{
    ColdWakeOutcome, ManagedUnit, ServicePlatform, SessionHostLaunch, UnitLaunchOptions,
    install_unit, unit_path,
};
use mhc_relayd::config::CONFIG_FILE;
use mhc_relayd::daemon::service_answers;
use mhc_relayd::identity::IDENTITY_FILE;
use mhc_relayd::{DeviceIdentity, Relayd, RelaydConfig, RelaydOptions};
use serde_json::{Value, json};

use crate::context::RemoteContext;
use crate::exit::RemoteExit;
use crate::rpc_host::RpcSessionHost;

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn run_daemon(ctx: &RemoteContext) -> i32 {
    if service_answers(&ctx.state_dir) {
        ctx.err(&format!(
            "mhc remote daemon: a device service already answers on {}; mhc uses it.",
            ctx.state_dir.display()
        ));
        return RemoteExit::Unavailable.code();
    }
    let config = match RelaydConfig::load(&ctx.state_dir) {
        Ok(config) => config,
        Err(error) => {
            ctx.err(&format!("mhc remote daemon: {error}"));
            return RemoteExit::Failed.code();
        }
    };
    let identity = match DeviceIdentity::load_or_create(&ctx.state_dir, config.name.as_deref()) {
        Ok(identity) => identity,
        Err(error) => {
            ctx.err(&format!("mhc remote daemon: {}", error.reason));
            return RemoteExit::Failed.code();
        }
    };
    let relayd = match Relayd::start(RelaydOptions {
        state_dir: ctx.state_dir.clone(),
        config,
        identity: identity.clone(),
        host: Arc::new(RpcSessionHost::new(ctx.agent_dir.clone())),
        service_version: ctx.version.clone(),
    })
    .await
    {
        Ok(relayd) => relayd,
        Err(error) => {
            ctx.err(&format!("mhc remote daemon: {error}"));
            return RemoteExit::Unavailable.code();
        }
    };
    let agent_dir = ctx.agent_dir.to_string_lossy().into_owned();
    if let Err(error) = relayd.state().update_settings(|settings| {
        settings.endpoints.insert(agent_dir.clone());
    }) {
        ctx.err(&format!(
            "mhc remote daemon: could not register {agent_dir}: {error}"
        ));
    }
    ctx.out(&format!(
        "mhc-relayd {}: serving {} as {} ({})",
        ctx.version,
        ctx.state_dir.display(),
        identity.name(),
        identity.id()
    ));
    if let Some(address) = relayd.peer_address() {
        ctx.out(&format!("peer link listening on {address}"));
    }
    wait_for_stop().await;
    relayd.shutdown().await;
    RemoteExit::Ok.code()
}

fn outcome_json(outcome: &ColdWakeOutcome) -> Value {
    match outcome {
        ColdWakeOutcome::Installed { unit_changed } => {
            json!({ "kind": "installed", "unit_changed": unit_changed })
        }
        ColdWakeOutcome::Removed => json!({ "kind": "removed" }),
        ColdWakeOutcome::Unsupported => json!({ "kind": "unsupported" }),
        ColdWakeOutcome::Failed { step, detail } => {
            json!({ "kind": "failed", "step": step, "detail": detail })
        }
    }
}

fn outcome_line(name: &str, outcome: &ColdWakeOutcome) -> String {
    match outcome {
        ColdWakeOutcome::Installed { unit_changed: true } => {
            format!("{name}: installed and started.")
        }
        ColdWakeOutcome::Installed {
            unit_changed: false,
        } => format!("{name}: already installed; started."),
        ColdWakeOutcome::Removed => format!("{name}: removed."),
        ColdWakeOutcome::Unsupported => format!("{name}: not supported on this platform."),
        ColdWakeOutcome::Failed { step, detail } => format!("{name}: failed at {step}: {detail}"),
    }
}

/// Sets this device up: its key, `relayd.json`, and (where a user service manager exists) the
/// always-on `mhc-relayd` and session host units, then registers this agent dir with the service
/// when it already answers. An installed unit's PATH is carried over (omo#14191).
pub async fn run_bootstrap(ctx: &RemoteContext, json_output: bool) -> i32 {
    if !ctx.state_dir.join(IDENTITY_FILE).exists() && service_answers(&ctx.state_dir) {
        let registered = match ctx.connect().await {
            Ok(client) => {
                let registered = client.register_endpoint(&ctx.agent_dir).await.is_ok();
                client.close().await;
                registered
            }
            Err(_) => false,
        };
        ctx.out(
            "Another device service (omo's) runs on this device: mhc uses it and installs nothing.",
        );
        return if registered {
            RemoteExit::Ok.code()
        } else {
            RemoteExit::Unavailable.code()
        };
    }
    let config = match RelaydConfig::load(&ctx.state_dir) {
        Ok(config) => config,
        Err(error) => {
            ctx.err(&format!("mhc remote bootstrap: {error}"));
            return RemoteExit::Failed.code();
        }
    };
    let identity = match DeviceIdentity::load_or_create(&ctx.state_dir, config.name.as_deref()) {
        Ok(identity) => identity,
        Err(error) => {
            ctx.err(&format!("mhc remote bootstrap: {}", error.reason));
            return RemoteExit::Failed.code();
        }
    };
    if !ctx.state_dir.join(CONFIG_FILE).exists()
        && let Err(error) = config.save(&ctx.state_dir)
    {
        ctx.err(&format!("mhc remote bootstrap: {error}"));
        return RemoteExit::Failed.code();
    }
    let mut units = serde_json::Map::new();
    let mut lines = vec![
        format!("This device: {} ({})", identity.name(), identity.id()),
        format!("Public key: {}", identity.public_key()),
        format!("Account: {}", config.account),
    ];
    let mut failed = false;
    match (&ctx.cold_wake, &ctx.mhc_bin) {
        (Some(host), Some(mhc_bin)) => {
            let darwin = host.platform == ServicePlatform::Darwin;
            let installed_path = std::fs::read_to_string(unit_path(host, ManagedUnit::SessionHost))
                .ok()
                .and_then(|text| read_installed_session_host_unit(&text, darwin))
                .and_then(|unit| unit.env.get("PATH").cloned());
            let options = UnitLaunchOptions {
                mhc_bin: mhc_bin.clone(),
                agent_dir: ctx.agent_dir.to_string_lossy().into_owned(),
                path: installed_path.or_else(|| std::env::var("PATH").ok()),
            };
            for (unit, name, launch) in [
                (
                    ManagedUnit::DeviceService,
                    "device service",
                    SessionHostLaunch::device_service(&options),
                ),
                (
                    ManagedUnit::SessionHost,
                    "session host",
                    SessionHostLaunch::session_host(&options),
                ),
            ] {
                let outcome = match launch {
                    Ok(launch) => install_unit(host, unit, &launch),
                    Err(error) => ColdWakeOutcome::Failed {
                        step: "unit_launch".into(),
                        detail: error.to_string(),
                    },
                };
                failed |= outcome.is_failure();
                lines.push(outcome_line(name, &outcome));
                units.insert(name.replace(' ', "_"), outcome_json(&outcome));
            }
        }
        _ => {
            lines.push("This platform has no user service manager mhc installs into; run `mhc remote daemon` yourself.".into());
        }
    }
    let registered = match ctx.connect().await {
        Ok(client) => {
            let registered = client.register_endpoint(&ctx.agent_dir).await.is_ok();
            client.close().await;
            registered
        }
        Err(_) => false,
    };
    lines.push(if registered {
        format!(
            "Registered {} with the device service.",
            ctx.agent_dir.display()
        )
    } else {
        "The device service does not answer yet; it registers this agent dir when it starts.".into()
    });
    if json_output {
        ctx.out(
            &json!({
                "device": { "id": identity.id(), "name": identity.name(), "public_key": identity.public_key() },
                "account": config.account,
                "units": units,
                "registered": registered,
            })
            .to_string(),
        );
    } else {
        for line in &lines {
            ctx.out(line);
        }
    }
    if failed {
        RemoteExit::Failed.code()
    } else {
        RemoteExit::Ok.code()
    }
}
