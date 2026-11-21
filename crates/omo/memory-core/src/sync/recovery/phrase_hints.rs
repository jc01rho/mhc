use super::{RecoveryError, ports::RecoveryTransport, salt_inputs::MAX_RECOVERY_KEYS};
use serde_json::Value;
use std::collections::BTreeSet;

/// A still-enrolled phrase key as the served rows state it, read BEFORE verification: a hint only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseRecoveryHint {
    pub credential_id: String,
    pub enrollment_nonce: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServedPhraseHints {
    Hints(Vec<PhraseRecoveryHint>),
    Refused(PhraseHintRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseHintRefusal {
    Unavailable,
    TooManyRecoveryKeys,
}

fn text(value: &Value, key: &str) -> Option<&str> {
    value[key].as_str().filter(|text| !text.is_empty())
}

fn served_key(value: &Value) -> Option<(String, Option<PhraseRecoveryHint>)> {
    let credential_id = text(value, "credential_id")?.to_owned();
    let enrollment_nonce = text(value, "enrollment_nonce");
    let sealing = text(value, "sealing_public_key");
    let signing = text(value, "signing_public_key");
    let phrase = value["survivable"] == "phrase";
    let hint = match (phrase, enrollment_nonce, sealing, signing) {
        (true, Some(enrollment_nonce), Some(sealing), Some(signing)) => Some(PhraseRecoveryHint {
            credential_id: credential_id.clone(),
            enrollment_nonce: enrollment_nonce.into(),
            sealing_public_key: sealing.into(),
            signing_public_key: signing.into(),
        }),
        _ => None,
    };
    Some((credential_id, hint))
}

fn key_of_row(row: &Value) -> Option<(String, Option<PhraseRecoveryHint>)> {
    let body = &row["body"];
    match body["kind"].as_str() {
        Some("genesis") => served_key(&body["bootstrap"]["recovery_key"]),
        Some("recovery-key") => served_key(body),
        _ => None,
    }
}

/// The phrase-kind keys a typed phrase is derived against (at most `MAX_RECOVERY_KEYS` enrolled keys
/// of BOTH kinds, removals excluded, the same bound as the PRF salts). A wrong hint only yields keys
/// that fail the anchored walk.
pub fn parse_phrase_hints(rows: &Value) -> Result<Vec<PhraseRecoveryHint>, PhraseHintRefusal> {
    let rows = rows.as_array().ok_or(PhraseHintRefusal::Unavailable)?;
    let mut found: Vec<(String, Option<PhraseRecoveryHint>)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let body = &row["body"];
        if body["kind"] == "recovery-key-removed"
            && let Some(credential) = text(body, "credential_id")
        {
            removed.insert(credential.into());
        }
        if let Some((credential, hint)) = key_of_row(row)
            && seen.insert(credential.clone())
        {
            found.push((credential, hint));
        }
    }
    found.retain(|(credential, _)| !removed.contains(credential));
    if found.len() > MAX_RECOVERY_KEYS {
        return Err(PhraseHintRefusal::TooManyRecoveryKeys);
    }
    Ok(found.into_iter().filter_map(|(_, hint)| hint).collect())
}

pub async fn served_phrase_hints(
    transport: &dyn RecoveryTransport,
) -> Result<ServedPhraseHints, RecoveryError> {
    Ok(match parse_phrase_hints(&transport.list_keys().await?) {
        Ok(hints) => ServedPhraseHints::Hints(hints),
        Err(reason) => ServedPhraseHints::Refused(reason),
    })
}
