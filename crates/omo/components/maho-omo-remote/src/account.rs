//! `mhc login`, `mhc whoami`, `mhc logout` (omo `account-cli.ts`), in local mode.
//!
//! omo signs a device in to its hosted account service. mhc has no hosted sign-in (the service
//! endpoints resolve to local mode unless configured, and even then no sign-in flow is served), so
//! signing in is this device's own key in the device service's state dir: `login` makes it (or takes
//! a signed-out device back), `whoami` shows it, `logout` signs the device out (the service serves no
//! peer) and keeps the key; `logout --forget-key` also removes it. Devices form an account by listing
//! each other's public keys in `relayd.json`. omo's busy sign-in lock (v5.1.77) has no counterpart:
//! there is no credential store lock to hold.

use device_service_core::local_api::LocalApiClient;
use maho_service_endpoints::resolve_service_api_from_process;
use mhc_relayd::daemon::service_answers;
use mhc_relayd::identity::IDENTITY_FILE;
use mhc_relayd::{DeviceIdentity, RelaydConfig};
use serde_json::json;

use crate::context::RemoteContext;
use crate::exit::RemoteExit;
use crate::render::state_label;
use crate::usage::is_help_flag;

fn usage(verb: &str) -> &'static str {
    match verb {
        "login" => {
            "usage: mhc login [--name NAME] [--json]\n  make this device's key (local mode: there is no hosted sign-in); list it on your other devices"
        }
        "whoami" => "usage: mhc whoami [--json]\n  this device's name, id, public key and account",
        _ => {
            "usage: mhc logout [--forget-key] [--json]\n  sign this device out of remote sessions; --forget-key also removes its key"
        }
    }
}

struct AccountArgs {
    json: bool,
    name: Option<String>,
    forget_key: bool,
}

fn parse(verb: &str, args: &[String]) -> Result<AccountArgs, String> {
    let mut parsed = AccountArgs {
        json: false,
        name: None,
        forget_key: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => parsed.json = true,
            "--forget-key" if verb == "logout" => parsed.forget_key = true,
            "--name" if verb == "login" => {
                let name = args
                    .get(index + 1)
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| "mhc login: --name needs a value".to_owned())?;
                parsed.name = Some(name.clone());
                index += 1;
            }
            other => return Err(format!("mhc {verb}: unknown argument {other}")),
        }
        index += 1;
    }
    Ok(parsed)
}

/// Runs `mhc <verb>` for `login`, `whoami` or `logout`; the process exit code.
pub async fn run_account_command(ctx: &RemoteContext, verb: &str, args: &[String]) -> i32 {
    if args.iter().any(|arg| is_help_flag(arg)) {
        ctx.out(usage(verb));
        return RemoteExit::Ok.code();
    }
    let parsed = match parse(verb, args) {
        Ok(parsed) => parsed,
        Err(message) => {
            ctx.err(&message);
            ctx.err(usage(verb));
            return RemoteExit::Usage.code();
        }
    };
    match verb {
        "login" => login(ctx, parsed).await,
        "whoami" => whoami(ctx, parsed.json).await,
        _ => logout(ctx, parsed).await,
    }
}

/// Sets the signed-out switch where no service runs to take it: the service reads it at its start.
fn set_signed_out_offline(ctx: &RemoteContext, forgotten: bool) -> Result<(), String> {
    let mut settings = mhc_relayd::settings::Settings::load(&ctx.state_dir);
    if settings.forgotten == forgotten {
        return Ok(());
    }
    settings.forgotten = forgotten;
    settings.save(&ctx.state_dir)
}

async fn connected(ctx: &RemoteContext) -> Option<LocalApiClient> {
    ctx.connect().await.ok()
}

