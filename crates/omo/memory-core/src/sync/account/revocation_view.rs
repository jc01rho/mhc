//! What a rotator needs from the account's device list (sync-design section 7, rotation on revoke):
//! which sync principals are on a revoked row, which are listed at all, and which one is this device.
//! Only `current`, `revokedAt` and `syncPrincipal` are read. Every other field is ignored, so a device
//! on a platform or of a kind this client does not know never stops a rotation. A row whose three
//! fields do not parse vouches for nothing; it does not fail the view.

use std::collections::BTreeSet;

use serde_json::Value;

use super::http::{AccountHttp, AccountHttpError};
use crate::sync::engine::error::{EngineError, EngineResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationView {
    Unusable,
    Usable {
        /// The principal on the `current: true` row.
        self_principal: String,
        self_revoked: bool,
        revoked: BTreeSet<String>,
        listed: BTreeSet<String>,
    },
}

pub fn parse_revocation_view(body: &Value) -> RevocationView {
    let Some(rows) = body.get("devices").and_then(Value::as_array) else {
        return RevocationView::Unusable;
    };
    let mut revoked = BTreeSet::new();
    let mut listed = BTreeSet::new();
    let mut own: Option<(String, bool)> = None;
    for row in rows {
        if !row.is_object() {
            continue;
        }
        let Some(principal) = row["syncPrincipal"].as_str().filter(|p| !p.is_empty()) else {
            continue;
        };
        let revoked_at = &row["revokedAt"];
        if !(revoked_at.is_null() || revoked_at.is_string()) {
            continue;
        }
        listed.insert(principal.to_string());
        if !revoked_at.is_null() {
            revoked.insert(principal.to_string());
        }
        if row["current"] == true && own.is_none() {
            own = Some((principal.to_string(), !revoked_at.is_null()));
        }
    }
    match own {
        None => RevocationView::Unusable,
        Some((self_principal, self_revoked)) => RevocationView::Usable {
            self_principal,
            self_revoked,
            revoked,
            listed,
        },
    }
}

/// The view over `GET /v1/devices` for a device holding its own bearer token. The request carries
/// the sync contract headers like every other account read (#13691).
pub async fn read_revocation_view(http: &AccountHttp) -> EngineResult<RevocationView> {
    let (status, body) = http.send("GET", "/v1/devices", None).await?;
    if !(200..300).contains(&status) {
        return Err(EngineError::Account(AccountHttpError {
            code: if status >= 500 {
                "unavailable"
            } else {
                "refused"
            }
            .into(),
            status,
        }));
    }
    Ok(parse_revocation_view(&body))
}
