//! This device's identity: an Ed25519 key kept private in the state dir. The device id is derived
//! from the public key, so a peer list needs only names, addresses and public keys.

use std::fmt;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use device_service_core::local_api::LocalApiError;
use device_service_core::local_api::proof::random_bytes;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const IDENTITY_FILE: &str = "device.key";

#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    v: u32,
    name: String,
    seed: String,
}

#[derive(Clone)]
pub struct DeviceIdentity {
    name: String,
    signing: SigningKey,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("id", &self.id())
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// `dev_` and the first 20 base64url characters of the public key's SHA-256.
pub fn device_id_of(public_key: &[u8]) -> String {
    let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(public_key));
    format!("dev_{}", &digest[..20])
}

pub fn decode_public_key(text: &str) -> Option<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(text.trim()).ok()?;
    let array: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&array).ok()
}

/// A strict Ed25519 check of `signature` (base64url) over `message`.
pub fn verify_signature(public_key: &VerifyingKey, message: &[u8], signature: &str) -> bool {
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(signature) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&bytes) else {
        return false;
    };
    public_key.verify_strict(message, &signature).is_ok()
}

pub fn default_device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    if name.trim().is_empty() {
        "mhc device".into()
    } else {
        name
    }
}

fn internal(error: impl fmt::Display) -> LocalApiError {
    LocalApiError::new("internal", error.to_string())
}

impl DeviceIdentity {
    pub fn from_seed(name: impl Into<String>, seed: [u8; 32]) -> Self {
        Self {
            name: name.into(),
            signing: SigningKey::from_bytes(&seed),
        }
    }

    pub fn generate(name: impl Into<String>) -> Result<Self, LocalApiError> {
        Ok(Self::from_seed(name, random_bytes::<32>()?))
    }

    pub fn id(&self) -> String {
        device_id_of(&self.signing.verifying_key().to_bytes())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The public key as base64url, what a peer list names this device by.
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.sign(message).to_bytes())
    }

    /// The identity in `state_dir`, `None` when there is none yet.
    pub fn load(state_dir: &Path) -> Result<Option<Self>, LocalApiError> {
        let path = state_dir.join(IDENTITY_FILE);
        #[cfg(unix)]
        {
            use device_service_core::local_api::access::{EntryKind, current_uid, private_entry};
            if path.exists() {
                private_entry(&path, EntryKind::File, current_uid(), true)?;
            }
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(LocalApiError::new("read_failed", error.to_string())),
        };
        let stored: StoredIdentity = serde_json::from_str(&text).map_err(internal)?;
        let seed: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&stored.seed)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| internal("the device key is malformed"))?;
        Ok(Some(Self::from_seed(stored.name, seed)))
    }

    /// Writes the identity into `state_dir`, readable by this user only.
    pub fn save(&self, state_dir: &Path) -> Result<(), LocalApiError> {
        let stored = StoredIdentity {
            v: 1,
            name: self.name.clone(),
            seed: URL_SAFE_NO_PAD.encode(self.signing.to_bytes()),
        };
        let text = serde_json::to_string(&stored).map_err(internal)?;
        let path = state_dir.join(IDENTITY_FILE);
        #[cfg(unix)]
        {
            device_service_core::local_api::access::ensure_private_dir(state_dir)?;
            device_service_core::local_api::access::write_secret_file(&path, &text)
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(state_dir).map_err(internal)?;
            std::fs::write(&path, text).map_err(internal)
        }
    }

    pub fn load_or_create(state_dir: &Path, name: Option<&str>) -> Result<Self, LocalApiError> {
        if let Some(identity) = Self::load(state_dir)? {
            return Ok(identity);
        }
        let name = name.map_or_else(default_device_name, str::to_owned);
        let identity = Self::generate(name)?;
        identity.save(state_dir)?;
        Ok(identity)
    }

    /// Forgets the key: the device must be listed again by its peers under a new one.
    pub fn remove(state_dir: &Path) -> Result<bool, LocalApiError> {
        match std::fs::remove_file(state_dir.join(IDENTITY_FILE)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(internal(error)),
        }
    }
}