async fn login(ctx: &RemoteContext, parsed: AccountArgs) -> i32 {
    if !ctx.state_dir.join(IDENTITY_FILE).exists() && service_answers(&ctx.state_dir) {
        ctx.err("omo's device service runs on this device and holds its sign-in: sign in with `omo login`.");
        return RemoteExit::Unavailable.code();
    }
    let hosted = match resolve_service_api_from_process(None) {
        Ok(Some(origin)) => Some(format!(
            "A service API is configured ({}), but mhc has no hosted sign-in: this device signs in locally.",
            origin.origin
        )),
        Ok(None) => None,
        Err(error) => Some(error.to_string()),
    };
    let (identity, created) = match DeviceIdentity::load(&ctx.state_dir) {
        Ok(Some(identity)) => (identity, false),
        Ok(None) => {
            let name = parsed.name.clone().or_else(|| {
                RelaydConfig::load(&ctx.state_dir)
                    .ok()
                    .and_then(|config| config.name)
            });
            match DeviceIdentity::load_or_create(&ctx.state_dir, name.as_deref()) {
                Ok(identity) => (identity, true),
                Err(error) => {
                    ctx.err(&format!("mhc login: {}", error.reason));
                    return RemoteExit::Failed.code();
                }
            }
        }
        Err(error) => {
            ctx.err(&format!("mhc login: {}", error.reason));
            return RemoteExit::Failed.code();
        }
    };
    match connected(ctx).await {
        Some(client) => {
            let signed_out = client
                .status()
                .await
                .is_ok_and(|status| status["state"] == "signed_out");
            if signed_out {
                let _ = client.set_enabled(true).await;
            }
            client.close().await;
        }
        None => {
            if let Err(error) = set_signed_out_offline(ctx, false) {
                ctx.err(&format!("mhc login: {error}"));
                return RemoteExit::Failed.code();
            }
        }
    }
    if parsed.json {
        ctx.out(
            &json!({
                "device": { "id": identity.id(), "name": identity.name(), "public_key": identity.public_key() },
                "created": created,
                "local_mode": true,
            })
            .to_string(),
        );
    } else {
        if let Some(note) = &hosted {
            ctx.err(note);
        }
        ctx.out(&format!(
            "Signed in on this device as {} ({}), local mode.",
            identity.name(),
            identity.id()
        ));
        ctx.out(&format!("Public key: {}", identity.public_key()));
        ctx.out("List it in the peers of relayd.json on your other devices (docs/remote.md).");
    }
    RemoteExit::Ok.code()
}

async fn whoami(ctx: &RemoteContext, json_output: bool) -> i32 {
    let identity = match DeviceIdentity::load(&ctx.state_dir) {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            if json_output {
                ctx.out(&json!({ "signed_in": false }).to_string());
            }
            ctx.err("Not signed in: this device has no key yet. Run `mhc login`.");
            return RemoteExit::Unavailable.code();
        }
        Err(error) => {
            ctx.err(&format!("mhc whoami: {}", error.reason));
            return RemoteExit::Failed.code();
        }
    };
    let account = RelaydConfig::load(&ctx.state_dir)
        .unwrap_or_default()
        .account;
    let state = match connected(ctx).await {
        Some(client) => {
            let state = client
                .status()
                .await
                .ok()
                .and_then(|status| status["state"].as_str().map(str::to_owned));
            client.close().await;
            state
        }
        None => None,
    };
    if json_output {
        ctx.out(
            &json!({
                "signed_in": true,
                "device": { "id": identity.id(), "name": identity.name(), "public_key": identity.public_key() },
                "account": account,
                "local_mode": true,
                "service": state,
            })
            .to_string(),
        );
    } else {
        ctx.out(&format!(
            "Device      {} ({})",
            identity.name(),
            identity.id()
        ));
        ctx.out(&format!("Public key  {}", identity.public_key()));
        ctx.out(&format!("Account     {account} (local mode)"));
        ctx.out(&format!(
            "Service     {}",
            state.as_deref().map_or("not running", state_label)
        ));
    }
    RemoteExit::Ok.code()
}

async fn logout(ctx: &RemoteContext, parsed: AccountArgs) -> i32 {
    if !ctx.state_dir.join(IDENTITY_FILE).exists() {
        if parsed.json {
            ctx.out(&json!({ "signed_out": false, "key_removed": false }).to_string());
        } else {
            ctx.out("Not signed in on this device.");
        }
        return RemoteExit::Ok.code();
    }
    let forgotten = match connected(ctx).await {
        Some(client) => {
            let forgotten = client.forget_device().await.map_err(|error| error.reason);
            client.close().await;
            forgotten
        }
        None => set_signed_out_offline(ctx, true),
    };
    if let Err(error) = forgotten {
        ctx.err(&format!("mhc logout: {error}"));
        return RemoteExit::Failed.code();
    }
    let key_removed = if parsed.forget_key {
        match DeviceIdentity::remove(&ctx.state_dir) {
            Ok(removed) => removed,
            Err(error) => {
                ctx.err(&format!("mhc logout: {}", error.reason));
                return RemoteExit::Failed.code();
            }
        }
    } else {
        false
    };
    if parsed.json {
        ctx.out(&json!({ "signed_out": true, "key_removed": key_removed }).to_string());
    } else if key_removed {
        ctx.out("Signed out, and this device's key is removed: the next `mhc login` makes a new one, which your other devices must list again. Restart the device service to drop the old key.");
    } else {
        ctx.out("Signed out: other devices no longer reach this one. `mhc login` signs it back in with the same key.");
    }
    RemoteExit::Ok.code()
}
