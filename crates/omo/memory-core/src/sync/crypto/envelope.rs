use super::entry::*;
use serde_json::Value;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub fn exact(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
}
pub fn integer(value: &Value, min: u64) -> Option<u64> {
    value
        .as_u64()
        .filter(|n| *n >= min && *n <= MAX_SAFE_INTEGER)
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.is_finite() && n.fract() == 0.0)
                .and_then(|n| n.to_string().parse::<u64>().ok())
                .filter(|n| *n >= min && *n <= MAX_SAFE_INTEGER)
        })
}
pub fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
fn within_depth(value: &Value) -> bool {
    let mut stack = vec![(value, 1)];
    while let Some((node, depth)) = stack.pop() {
        match node {
            Value::Array(a) => {
                if depth > 64 {
                    return false;
                }
                stack.extend(a.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(o) => {
                if depth > 64 {
                    return false;
                }
                stack.extend(o.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    true
}
const COMMON: &[&str] = &[
    "seq",
    "base_seq",
    "kind",
    "account",
    "epoch",
    "author",
    "digest",
    "link",
    "keyring_link",
    "record_sig",
];

/// The contract's batch id (its `readBatchId`): 16 bytes as unpadded base64url (#12292).
fn is_batch_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| {
        id.len() == 22
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// Closed transport parsing happens before admission can access any fields.
///
/// A batch row may also carry the contract's `batch_id`, which `/v1/sync/entries` served before the
/// service dropped it there. It is checked for shape here and never read: the sealed header's id is
/// bound to the `/v1/sync/batches` row where the object opens (`engine/receive.rs`), and the chain
/// never opens objects. A present `batch_id` that is null or malformed is refused, not ignored.
pub fn parse_entry(row: &Value) -> Result<SyncEntry, &'static str> {
    let bad = "bad_envelope";
    if !row.is_object() || !within_depth(row) {
        return Err(bad);
    }
    let batch = row["kind"] == "batch";
    if batch && row.get("body").is_some() {
        return Err("batch_with_body");
    }
    let mut keys = COMMON.to_vec();
    if !batch {
        if row["kind"] != "keyring" {
            return Err(bad);
        }
        keys.extend(["body", "wraps"]);
    }
    let closed = exact(row, &keys)
        || (batch && {
            let mut with_id = keys.clone();
            with_id.push("batch_id");
            exact(row, &with_id) && is_batch_id(&row["batch_id"])
        });
    if !closed {
        return Err(bad);
    }
    let seq = integer(&row["seq"], 1).ok_or(bad)?;
    let fields = SyncRecordFields {
        account: text(&row["account"]).ok_or(bad)?.into(),
        base_seq: integer(&row["base_seq"], 0).ok_or(bad)?,
        epoch: integer(&row["epoch"], 1).ok_or(bad)?,
        digest: text(&row["digest"]).ok_or(bad)?.into(),
        link: text(&row["link"]).ok_or(bad)?.into(),
        keyring_link: if batch {
            if !row["keyring_link"].is_null() {
                return Err(bad);
            }
            None
        } else {
            Some(text(&row["keyring_link"]).ok_or(bad)?.into())
        },
    };
    let a = &row["author"];
    let author = match a["kind"].as_str() {
        Some("device")
            if exact(
                a,
                &["kind", "id", "sealing_public_key", "signing_public_key"],
            ) =>
        {
            SyncAuthor::Device {
                id: text(&a["id"]).ok_or(bad)?.into(),
                sealing_public_key: text(&a["sealing_public_key"]).ok_or(bad)?.into(),
                signing_public_key: text(&a["signing_public_key"]).ok_or(bad)?.into(),
            }
        }
        Some("recovery") if exact(a, &["kind", "credential_id"]) => SyncAuthor::Recovery {
            credential_id: text(&a["credential_id"]).ok_or(bad)?.into(),
        },
        _ => return Err(bad),
    };
    let record_sig = text(&row["record_sig"]).ok_or(bad)?.into();
    if batch {
        return Ok(SyncEntry::Batch(BatchEntry {
            seq,
            fields,
            author,
            record_sig,
        }));
    }
    if !row["body"].is_object() {
        return Err(bad);
    }
    let wraps = row["wraps"]
        .as_array()
        .ok_or(bad)?
        .iter()
        .map(|w| {
            if !exact(w, &["epoch", "id", "wrap"]) {
                return Err(bad);
            }
            Ok(WrapEntry {
                epoch: integer(&w["epoch"], 1).ok_or(bad)?,
                id: text(&w["id"]).ok_or(bad)?.into(),
                wrap: text(&w["wrap"]).ok_or(bad)?.into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncEntry::Keyring(KeyringEntry {
        seq,
        fields,
        author,
        record_sig,
        body: row["body"].clone(),
        wraps,
    }))
}

pub fn envelope_refusal(entry: &SyncEntry) -> Option<&'static str> {
    parse_entry(&entry.wire()).err()
}

#[cfg(test)]
mod tests {
    use super::parse_entry;
    use serde_json::{Value, json};

    fn batch_row() -> Value {
        json!({
            "seq": 2, "base_seq": 1, "kind": "batch", "account": "acct", "epoch": 1,
            "author": {"kind": "device", "id": "d1", "sealing_public_key": "s", "signing_public_key": "g"},
            "digest": "dg", "link": "ln", "keyring_link": null, "record_sig": "sig"
        })
    }

    fn with(mut row: Value, key: &str, value: Value) -> Value {
        if let Some(object) = row.as_object_mut() {
            object.insert(key.to_string(), value);
        }
        row
    }

    #[test]
    fn a_batch_row_with_or_without_a_well_formed_batch_id_parses() {
        assert!(parse_entry(&batch_row()).is_ok());
        let with_id = with(batch_row(), "batch_id", json!("AAAAAAAAAAAAAAAAAAAA-_"));
        assert!(parse_entry(&with_id).is_ok());
    }

    #[test]
    fn a_null_or_malformed_batch_id_is_refused() {
        for bad in [
            json!(null),
            json!("short"),
            json!("AAAAAAAAAAAAAAAAAAAAA="),
            json!("AAAAAAAAAAAAAAAAAAAAAAA"),
            json!(7),
        ] {
            assert_eq!(
                parse_entry(&with(batch_row(), "batch_id", bad.clone())).err(),
                Some("bad_envelope"),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_keyring_row_carrying_a_batch_id_is_refused() {
        let keyring = json!({
            "seq": 1, "base_seq": 0, "kind": "keyring", "account": "acct", "epoch": 1,
            "author": {"kind": "recovery", "credential_id": "c"},
            "digest": "dg", "link": "ln", "keyring_link": "kl", "record_sig": "sig",
            "body": {}, "wraps": []
        });
        assert!(parse_entry(&keyring).is_ok());
        let with_id = with(keyring, "batch_id", json!("AAAAAAAAAAAAAAAAAAAAAA"));
        assert_eq!(parse_entry(&with_id).err(), Some("bad_envelope"));
    }
}
