//! Port of `account-show.ts` (`ACCOUNT_EXIT`, `credentialId`), `protocol.ts` (`credentialSchema`),
//! `signed-out-marker.ts`, `state-file-store.ts`, and `sync-pin-retire.ts` for `mhc account`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use memory_core::sync::crypto::trust_pin::forget_trust_anchor;
use serde_json::{Map, Value, json};

use crate::context::LineSink;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountExit {
    Ok = 0,
    Failed = 1,
    Invalid = 2,
    Conflict = 3,
    SignedOut = 4,
    Ended = 5,
    Uncertain = 6,
    KeychainLocked = 7,
    KeychainUnavailable = 8,
    StoreUnavailable = 9,
    LoggedOut = 10,
}

impl AccountExit {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Credentials {
    pub access_token: String,
    pub access_token_expires_at: String,
    pub refresh_token: String,
    pub device: CredentialDevice,
    pub signing_private_key: String,
    pub sealing_private_key: String,
    pub account_id: Option<String>,
    pub owner: Option<Value>,
    pub refresh_state: Option<String>,
    pub refresh_attempt: Option<Value>,
    pub raw: Map<String, Value>,
}

fn non_empty_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub fn parse_expiry_ms(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

impl Credentials {
    pub fn parse(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let access_token = non_empty_str(map, "accessToken")?;
        let access_token_expires_at = non_empty_str(map, "accessTokenExpiresAt")?;
        let _ = parse_expiry_ms(&access_token_expires_at)?;
        let refresh_token = non_empty_str(map, "refreshToken")?;
        let device_obj = map.get("device")?.as_object()?;
        let device = CredentialDevice {
            id: non_empty_str(device_obj, "id")?,
            name: non_empty_str(device_obj, "name")?,
        };
        let signing_private_key = non_empty_str(map, "signingPrivateKey")?;
        let sealing_private_key = non_empty_str(map, "sealingPrivateKey")?;
        let refresh_state = match map.get("refreshState") {
            None | Some(Value::Null) => None,
            Some(Value::String(state)) if state == "uncertain" => Some(state.clone()),
            Some(_) => return None,
        };
        let account_id = match map.get("accountId") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
            Some(_) => return None,
        };
        let owner = map.get("owner").cloned().filter(|v| !v.is_null());
        let refresh_attempt = map.get("refreshAttempt").cloned().filter(|v| !v.is_null());
        Some(Self {
            access_token,
            access_token_expires_at,
            refresh_token,
            device,
            signing_private_key,
            sealing_private_key,
            account_id,
            owner,
            refresh_state,
            refresh_attempt,
            raw: map.clone(),
        })
    }

    pub fn parse_adopt_input(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let mut parsed = Self::parse(&value)?;
        parsed.refresh_state = None;
        parsed.refresh_attempt = None;
        parsed.raw.remove("refreshState");
        parsed.raw.remove("refreshAttempt");
        Some(parsed)
    }

    pub fn to_value(&self) -> Value {
        let mut map = self.raw.clone();
        map.insert(
            "accessToken".into(),
            Value::String(self.access_token.clone()),
        );
        map.insert(
            "accessTokenExpiresAt".into(),
            Value::String(self.access_token_expires_at.clone()),
        );
        map.insert(
            "refreshToken".into(),
            Value::String(self.refresh_token.clone()),
        );
        map.insert(
            "device".into(),
            json!({ "id": self.device.id, "name": self.device.name }),
        );
        map.insert(
            "signingPrivateKey".into(),
            Value::String(self.signing_private_key.clone()),
        );
        map.insert(
            "sealingPrivateKey".into(),
            Value::String(self.sealing_private_key.clone()),
        );
        if let Some(account_id) = &self.account_id {
            map.insert("accountId".into(), Value::String(account_id.clone()));
        } else {
            map.remove("accountId");
        }
        if let Some(state) = &self.refresh_state {
            map.insert("refreshState".into(), Value::String(state.clone()));
        } else {
            map.remove("refreshState");
        }
        if let Some(attempt) = &self.refresh_attempt {
            map.insert("refreshAttempt".into(), attempt.clone());
        } else {
            map.remove("refreshAttempt");
        }
        Value::Object(map)
    }

    pub fn with_refresh_state(&self, state: Option<&str>) -> Self {
        let mut next = self.clone();
        next.refresh_state = state.map(str::to_owned);
        if let Some(state) = state {
            next.raw
                .insert("refreshState".into(), Value::String(state.to_owned()));
        } else {
            next.raw.remove("refreshState");
        }
        next
    }

    pub fn effective_account_id(&self) -> Option<&str> {
        self.owner
            .as_ref()
            .and_then(|owner| owner.get("accountId"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .or(self.account_id.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedOutMarker {
    pub device: Option<String>,
    pub at: String,
}

impl SignedOutMarker {
    pub fn to_value(&self) -> Value {
        json!({
            "device": self.device,
            "at": self.at,
        })
    }
}

pub fn credential_id(api: &str) -> String {
    let trimmed = api.trim_end_matches('/');
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in trimmed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn signed_out_marker_path(home: &Path, credential: &str) -> PathBuf {
    home.join(".omo")
        .join("account-state")
        .join(format!("{credential}.signed-out.json"))
}

pub fn read_signed_out_marker(path: &Path) -> Option<SignedOutMarker> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let obj = value.as_object()?;
    let at = obj.get("at")?.as_str()?.to_owned();
    let device = match obj.get("device")? {
        Value::Null => None,
        Value::String(id) => Some(id.clone()),
        _ => return None,
    };
    Some(SignedOutMarker { device, at })
}

pub fn write_signed_out_marker(path: &Path, device: Option<&str>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        device_service_core::local_api::access::ensure_private_dir(parent)
            .map_err(|error| error.reason)?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let marker = SignedOutMarker {
        device: device.map(str::to_owned),
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    let text = marker.to_value().to_string();
    #[cfg(unix)]
    {
        device_service_core::local_api::access::write_secret_file(path, &text)
            .map_err(|error| error.reason)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text).map_err(|error| error.to_string())
    }
}

pub fn clear_signed_out_marker(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountStoreError {
    KeychainLocked(String),
    KeychainUnavailable(String),
    StoreUnavailable(String),
    InvalidState(String),
    Other(String),
}

impl AccountStoreError {
    pub fn message(&self) -> &str {
        match self {
            Self::KeychainLocked(msg)
            | Self::KeychainUnavailable(msg)
            | Self::StoreUnavailable(msg)
            | Self::InvalidState(msg)
            | Self::Other(msg) => msg,
        }
    }
}

pub trait AccountCredentialStore: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Result<Option<Credentials>, AccountStoreError>>;
    fn write(&self, value: &Credentials) -> BoxFuture<'_, Result<(), AccountStoreError>>;
    fn clear(&self) -> BoxFuture<'_, Result<(), AccountStoreError>>;
    fn storage(&self) -> Option<&'static str> {
        None
    }
}

pub struct FileAccountCredentialStore {
    path: PathBuf,
}

impl FileAccountCredentialStore {
    pub fn new(home: &Path, api: &str) -> Self {
        let dir = home.join(".omo").join("account-state");
        let name = format!("omo-cli-{}.json", credential_id(api));
        Self {
            path: dir.join(name),
        }
    }
}

impl AccountCredentialStore for FileAccountCredentialStore {
    fn read(&self) -> BoxFuture<'_, Result<Option<Credentials>, AccountStoreError>> {
        let path = self.path.clone();
        Box::pin(async move {
            #[cfg(unix)]
            if path.exists() {
                use device_service_core::local_api::access::{
                    EntryKind, current_uid, private_entry,
                };
                if private_entry(&path, EntryKind::File, current_uid(), true).is_err() {
                    return Ok(None);
                }
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(AccountStoreError::StoreUnavailable(error.to_string())),
            };
            let value: Value = serde_json::from_str(&text).map_err(|_| {
                AccountStoreError::InvalidState(
                    "Stored sign-in data is invalid. Run mhc account forget, then mhc login."
                        .into(),
                )
            })?;
            Credentials::parse(&value).map(Some).ok_or_else(|| {
                AccountStoreError::InvalidState(
                    "Stored sign-in data is invalid. Run mhc account forget, then mhc login."
                        .into(),
                )
            })
        })
    }

    fn write(&self, value: &Credentials) -> BoxFuture<'_, Result<(), AccountStoreError>> {
        let path = self.path.clone();
        let serialized = value.to_value().to_string();
        Box::pin(async move {
            let Some(parent) = path.parent() else {
                return Err(AccountStoreError::StoreUnavailable(
                    "missing state dir".into(),
                ));
            };
            #[cfg(unix)]
            {
                device_service_core::local_api::access::ensure_private_dir(parent)
                    .map_err(|error| AccountStoreError::StoreUnavailable(error.reason))?;
                device_service_core::local_api::access::write_secret_file(&path, &serialized)
                    .map_err(|error| AccountStoreError::StoreUnavailable(error.reason))
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir_all(parent)
                    .map_err(|error| AccountStoreError::StoreUnavailable(error.to_string()))?;
                std::fs::write(&path, serialized)
                    .map_err(|error| AccountStoreError::StoreUnavailable(error.to_string()))
            }
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), AccountStoreError>> {
        let path = self.path.clone();
        Box::pin(async move {
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(AccountStoreError::StoreUnavailable(error.to_string())),
            }
        })
    }

    fn storage(&self) -> Option<&'static str> {
        Some("file")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardCause {
    Refused,
    Forget,
    Logout,
    Replaced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscardedCredential {
    Credential {
        device_id: String,
        account_id: Option<String>,
        cause: DiscardCause,
    },
    Unreadable,
}

pub type BeforeKeysDiscarded = Arc<dyn Fn(DiscardedCredential) + Send + Sync>;

fn has_live_trust_anchor(pin_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(pin_dir.join("trust-anchor")) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|entry| !entry.file_name().to_string_lossy().contains(".retired-"))
}

pub fn retire_sync_pin_on_discard(pin_dir: PathBuf, log: LineSink) -> BeforeKeysDiscarded {
    Arc::new(move |discarded: DiscardedCredential| match discarded {
        DiscardedCredential::Unreadable => {
            if has_live_trust_anchor(&pin_dir) {
                log(
                    "memory sync: a stored sign-in could not be read when its keys were discarded; its sync pin was left in place",
                );
            }
        }
        DiscardedCredential::Credential {
            account_id: None, ..
        } => {
            if has_live_trust_anchor(&pin_dir) {
                log(
                    "memory sync: the account of this sign-in is unknown, so a sync pin was kept; if it belongs to this sign-in's account, clear it with `mhc account retire-sync --account <id>`. Do not retire the pin of an account that is still signed in here, such as the desktop's.",
                );
            }
        }
        DiscardedCredential::Credential {
            account_id: Some(account),
            ..
        } => {
            if let Err(error) = forget_trust_anchor(&pin_dir, &account) {
                log(&format!(
                    "memory sync: the sync pin could not be retired ({error}); it was left in place"
                ));
            }
        }
    })
}
