//! Canonical JSON and its SHA-256, so a keyring append's digest equals the one the client signed.
//!
//! The client computes `sha256_b64(canonical(record))` before it signs a keyring record, and the
//! chain it verifies links entries by that digest. The server therefore canonicalizes the record
//! it received the same way: object keys sorted by UTF-16 code units, minimal string escapes, and
//! numbers serialized as the sync format's safe integers.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// SHA-256 of the canonical form, as 43-character base64url (no padding).
pub fn canonical_digest(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonicalize(value)))
}

/// SHA-256 of raw bytes, as 43-character base64url (no padding).
pub fn digest_of(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

/// RFC 8785-shaped serialization: sorted keys, minimal escapes, safe integers.
pub fn canonicalize(value: &Value) -> Vec<u8> {
    let mut out = String::new();
    write_value(value, &mut out);
    out.into_bytes()
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_string(text, out),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(value, out);
            }
            out.push(']');
        }
        Value::Object(values) => {
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = values.iter().collect();
            entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(value, out);
            }
            out.push('}');
        }
    }
}

fn write_string(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            control if control < '\u{20}' => {
                let code = usize::from(u8::try_from(u32::from(control)).unwrap_or(0));
                out.push_str("\\u00");
                out.push(char::from(HEX[code / 16]));
                out.push(char::from(HEX[code % 16]));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
