//! Port of `packages/omo-native/account-cli.ts`, `account-show.ts`, `account-session-outcome.ts`,
//! `account-store.ts`, and `#14711` (`logout` single store call on storage failure): `mhc account`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memory_core::sync::crypto::trust_pin::{PinLoad, forget_trust_anchor, load_trust_anchor};
use serde_json::{Value, json};

use crate::account_store::{
    AccountCredentialStore, AccountExit, AccountStoreError, BeforeKeysDiscarded, BoxFuture,
    Credentials, DiscardCause, DiscardedCredential, FileAccountCredentialStore,
    clear_signed_out_marker, credential_id, parse_expiry_ms, read_signed_out_marker,
    retire_sync_pin_on_discard, signed_out_marker_path, write_signed_out_marker,
};
use crate::context::{LineSink, RemoteContext};
use crate::device_service_handoff::{CliDeviceServiceHandoff, DeviceServiceHandoff};
use crate::sync_status::sync_marks_dir;

pub const ACCOUNT_USAGE: &str = "usage: omo account show [--account <id>]|adopt [--replace]|token|export|forget [--device <id>]|devices [revoke <id>]|sign-in [--stdin|--check]|retire-sync --account <id>|store status|enable|disable [--discard]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Refreshed {
        access_token: String,
        access_token_expires_at: String,
        refresh_token: String,
    },
    Refused {
        status: u16,
        code: String,
    },
    Pending {
        deadline_iso: String,
    },
    RateLimited,
    NotSent,
    Failed(String),
}

pub type RefreshTokenFn =
    Arc<dyn Fn(&str, &str) -> BoxFuture<'static, RefreshOutcome> + Send + Sync>;

pub struct AccountCommandDependencies {
    pub api: String,
    pub store: Arc<dyn AccountCredentialStore>,
    pub home: PathBuf,
    pub pin_dir: PathBuf,
    pub before_keys_discarded: BeforeKeysDiscarded,
    pub device_service: Option<Arc<dyn DeviceServiceHandoff>>,
    pub refresh_token: RefreshTokenFn,
    pub read_stdin: Arc<dyn Fn() -> String + Send + Sync>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub output: LineSink,
    pub error: LineSink,
}

impl AccountCommandDependencies {
    fn reply(&self, value: &Value, code: AccountExit) -> i32 {
        (self.output)(&value.to_string());
        code.code()
    }

    fn marker_path(&self) -> PathBuf {
        signed_out_marker_path(&self.home, &credential_id(&self.api))
    }

    fn discard(&self, current: &Credentials, cause: DiscardCause) {
        (self.before_keys_discarded)(DiscardedCredential::Credential {
            device_id: current.device.id.clone(),
            account_id: current.effective_account_id().map(str::to_owned),
            cause,
        });
    }

    fn signed_out(&self) -> i32 {
        match read_signed_out_marker(&self.marker_path()) {
            None => self.reply(&json!({ "signedIn": false }), AccountExit::SignedOut),
            Some(marker) => self.reply(
                &json!({ "signedIn": false, "signedOut": marker.to_value() }),
                AccountExit::LoggedOut,
            ),
        }
    }
}

pub fn account_store_failure(deps: &AccountCommandDependencies, error: &AccountStoreError) -> i32 {
    (deps.error)(error.message());
    match error {
        AccountStoreError::KeychainLocked(_) => deps.reply(
            &json!({ "keychain": "locked" }),
            AccountExit::KeychainLocked,
        ),
        AccountStoreError::KeychainUnavailable(_) => deps.reply(
            &json!({ "keychain": "unavailable" }),
            AccountExit::KeychainUnavailable,
        ),
        AccountStoreError::StoreUnavailable(_) => deps.reply(
            &json!({ "error": "store-unavailable" }),
            AccountExit::StoreUnavailable,
        ),
        AccountStoreError::InvalidState(_) | AccountStoreError::Other(_) => {
            AccountExit::Failed.code()
        }
    }
}

