//! What the local server and the peer link share: this device, its switches, its ledger, and the
//! status every `status.subscribe` follows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config::{PeerConfig, RelaydConfig};
use crate::identity::DeviceIdentity;
use crate::ledger::CommandLedger;
use crate::session_host::SessionHost;
use crate::settings::Settings;
use crate::wire::now_ms;

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

pub struct RelaydState {
    pub identity: DeviceIdentity,
    pub config: RelaydConfig,
    pub state_dir: PathBuf,
    pub service_version: String,
    pub host: Arc<dyn SessionHost>,
    /// This boot's local API token.
    pub token: Vec<u8>,
    settings: Mutex<Settings>,
    ledger: Mutex<CommandLedger>,
    peer_seen: Mutex<HashMap<String, i64>>,
    status: watch::Sender<Value>,
    since_ms: AtomicI64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RelaydState {
    pub fn new(
        identity: DeviceIdentity,
        config: RelaydConfig,
        state_dir: PathBuf,
        service_version: String,
        host: Arc<dyn SessionHost>,
        token: Vec<u8>,
    ) -> Self {
        let settings = Settings::load(&state_dir);
        let (status, _) = watch::channel(Value::Null);
        let state = Self {
            identity,
            config,
            state_dir,
            service_version,
            host,
            token,
            settings: Mutex::new(settings),
            ledger: Mutex::new(CommandLedger::default()),
            peer_seen: Mutex::new(HashMap::new()),
            status,
            since_ms: AtomicI64::new(now_ms()),
        };
        state.status.send_replace(state.status());
        state
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Applies `change`, persists the switches and publishes the status when it changed.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let updated = {
            let mut settings = lock(&self.settings);
            let before = self.state_of(&settings);
            change(&mut settings);
            settings.unreadable = false;
            settings.save(&self.state_dir)?;
            if self.state_of(&settings) != before {
                self.since_ms.store(now_ms(), Ordering::SeqCst);
            }
            settings.clone()
        };
        self.status.send_replace(self.status());
        Ok(updated)
    }

    pub fn ledger(&self) -> MutexGuard<'_, CommandLedger> {
        lock(&self.ledger)
    }

    fn state_of(&self, settings: &Settings) -> &'static str {
        if settings.forgotten {
            "signed_out"
        } else if !settings.enabled {
            "disabled"
        } else {
            "connected"
        }
    }

    /// The local API `Status`.
    pub fn status(&self) -> Value {
        let settings = self.settings();
        let device = if settings.forgotten {
            Value::Null
        } else {
            self.this_device()
        };
        json!({
            "state": self.state_of(&settings),
            "since_ms": self.since_ms.load(Ordering::SeqCst),
            "device": device,
            "origin_configured": true,
            "enabled": settings.enabled,
        })
    }

    pub fn status_receiver(&self) -> watch::Receiver<Value> {
        self.status.subscribe()
    }

    pub fn this_device(&self) -> Value {
        json!({ "id": self.identity.id(), "name": self.identity.name() })
    }

    /// Whether this device takes part in the account: enabled and not signed out.
    pub fn serving(&self) -> bool {
        let settings = self.settings();
        settings.enabled && !settings.forgotten
    }

    pub fn control_attached(&self) -> bool {
        self.settings().control_attached()
    }

    pub fn agent_dirs(&self) -> Vec<PathBuf> {
        self.settings()
            .endpoints
            .iter()
            .map(PathBuf::from)
            .collect()
    }

    pub fn find_peer(&self, device: &str) -> Option<PeerConfig> {
        self.config.find_peer(device).cloned()
    }

    pub fn record_peer_seen(&self, device: &str) {
        lock(&self.peer_seen).insert(device.to_owned(), now_ms());
    }

    pub fn peer_last_seen(&self, device: &str) -> Option<i64> {
        lock(&self.peer_seen).get(device).copied()
    }
}
