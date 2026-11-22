use super::{
    RecoveryError,
    ports::{RecoverySaltInput, RecoveryTransport},
};
use crate::sync::crypto::recovery_kind::RESERVED_RP_PREFIX;
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_RECOVERY_KEYS: usize = 16;
#[derive(Debug, PartialEq, Eq)]
pub enum SaltRefusal {
    Unavailable,
    TooManyRecoveryKeys,
}
/// A phrase key, or anything in the reserved `omo-sync/` rp namespace, is never a PRF salt input
/// (contract F12): the authenticator would mint a PRF output for a credential that has no passkey.
fn prf_capable(key: &serde_json::Value) -> bool {
    key["survivable"] != "phrase"
        && !key["rp_id"]
            .as_str()
            .is_some_and(|rp_id| rp_id.starts_with(RESERVED_RP_PREFIX))
}
pub fn parse_salt_inputs(keys: &serde_json::Value) -> Result<Vec<RecoverySaltInput>, SaltRefusal> {
    let keys = keys.as_array().ok_or(SaltRefusal::Unavailable)?;
    let mut found: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut order = Vec::new();
    let mut removed = BTreeSet::new();
    for row in keys {
        let body = &row["body"];
        if body["kind"] == "recovery-key-removed"
            && let Some(id) = body["credential_id"].as_str()
        {
            removed.insert(id);
        }
        let key = match body["kind"].as_str() {
            Some("genesis") => &body["bootstrap"]["recovery_key"],
            Some("recovery-key") => body,
            _ => continue,
        };
        if let (Some(credential), Some(nonce)) = (
            key["credential_id"].as_str(),
            key["enrollment_nonce"].as_str(),
        ) && !found.contains_key(credential)
        {
            order.push(credential.to_string());
            found.insert(
                credential.to_string(),
                (nonce.to_string(), prf_capable(key)),
            );
        }
    }
    order.retain(|id| !removed.contains(id.as_str()));
    if order.len() > MAX_RECOVERY_KEYS {
        return Err(SaltRefusal::TooManyRecoveryKeys);
    }
    Ok(order
        .into_iter()
        .filter_map(|id| {
            let (nonce, prf) = found.remove(&id)?;
            prf.then_some(RecoverySaltInput {
                credential_id: id,
                enrollment_nonce: nonce,
            })
        })
        .collect())
}
pub async fn served_salt_inputs(
    transport: &dyn RecoveryTransport,
) -> Result<Result<Vec<RecoverySaltInput>, SaltRefusal>, RecoveryError> {
    Ok(parse_salt_inputs(&transport.list_keys().await?))
}