fn sync_state_of(pin_dir: &Path, account: &str) -> &'static str {
    match load_trust_anchor(pin_dir, account) {
        Ok(PinLoad::Loaded { .. }) => "joined",
        Ok(PinLoad::Refused {
            reason: "corrupt_pin",
            ..
        }) => "corrupt_pin",
        _ => "not_joined",
    }
}

fn reported_refresh_state(current: &Credentials) -> &'static str {
    if current.refresh_state.as_deref() != Some("uncertain") {
        return "ok";
    }
    let Some(attempt) = &current.refresh_attempt else {
        return "expired";
    };
    if attempt.get("state").and_then(Value::as_str) == Some("exhausted") || current.owner.is_none()
    {
        return "expired";
    }
    "uncertain"
}

pub async fn show_account(
    deps: &AccountCommandDependencies,
    flags: &[String],
) -> Result<i32, AccountStoreError> {
    let current = deps.store.read().await?;
    let account = if let Some(pos) = flags.iter().position(|flag| flag == "--account") {
        let Some(id) = flags.get(pos + 1).filter(|id| !id.is_empty()) else {
            (deps.error)("omo account show: --account needs an account id");
            return Ok(AccountExit::Invalid.code());
        };
        Some(id.as_str())
    } else {
        None
    };
    let mut body = match &current {
        None => {
            let mut obj = json!({ "signedIn": false });
            if let Some(marker) = read_signed_out_marker(&deps.marker_path()) {
                obj["signedOut"] = marker.to_value();
            }
            obj
        }
        Some(cred) => json!({
            "signedIn": true,
            "device": { "id": cred.device.id, "name": cred.device.name },
            "refreshState": reported_refresh_state(cred),
        }),
    };
    if let Some(account) = account {
        body["sync"] = Value::String(sync_state_of(&deps.pin_dir, account).into());
    }
    if let Some(storage) = deps.store.storage() {
        body["storage"] = Value::String(storage.into());
    }
    Ok(deps.reply(&body, AccountExit::Ok))
}

async fn adopt(deps: &AccountCommandDependencies, replace: bool) -> Result<i32, AccountStoreError> {
    let Some(incoming) = Credentials::parse_adopt_input(&(deps.read_stdin)()) else {
        (deps.error)("omo account adopt: stdin is not a valid credential");
        return Ok(AccountExit::Invalid.code());
    };
    let current = match deps.store.read().await {
        Ok(current) => current,
        Err(AccountStoreError::InvalidState(_)) if replace => None,
        Err(error) => return Err(error),
    };
    if let Some(curr) = &current
        && curr.device.id != incoming.device.id
        && !replace
    {
        return Ok(deps.reply(
            &json!({
                "conflict": {
                    "device": { "id": curr.device.id, "name": curr.device.name }
                }
            }),
            AccountExit::Conflict,
        ));
    }
    let keep_current = current.as_ref().is_some_and(|curr| {
        curr.device.id == incoming.device.id
            && curr.refresh_state.as_deref() != Some("uncertain")
            && parse_expiry_ms(&curr.access_token_expires_at)
                > parse_expiry_ms(&incoming.access_token_expires_at)
    });
    if let Some(curr) = &current
        && curr.device.id != incoming.device.id
    {
        deps.discard(curr, DiscardCause::Replaced);
    }
    if !keep_current {
        deps.store.write(&incoming).await?;
    }
    clear_signed_out_marker(&deps.marker_path());
    let adopted = match (&current, keep_current) {
        (Some(curr), true) => curr.clone(),
        _ => incoming.clone(),
    };
    let mut reply = json!({
        "adopted": true,
        "device": { "id": incoming.device.id, "name": incoming.device.name },
    });
    if let Some(storage) = deps.store.storage() {
        reply["storage"] = Value::String(storage.into());
    }
    let code = deps.reply(&reply, AccountExit::Ok);
    if let Some(ds) = &deps.device_service {
        ds.adopt(&adopted).await;
    }
    Ok(code)
}

