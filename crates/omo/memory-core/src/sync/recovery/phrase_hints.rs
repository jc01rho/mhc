//! A still-enrolled phrase key as the served rows state it, read BEFORE verification: a hint only
//! (contract 6, DI, #13336). A wrong hint only yields keys that fail the anchored walk.

use serde_json::Value;

use super::ports::RecoveryTransport;
use super::salt_inputs::MAX_RECOVERY_KEYS;
use crate::sync::RecoveryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseRecoveryHint {
    pub credential_id: String,
    pub enrollment_nonce: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServedPhraseHints {
    Ok(Vec<PhraseRecoveryHint>),
    Unavailable,
    TooManyRecoveryKeys,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|text| !text.is_empty())
}

struct Served {
    credential_id: String,
    hint: Option<PhraseRecoveryHint>,
}

fn served_key(value: &Value) -> Option<Served> {
    let credential_id = text(value, "credential_id")?.to_string();
    let nonce = text(value, "enrollment_nonce");
    let sealing = text(value, "sealing_public_key");
    let signing = text(value, "signing_public_key");
    let phrase = value["survivable"] == "phrase";
    let hint = match (phrase, nonce, sealing, signing) {
        (true, Some(nonce), Some(sealing), Some(signing)) => Some(PhraseRecoveryHint {
            credential_id: credential_id.clone(),
            enrollment_nonce: nonce.to_string(),
            sealing_public_key: sealing.to_string(),
            signing_public_key: signing.to_string(),
        }),
        _ => None,
    };
    Some(Served {
        credential_id,
        hint,
    })
}

fn key_of_row(row: &Value) -> Option<Served> {
    let body = &row["body"];
    match body["kind"].as_str() {
        Some("genesis") => served_key(&body["bootstrap"]["recovery_key"]),
        Some("recovery-key") => served_key(body),
        _ => None,
    }
}

/// The phrase-kind keys a typed phrase is derived against (at most [`MAX_RECOVERY_KEYS`] enrolled
/// keys of both kinds, removals excluded, the same bound as the PRF salts).
pub async fn served_phrase_hints(
    transport: &dyn RecoveryTransport,
) -> Result<ServedPhraseHints, RecoveryError> {
    let rows = transport.list_keys().await?;
    let Some(rows) = rows.as_array() else {
        return Ok(ServedPhraseHints::Unavailable);
    };
    let mut found: Vec<Served> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for row in rows {
        let body = &row["body"];
        if body["kind"] == "recovery-key-removed"
            && let Some(credential) = text(body, "credential_id")
        {
            removed.push(credential.to_string());
        }
        if let Some(served) = key_of_row(row)
            && !found
                .iter()
                .any(|known| known.credential_id == served.credential_id)
        {
            found.push(served);
        }
    }
    found.retain(|served| !removed.contains(&served.credential_id));
    if found.len() > MAX_RECOVERY_KEYS {
        return Ok(ServedPhraseHints::TooManyRecoveryKeys);
    }
    Ok(ServedPhraseHints::Ok(
        found.into_iter().filter_map(|served| served.hint).collect(),
    ))
}
