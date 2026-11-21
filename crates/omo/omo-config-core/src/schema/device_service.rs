//! Port of `schema/device-service.ts` (omo #12051, shipped in #13563).
//!
//! `device_service`: the device service bootstrap an interactive start runs. On by default;
//! `enabled: false` skips it entirely, with no install and no connection. Whether this account's
//! other devices may control this one is the service's own attach setting, not this key.

use serde_json::{Value, json};

use crate::internal::validate::{Node, boolean, defaulted, optional, strict_object};

pub fn omo_device_service_settings_layer_schema() -> Node {
    strict_object(vec![optional("enabled", boolean())])
}

pub fn omo_device_service_settings_schema() -> Node {
    strict_object(vec![defaulted("enabled", boolean(), || json!(true))])
}

/// `isOmoDeviceServiceEnabled`: absent block or key reads as enabled.
pub fn is_omo_device_service_enabled(config: &Value) -> bool {
    config
        .get("device_service")
        .and_then(|settings| settings.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
