//! A still-enrolled phrase key as the served rows state it, read BEFORE verification: a hint only
//! (contract 6, DI, #13336). A wrong hint only yields keys that fail the anchored walk.

use serde_json::Value;

use super::ports::RecoveryTransport;
use super::salt_inputs::MAX_RECOVERY_KEYS;
use crate::sync::recovery::RecoveryError;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::recovery::ports::{AppendResult, KeyringAppendRequest, PortFuture};
    use serde_json::json;

    struct HintTransport(Value);

    impl RecoveryTransport for HintTransport {
        fn list_keys(&self) -> PortFuture<'_, Value> {
            Box::pin(async { Ok(self.0.clone()) })
        }
        fn list_entries(&self) -> PortFuture<'_, Value> {
            Box::pin(async { Ok(Value::Null) })
        }
        fn list_recovery_wraps(&self) -> PortFuture<'_, Value> {
            Box::pin(async { Ok(Value::Null) })
        }
        fn append_keys(&self, _request: KeyringAppendRequest) -> PortFuture<'_, AppendResult> {
            Box::pin(async { Ok(Err(crate::sync::recovery::ports::TransportRefusal::Refused)) })
        }
        fn recovery_join(&self, _request: KeyringAppendRequest) -> PortFuture<'_, AppendResult> {
            Box::pin(async { Ok(Err(crate::sync::recovery::ports::TransportRefusal::Refused)) })
        }
    }

    #[tokio::test]
    async fn served_phrase_hints_keep_served_order_drop_removals_and_bound_both_kinds() {
        let phrase = |id: &str| {
            json!({
                "body": {
                    "kind": "recovery-key",
                    "credential_id": id,
                    "enrollment_nonce": "n",
                    "sealing_public_key": "s",
                    "signing_public_key": "g",
                    "survivable": "phrase",
                }
            })
        };
        let rows = json!([
            {"body":{"kind":"genesis","bootstrap":{"recovery_key":{"credential_id":"passkey","enrollment_nonce":"n","sealing_public_key":"s","signing_public_key":"g","survivable":"synced"}}}},
            phrase("z-phrase"),
            phrase("a-phrase"),
            phrase("z-phrase"),
            {"body":{"kind":"recovery-key-removed","credential_id":"a-phrase"}},
        ]);
        let hints = served_phrase_hints(&HintTransport(rows))
            .await
            .expect("hints");
        assert_eq!(
            hints,
            ServedPhraseHints::Ok(vec![PhraseRecoveryHint {
                credential_id: "z-phrase".into(),
                enrollment_nonce: "n".into(),
                sealing_public_key: "s".into(),
                signing_public_key: "g".into(),
            }])
        );

        let mut mixed: Vec<Value> = (0..MAX_RECOVERY_KEYS)
            .map(|id| {
                json!({
                    "body": {
                        "kind": "recovery-key",
                        "credential_id": format!("passkey-{id}"),
                        "enrollment_nonce": "n",
                        "sealing_public_key": "s",
                        "signing_public_key": "g",
                        "survivable": "synced",
                    }
                })
            })
            .collect();
        mixed.push(phrase("overflow-phrase"));
        assert_eq!(
            served_phrase_hints(&HintTransport(json!(mixed)))
                .await
                .expect("bound"),
            ServedPhraseHints::TooManyRecoveryKeys
        );
        assert_eq!(
            served_phrase_hints(&HintTransport(json!({"rows": []})))
                .await
                .expect("unavailable"),
            ServedPhraseHints::Unavailable
        );
    }
}
