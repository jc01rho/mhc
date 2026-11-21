//! Port of `local-api/proof.ts`: nonces, ids and the HMAC-SHA256 proofs both sides exchange.
//!
//! `server_proof = base64url(HMAC-SHA256(token, "server" || client_nonce || endpoint))` and
//! `client_proof = base64url(HMAC-SHA256(token, "client" || server_nonce || endpoint))`, over the
//! nonce's 32 raw bytes followed by the UTF-8 endpoint (`unix`, or `tcp:<port>` on Windows).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::errors::LocalApiError;

pub const NONCE_BYTES: usize = 32;

pub fn random_bytes<const N: usize>() -> Result<[u8; N], LocalApiError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
    Ok(bytes)
}

pub fn mint_nonce() -> Result<String, LocalApiError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<NONCE_BYTES>()?))
}

/// 16 random bytes as 22 base64url characters: request ids, attach handles and command ids.
pub fn mint_id() -> Result<String, LocalApiError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes::<16>()?))
}

/// Exactly `bytes` bytes in canonical unpadded base64url; anything else is a protocol violation.
pub fn decoded_bytes(value: &str, bytes: usize, what: &str) -> Result<Vec<u8>, LocalApiError> {
    let violation =
        || LocalApiError::protocol_violation(format!("{what} is not {bytes} base64url bytes"));
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| violation())?;
    if decoded.len() != bytes || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(violation());
    }
    Ok(decoded)
}

fn proof(token: &[u8], label: &str, nonce: &str, endpoint: &str) -> Result<String, LocalApiError> {
    let nonce = decoded_bytes(nonce, NONCE_BYTES, "a nonce")?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(token)
        .map_err(|error| LocalApiError::new("internal", error.to_string()))?;
    mac.update(label.as_bytes());
    mac.update(&nonce);
    mac.update(endpoint.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

pub fn server_proof(
    token: &[u8],
    client_nonce: &str,
    endpoint: &str,
) -> Result<String, LocalApiError> {
    proof(token, "server", client_nonce, endpoint)
}

pub fn client_proof(
    token: &[u8],
    server_nonce: &str,
    endpoint: &str,
) -> Result<String, LocalApiError> {
    proof(token, "client", server_nonce, endpoint)
}

/// Constant-time comparison of two proofs.
pub fn proof_matches(expected: &str, actual: &str) -> bool {
    let (expected, actual) = (expected.as_bytes(), actual.as_bytes());
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .fold(0u8, |acc, (left, right)| acc | (left ^ right))
            == 0
}