const REVOKING_CODES: [&str; 5] = [
    "unauthorized",
    "account_deleted",
    "reauth_required",
    "invalid_grant",
    "device_revoked",
];

async fn token(deps: &AccountCommandDependencies) -> Result<i32, AccountStoreError> {
    let Some(current) = deps.store.read().await? else {
        return Ok(deps.signed_out());
    };
    if current.refresh_state.as_deref() == Some("uncertain") || current.refresh_attempt.is_some() {
        return Ok(deps.reply(
            &json!({ "ended": { "code": "refresh_recovery_expired" } }),
            AccountExit::Ended,
        ));
    }
    let now_ms = (deps.now)();
    if parse_expiry_ms(&current.access_token_expires_at).unwrap_or(0) > now_ms + 30_000 {
        let mut reply = json!({
            "accessToken": current.access_token,
            "accessTokenExpiresAt": current.access_token_expires_at,
            "device": { "id": current.device.id, "name": current.device.name },
        });
        if let Some(owner) = &current.owner {
            reply["owner"] = owner.clone();
        }
        return Ok(deps.reply(&reply, AccountExit::Ok));
    }
    let journaled = current.with_refresh_state(Some("uncertain"));
    deps.store.write(&journaled).await?;
    match (deps.refresh_token)(&deps.api, &current.refresh_token).await {
        RefreshOutcome::Refreshed {
            access_token,
            access_token_expires_at,
            refresh_token,
        } => {
            let mut next = current.with_refresh_state(None);
            next.access_token = access_token;
            next.access_token_expires_at = access_token_expires_at;
            next.refresh_token = refresh_token;
            deps.store.write(&next).await?;
            let mut reply = json!({
                "accessToken": next.access_token,
                "accessTokenExpiresAt": next.access_token_expires_at,
                "device": { "id": next.device.id, "name": next.device.name },
            });
            if let Some(owner) = &next.owner {
                reply["owner"] = owner.clone();
            }
            Ok(deps.reply(&reply, AccountExit::Ok))
        }
        RefreshOutcome::Refused { status, code }
            if status < 500 && REVOKING_CODES.contains(&code.as_str()) =>
        {
            deps.discard(&current, DiscardCause::Refused);
            deps.store.clear().await?;
            if let Some(ds) = &deps.device_service {
                ds.forget().await;
            }
            Ok(deps.reply(&json!({ "ended": { "code": code } }), AccountExit::Ended))
        }
        RefreshOutcome::Pending { deadline_iso } => Ok(deps.reply(
            &json!({ "uncertain": { "deadline": deadline_iso } }),
            AccountExit::Uncertain,
        )),
        RefreshOutcome::RateLimited | RefreshOutcome::NotSent => {
            deps.store.write(&current).await?;
            session_failure(deps, "omo account token: the refresh failed").await
        }
        RefreshOutcome::Refused { .. } | RefreshOutcome::Failed(_) => {
            session_failure(deps, "omo account token: the refresh failed").await
        }
    }
}

async fn session_failure(
    deps: &AccountCommandDependencies,
    fallback_message: &str,
) -> Result<i32, AccountStoreError> {
    let after = deps.store.read().await?;
    (deps.error)(fallback_message);
    let Some(after) = after else {
        return Ok(deps.signed_out());
    };
    if after.refresh_state.as_deref() == Some("uncertain") {
        return Ok(deps.reply(
            &json!({ "ended": { "code": "refresh_recovery_expired" } }),
            AccountExit::Ended,
        ));
    }
    Ok(AccountExit::Failed.code())
}

