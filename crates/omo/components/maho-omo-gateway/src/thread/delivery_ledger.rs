//! The sender device a delivery's ledger key names (#14305).
//!
//! Upstream counterpart: `thread/delivery-ledger.ts`. Both readers of a ledger key (the intake admission check and the
//! wake carrier) take the device from this one function, so the engine's URI-encoded key and the store's plain key
//! name the same device.

const LEDGER_DEVICE: &str = "device:";

/// `device:<device>/<id>` (amendment A4.3.1 of #13037). The engine's key for a live arrival URI-encodes the device
/// part; the store's key for a delivery taken in from another device writes the device id, which encodes to itself.
/// `rpc:local/<id>`, a bare row id or an empty device part name no device. A device part that is not valid URI encoding
/// is kept as written: it names no listed device, so whatever looks it up misses.
pub fn ledger_device(id: &str) -> Option<String> {
    let rest = id.strip_prefix(LEDGER_DEVICE)?;
    let part = rest.split_once('/').map_or(rest, |(device, _)| device);
    if part.is_empty() {
        return None;
    }
    Some(decode_uri_component(part).unwrap_or_else(|| part.to_owned()))
}

/// `decodeURIComponent`: every `%XX` is one byte and the bytes must read as UTF-8; anything else is not URI encoding.
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::ledger_device;

    #[test]
    fn a_ledger_key_names_its_sender_device_or_none() {
        let cases = [
            (
                "the gateway store's key",
                "device:b7c41e09d2a64f1e/d-1",
                Some("b7c41e09d2a64f1e"),
            ),
            (
                "the engine's URI-encoded key",
                "device:dev%20b/d-1",
                Some("dev b"),
            ),
            (
                "a device part with no id after it",
                "device:b7c41e09d2a64f1e",
                Some("b7c41e09d2a64f1e"),
            ),
            (
                "a device part that is not valid URI encoding",
                "device:dev%zz/d-1",
                Some("dev%zz"),
            ),
            ("an empty device part", "device:/d-1", None),
            ("this device's own key", "rpc:local/d-1", None),
            ("a bare row id", "d-1", None),
        ];
        for (case, id, device) in cases {
            assert_eq!(ledger_device(id).as_deref(), device, "{case}");
        }
    }
}
