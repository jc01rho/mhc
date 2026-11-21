//! `<state dir>/settings.json`: the switches the local API sets and that outlive a restart.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SETTINGS_FILE: &str = "settings.json";

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switch {
    pub value: bool,
    pub changed_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeSwitch {
    /// `None`: nothing stored, the build default applies.
    pub value: Option<bool>,
    pub changed_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// `service.set_enabled`: off keeps the service running but disconnected from its peers.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `device.forget` ran: the service reports `signed_out` and serves no peer.
    #[serde(default)]
    pub forgotten: bool,
    /// `control.attach.*`: whether other devices may view and command this device's sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Switch>,
    /// `control.session_wake.*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeSwitch>,
    /// The agent dirs `endpoints.register` named.
    #[serde(default)]
    pub endpoints: BTreeSet<String>,
    /// The file existed but did not parse; the next set rewrites it.
    #[serde(skip)]
    pub unreadable: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            forgotten: false,
            control: None,
            wake: None,
            endpoints: BTreeSet::new(),
            unreadable: false,
        }
    }
}

/// Waking a session that is not running is on unless turned off (omo#13637).
pub const WAKE_DEFAULT: bool = true;

impl Settings {
    pub fn load(state_dir: &Path) -> Self {
        match std::fs::read_to_string(state_dir.join(SETTINGS_FILE)) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| Self {
                unreadable: true,
                ..Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self {
                unreadable: true,
                ..Self::default()
            },
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        let temp = state_dir.join(format!("{SETTINGS_FILE}.{}.tmp", std::process::id()));
        let written = std::fs::write(&temp, text + "\n")
            .and_then(|()| std::fs::rename(&temp, state_dir.join(SETTINGS_FILE)));
        if written.is_err() {
            let _cleanup = std::fs::remove_file(&temp);
        }
        written.map_err(|error| error.to_string())
    }

    pub fn control_attached(&self) -> bool {
        self.control.as_ref().is_none_or(|switch| switch.value)
    }

    /// `{attached, changedAt}`; a never-set switch reads attached with `changedAt` "".
    pub fn control_view(&self) -> Value {
        json!({
            "attached": self.control_attached(),
            "changedAt": self.control.as_ref().map_or("", |switch| switch.changed_at.as_str()),
        })
    }

    /// `{enabled, stored, changedAt, overridden}`. `overridden`: this service cannot start a session
    /// that is not running, whatever the switch says.
    pub fn wake_view(&self) -> Value {
        let stored = if self.unreadable {
            Value::String("unreadable".into())
        } else {
            self.wake
                .as_ref()
                .and_then(|switch| switch.value)
                .map_or(Value::Null, Value::Bool)
        };
        json!({
            "enabled": self.wake.as_ref().and_then(|switch| switch.value).unwrap_or(WAKE_DEFAULT),
            "stored": stored,
            "changedAt": self.wake.as_ref().map_or("", |switch| switch.changed_at.as_str()),
            "overridden": true,
        })
    }
}