async fn forget(
    deps: &AccountCommandDependencies,
    flags: &[String],
) -> Result<i32, AccountStoreError> {
    if !flags.is_empty() {
        let device = flags.get(1).map(String::as_str).unwrap_or_default();
        if flags.len() != 2
            || flags[0] != "--device"
            || device.is_empty()
            || device.starts_with("--")
        {
            (deps.error)("omo account forget: use forget or forget --device <id>");
            return Ok(AccountExit::Invalid.code());
        }
        let current = match deps.store.read().await {
            Ok(current) => current,
            Err(AccountStoreError::InvalidState(message)) => {
                (deps.error)(&message);
                return Ok(deps.reply(
                    &json!({ "forgotten": false, "error": "unreadable-credential" }),
                    AccountExit::Failed,
                ));
            }
            Err(error) => return Err(error),
        };
        if let Some(curr) = &current
            && curr.device.id != device
        {
            (deps.error)(&format!(
                "omo account forget --device acts on this device only; sign out device {device} with omo account devices revoke {device}"
            ));
            return Ok(deps.reply(
                &json!({
                    "forgotten": false,
                    "device": { "id": curr.device.id, "name": curr.device.name }
                }),
                AccountExit::Conflict,
            ));
        }
        if let Some(curr) = &current {
            deps.discard(curr, DiscardCause::Forget);
        }
        deps.store.clear().await?;
        let code = deps.reply(&json!({ "forgotten": true }), AccountExit::Ok);
        if let Some(ds) = &deps.device_service {
            ds.forget().await;
        }
        return Ok(code);
    }
    match deps.store.read().await {
        Ok(Some(curr)) => {
            deps.discard(&curr, DiscardCause::Forget);
            deps.store.clear().await?;
        }
        Ok(None) => {
            deps.store.clear().await?;
        }
        Err(AccountStoreError::InvalidState(_)) => {
            deps.store.clear().await?;
            (deps.before_keys_discarded)(DiscardedCredential::Unreadable);
        }
        Err(error) => return Err(error),
    }
    if let Some(ds) = &deps.device_service {
        ds.forget().await;
    }
    Ok(deps.reply(&json!({ "forgotten": true }), AccountExit::Ok))
}

fn run_account_store(deps: &AccountCommandDependencies, flags: &[String]) -> i32 {
    let verb = flags.first().map(String::as_str).unwrap_or_default();
    let rest = flags.get(1..).unwrap_or_default();
    let discard = rest.len() == 1 && rest[0] == "--discard";
    let valid = (verb == "status" && rest.is_empty())
        || (verb == "enable" && rest.is_empty())
        || (verb == "disable" && (rest.is_empty() || discard));
    if !valid {
        (deps.error)("usage: omo account store status|enable|disable [--discard]");
        return AccountExit::Invalid.code();
    }
    if verb == "status" {
        let path = deps.home.join(".omo").join("keychain");
        return deps.reply(
            &json!({ "store": "login", "state": "absent", "path": path }),
            AccountExit::Ok,
        );
    }
    (deps.error)("omo account store: OmO's own keychain exists on macOS only.");
    AccountExit::Invalid.code()
}

pub async fn logout_account_session(deps: &AccountCommandDependencies) -> i32 {
    let current = match deps.store.read().await {
        Ok(current) => Ok(current),
        Err(AccountStoreError::InvalidState(_)) => Err(()),
        Err(
            AccountStoreError::StoreUnavailable(_)
            | AccountStoreError::KeychainLocked(_)
            | AccountStoreError::KeychainUnavailable(_),
        ) => {
            if let Some(ds) = &deps.device_service {
                ds.forget().await;
            }
            (deps.error)(
                "OmO could not read the credential store. Stored credentials and device keys were left untouched; run omo logout again when the keyring answers.",
            );
            return AccountExit::StoreUnavailable.code();
        }
        Err(AccountStoreError::Other(message)) => {
            if let Some(ds) = &deps.device_service {
                ds.forget().await;
            }
            (deps.error)(&message);
            return AccountExit::Failed.code();
        }
    };
    let device_id = match &current {
        Ok(Some(cred)) => Some(cred.device.id.as_str()),
        Ok(None) | Err(()) => None,
    };
    if let Err(error) = write_signed_out_marker(&deps.marker_path(), device_id) {
        if let Some(ds) = &deps.device_service {
            ds.forget().await;
        }
        (deps.error)(&error);
        return AccountExit::Failed.code();
    }
    match &current {
        Ok(Some(cred)) => deps.discard(cred, DiscardCause::Logout),
        Ok(None) => {}
        Err(()) => (deps.before_keys_discarded)(DiscardedCredential::Unreadable),
    }
    let cleared = deps.store.clear().await;
    if let Some(ds) = &deps.device_service {
        ds.forget().await;
    }
    if let Err(error) = cleared {
        (deps.error)(error.message());
        return AccountExit::Failed.code();
    }
    AccountExit::Ok.code()
}

