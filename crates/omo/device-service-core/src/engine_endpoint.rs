//! Port of `device-service-core/src/engine-endpoint.ts` (`EngineEndpoints`, `engineEndpointsOf`):
//! lists every engine endpoint key a device advertises and the one it names as its primary (0.1.11+).

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::local_api::{LocalApiClient, LocalApiError};
use crate::release::version::compare_versions;

/// The first service version that answers `session.endpoints` with the host's primary engine endpoint.
pub const ENDPOINT_PRIMARY_MIN_SERVICE_VERSION: &str = "0.1.11";

/// Every engine endpoint key `device` lists, and the one it names as its own (0.1.11+). The primary is
/// named by key, never by position: a host may list it last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineEndpoints {
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

/// The engine endpoints a read-only lookup may go to. Asks with `primary: true` where the service
/// knows it, so the caller can skip the primary by its key instead of guessing `endpoints[0]`.
pub async fn engine_endpoints_of(
    client: &LocalApiClient,
    device: &str,
) -> Result<EngineEndpoints, LocalApiError> {
    let asks_primary = compare_versions(
        client.service_version(),
        ENDPOINT_PRIMARY_MIN_SERVICE_VERSION,
    )? != Ordering::Less;
    let params = if asks_primary {
        json!({ "device": device, "primary": true })
    } else {
        json!({ "device": device })
    };
    let listed = client.request("session.endpoints", params).await?;
    let endpoints = listed
        .get("endpoints")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            LocalApiError::protocol_violation("session.endpoints answered without endpoints")
        })?;
    let mut keys = Vec::with_capacity(endpoints.len());
    for entry in endpoints {
        let Some(key) = entry.as_str() else {
            return Err(LocalApiError::protocol_violation(
                "session.endpoints endpoint key must be a string",
            ));
        };
        keys.push(key.to_owned());
    }
    let primary = match listed.get("primary") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(LocalApiError::protocol_violation(
                "session.endpoints primary must be a string",
            ));
        }
    };
    Ok(EngineEndpoints { keys, primary })
}
