//! Port of `local-api/limits.ts`: the inline, chunk and blob bounds, and the line arithmetic.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// A payload whose line would exceed this many bytes never goes on one line: it is chunked or paged.
pub const LOCAL_INLINE_MAX: usize = 524_288;
/// Raw bytes per chunk; every chunk but the last is exactly this size.
pub const LOCAL_CHUNK_MAX: usize = 262_144;
pub const LOCAL_JSON_EXPANSION_MAX: usize = 6;
/// The largest value the transport between devices carries, as canonical CBOR.
pub const SESSION_BLOB_MAX: usize = 16_777_216;
pub const LOCAL_BLOB_MAX: usize = LOCAL_JSON_EXPANSION_MAX * SESSION_BLOB_MAX;
/// `message_too_long`: a command record over this many UTF-16 code units, or over
/// [`COMMAND_TEXT_MAX_BYTES`] UTF-8 bytes, is refused before anything leaves the device.
pub const COMMAND_TEXT_MAX_CHARS: usize = 1_000_000;
pub const COMMAND_TEXT_MAX_BYTES: usize = 3_000_000;

/// The bytes `value` takes as one line, its LF included.
pub fn line_bytes(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len() + 1)
}

pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// `(off, base64)` per chunk of `data`.
pub fn chunk_payload(data: &[u8]) -> Vec<(usize, String)> {
    data.chunks(LOCAL_CHUNK_MAX)
        .enumerate()
        .map(|(index, chunk)| (index * LOCAL_CHUNK_MAX, STANDARD.encode(chunk)))
        .collect()
}

/// Whether a command record is within the engine's limit (UTF-16 code units and UTF-8 bytes).
pub fn command_text_fits(text: &str) -> bool {
    text.len() <= COMMAND_TEXT_MAX_BYTES && text.encode_utf16().count() <= COMMAND_TEXT_MAX_CHARS
}