pub async fn run_account_verb(deps: &AccountCommandDependencies, args: &[String]) -> i32 {
    let verb = args.first().map(String::as_str).unwrap_or_default();
    let flags = args.get(1..).unwrap_or_default();
    let outcome = match verb {
        "show" => show_account(deps, flags).await,
        "adopt" => adopt(deps, flags.iter().any(|flag| flag == "--replace")).await,
        "token" => token(deps).await,
        "export" => match deps.store.read().await {
            Ok(None) => Ok(deps.signed_out()),
            Ok(Some(cred)) => Ok(deps.reply(&cred.to_value(), AccountExit::Ok)),
            Err(error) => Err(error),
        },
        "forget" => forget(deps, flags).await,
        "store" => Ok(run_account_store(deps, flags)),
        "retire-sync" => {
            let account = if flags.len() == 2 && flags[0] == "--account" {
                Some(flags[1].as_str()).filter(|id| !id.is_empty() && !id.starts_with("--"))
            } else {
                None
            };
            let Some(account) = account else {
                (deps.error)("omo account retire-sync: use retire-sync --account <id>");
                return AccountExit::Invalid.code();
            };
            if let Err(error) = forget_trust_anchor(&deps.pin_dir, account) {
                (deps.error)(&error.to_string());
                return AccountExit::Failed.code();
            }
            Ok(deps.reply(&json!({ "retired": true }), AccountExit::Ok))
        }
        _ => {
            (deps.error)(ACCOUNT_USAGE);
            Ok(AccountExit::Invalid.code())
        }
    };
    match outcome {
        Ok(code) => code,
        Err(error) => account_store_failure(deps, &error),
    }
}

pub async fn run_account_cli(ctx: &RemoteContext, args: &[String]) -> i32 {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.state_dir.clone());
    let env: BTreeMap<String, String> = maho_core::config::current_env().into_iter().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let pin_dir = sync_marks_dir(&env, &cwd);
    let api = env
        .get("OMO_SERVICE_API_URL")
        .cloned()
        .unwrap_or_else(|| "https://api.omo.dev".into());
    let store: Arc<dyn AccountCredentialStore> =
        Arc::new(FileAccountCredentialStore::new(&home, &api));
    let handoff: Arc<dyn DeviceServiceHandoff> = Arc::new(CliDeviceServiceHandoff::for_state_dir(
        api.clone(),
        ctx.state_dir.clone(),
        &ctx.version,
        Arc::clone(&ctx.error),
    ));
    let ctx_for_stdin = ctx.clone();
    let deps = AccountCommandDependencies {
        api,
        store,
        home,
        pin_dir: pin_dir.clone(),
        before_keys_discarded: retire_sync_pin_on_discard(pin_dir, Arc::clone(&ctx.error)),
        device_service: Some(handoff),
        refresh_token: Arc::new(|_api, _refresh| {
            Box::pin(async {
                RefreshOutcome::Failed(
                    "mhc has no hosted account refresh service in local mode".into(),
                )
            })
        }),
        read_stdin: Arc::new(move || ctx_for_stdin.read_stdin()),
        now: Arc::clone(&ctx.now),
        output: Arc::clone(&ctx.output),
        error: Arc::clone(&ctx.error),
    };
    run_account_verb(&deps, args).await
}
