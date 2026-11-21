//! The recovery phrase (A3 engine contract sections 2-4): 32 bytes of entropy E, shown as 24 BIP39
//! English words. BIP39 is an encoding only, never a PBKDF2 seed: the keys come from HKDF-SHA-256
//! over E, bound to the account and the enrollment nonce. E and the word indices stay bytes until
//! [`RecoveryPhrase::reveal`]; every type that holds E, typed words or derived private keys redacts
//! its `Debug`/`Display` form, so no log, error or serde surface can carry a word (#13330).

use std::fmt;
use std::sync::OnceLock;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, Zeroizing};

use super::RecoveryError;
use super::bip39;
use crate::sync::crypto::signatures::sha256;
use crate::sync::crypto::vendor::sync_format::keys::hkdf_sha256;
use crate::sync::recovery::derive::{RecoveryKeyPairs, x25519_public_key};

const PHRASE_SALT_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-salt";
const PHRASE_SEAL_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-seal";
const PHRASE_SIGN_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-sign";

pub const ENTROPY_BYTES: usize = 32;
pub const WORD_COUNT: usize = 24;
const NONCE_BYTES: usize = 32;

const REDACTED_PHRASE: &str = "[recovery phrase]";
const REDACTED_TYPED: &str = "[typed recovery phrase]";
const REDACTED_KEYS: &str = "[recovery keys]";

fn word_index() -> &'static std::collections::HashMap<&'static str, u16> {
    static INDEX: OnceLock<std::collections::HashMap<&'static str, u16>> = OnceLock::new();
    INDEX.get_or_init(|| {
        bip39::words()
            .into_iter()
            .enumerate()
            .map(|(index, word)| (word, u16::try_from(index).unwrap_or(0)))
            .collect()
    })
}

fn checksum_byte(entropy: &[u8]) -> u8 {
    sha256(entropy).first().copied().unwrap_or(0)
}

/// 24 big-endian 11-bit indices over `E || first byte of SHA-256(E)`.
fn indices_of(entropy: &[u8]) -> [u16; WORD_COUNT] {
    let mut indices = [0u16; WORD_COUNT];
    let mut accumulator: u32 = 0;
    let mut bits: u8 = 0;
    let mut next = 0usize;
    for offset in 0..=ENTROPY_BYTES {
        let byte = if offset < ENTROPY_BYTES {
            entropy.get(offset).copied().unwrap_or(0)
        } else {
            checksum_byte(entropy)
        };
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        if bits >= 11 {
            bits -= 11;
            indices[next] = u16::try_from((accumulator >> bits) & 0x7ff).unwrap_or(0);
            next += 1;
            accumulator &= (1u32 << bits) - 1;
        }
    }
    indices
}

/// The 33 bytes `E || checksum` the 24 indices carry.
fn bytes_of(indices: &[u16; WORD_COUNT]) -> Zeroizing<[u8; ENTROPY_BYTES + 1]> {
    let mut out = Zeroizing::new([0u8; ENTROPY_BYTES + 1]);
    let mut accumulator: u32 = 0;
    let mut bits: u8 = 0;
    let mut next = 0usize;
    for index in indices {
        accumulator = (accumulator << 11) | u32::from(*index);
        bits += 11;
        while bits >= 8 {
            bits -= 8;
            out[next] = u8::try_from((accumulator >> bits) & 0xff).unwrap_or(0);
            next += 1;
        }
        accumulator &= (1u32 << bits) - 1;
    }
    out
}

/// A recovery phrase. E lives in a private field that zeroes on drop; every text form is
/// `[recovery phrase]`. Only the display surface calls [`reveal`](Self::reveal).
pub struct RecoveryPhrase {
    entropy: Zeroizing<[u8; ENTROPY_BYTES]>,
}

impl fmt::Debug for RecoveryPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_PHRASE)
    }
}

impl fmt::Display for RecoveryPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_PHRASE)
    }
}

impl RecoveryPhrase {
    /// The 24 words, for the display surface only.
    pub fn reveal(&self) -> Vec<&'static str> {
        let mut indices = indices_of(&*self.entropy);
        let words = bip39::words();
        let revealed = indices
            .iter()
            .filter_map(|index| words.get(usize::from(*index)).copied())
            .collect();
        indices.zeroize();
        revealed
    }

    /// A fresh phrase: E is 32 new bytes for every enrollment.
    pub fn generate() -> Result<Self, RecoveryError> {
        use aws_lc_rs::rand::{SecureRandom, SystemRandom};
        let mut entropy = Zeroizing::new([0u8; ENTROPY_BYTES]);
        SystemRandom::new()
            .fill(&mut *entropy)
            .map_err(|_| RecoveryError::Crypto)?;
        Ok(Self { entropy })
    }

    /// A phrase over a fixed entropy, for the pinned test vectors only.
    pub fn from_entropy(entropy: [u8; ENTROPY_BYTES]) -> Self {
        Self {
            entropy: Zeroizing::new(entropy),
        }
    }
}

/// Text a person typed as a recovery phrase, redacted the same way; [`parse_recovery_phrase`]
/// consumes it.
pub struct TypedPhrase(String);

impl TypedPhrase {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

impl fmt::Debug for TypedPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_TYPED)
    }
}

impl fmt::Display for TypedPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_TYPED)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseParseFailure {
    WrongWordCount,
    /// `position` is 1-based; the word itself is never reported.
    UnknownWord {
        position: usize,
    },
    ChecksumMismatch,
}

impl fmt::Display for PhraseParseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongWordCount => write!(f, "a recovery phrase is {WORD_COUNT} words"),
            Self::UnknownWord { position } => {
                write!(f, "word {position} is not in the BIP39 English list")
            }
            Self::ChecksumMismatch => f.write_str("the recovery phrase checksum does not match"),
        }
    }
}

impl std::error::Error for PhraseParseFailure {}

/// NFKD, lowercase, trim, and any whitespace run as one separator; then exactly 24 listed words and
/// a matching checksum, all before any derivation can run.
pub fn parse_recovery_phrase(typed: &TypedPhrase) -> Result<RecoveryPhrase, PhraseParseFailure> {
    let normalized: String = typed.0.nfkd().collect::<String>().to_lowercase();
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.len() != WORD_COUNT {
        return Err(PhraseParseFailure::WrongWordCount);
    }
    let index = word_index();
    let mut indices = [0u16; WORD_COUNT];
    for (offset, word) in words.iter().enumerate() {
        match index.get(word) {
            Some(at) => indices[offset] = *at,
            None => {
                indices.zeroize();
                return Err(PhraseParseFailure::UnknownWord {
                    position: offset + 1,
                });
            }
        }
    }
    let packed = bytes_of(&indices);
    indices.zeroize();
    let mut entropy = [0u8; ENTROPY_BYTES];
    entropy.copy_from_slice(&packed[..ENTROPY_BYTES]);
    if packed[ENTROPY_BYTES] != checksum_byte(&entropy) {
        entropy.zeroize();
        return Err(PhraseParseFailure::ChecksumMismatch);
    }
    Ok(RecoveryPhrase::from_entropy(entropy))
}

fn utf8_nfc(text: &str) -> Vec<u8> {
    text.nfc().collect::<String>().into_bytes()
}

fn framed(label: &[u8], payload: &[u8]) -> Vec<u8> {
    [label, &[0], payload].concat()
}

/// `salt = SHA-256(label || 0x00 || UTF-8(NFC(account)) || enrollment_nonce[32])`.
pub fn phrase_salt(account: &str, enrollment_nonce: &[u8]) -> Result<Vec<u8>, RecoveryError> {
    if enrollment_nonce.len() != NONCE_BYTES {
        return Err(RecoveryError::InvalidInput(
            "an enrollment nonce is exactly 32 bytes",
        ));
    }
    Ok(sha256(&framed(
        PHRASE_SALT_LABEL,
        &[utf8_nfc(account), enrollment_nonce.to_vec()].concat(),
    )))
}

/// Derived phrase keys. Private halves zero on drop and cannot be debugged or serialized, and the
/// string form is `[recovery keys]`.
pub struct PhraseKeyPairs(RecoveryKeyPairs);

impl fmt::Debug for PhraseKeyPairs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_KEYS)
    }
}

impl fmt::Display for PhraseKeyPairs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_KEYS)
    }
}

impl PhraseKeyPairs {
    pub fn seal_private(&self) -> &[u8] {
        self.0.seal_private.as_slice()
    }
    pub fn seal_public(&self) -> &[u8] {
        &self.0.seal_public
    }
    pub fn sign_seed(&self) -> &[u8] {
        self.0.sign_seed.as_slice()
    }
    pub fn sign_public(&self) -> &[u8] {
        &self.0.sign_public
    }
    /// The base64url of both public halves, as a served key states them.
    pub fn public_key_text(&self) -> (String, String) {
        (
            URL_SAFE_NO_PAD.encode(&self.0.seal_public),
            URL_SAFE_NO_PAD.encode(&self.0.sign_public),
        )
    }
}

/// `seal_priv = HKDF-SHA-256(E, salt, seal_label, 32)` and
/// `sign_seed = HKDF-SHA-256(E, salt, sign_label, 32)`, with the account-bound salt above.
pub fn derive_phrase_keys(
    phrase: &RecoveryPhrase,
    account: &str,
    enrollment_nonce: &[u8],
) -> Result<PhraseKeyPairs, RecoveryError> {
    let salt = phrase_salt(account, enrollment_nonce)?;
    let mut ikm = Zeroizing::new(*phrase.entropy);
    let seal_private = Zeroizing::new(hkdf_sha256(ikm.as_ref(), &salt, PHRASE_SEAL_LABEL));
    let sign_seed = Zeroizing::new(hkdf_sha256(ikm.as_ref(), &salt, PHRASE_SIGN_LABEL));
    ikm.zeroize();
    let seal_public = x25519_public_key(seal_private.as_ref())?;
    let sign_public = {
        use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
        Ed25519KeyPair::from_seed_unchecked(sign_seed.as_ref())
            .map_err(|_| RecoveryError::Crypto)?
            .public_key()
            .as_ref()
            .to_vec()
    };
    Ok(PhraseKeyPairs(RecoveryKeyPairs {
        seal_private,
        seal_public,
        sign_seed,
        sign_public,
    }))
}

/// Distinct 1-based word positions the person must type back, chosen uniformly at random and
/// sorted.
pub fn confirmation_positions(count: usize) -> Vec<usize> {
    use aws_lc_rs::rand::{SecureRandom, SystemRandom};
    let mut chosen = std::collections::BTreeSet::new();
    let random = SystemRandom::new();
    let mut draw = [0u8; 4];
    while chosen.len() < count {
        if random.fill(&mut draw).is_err() {
            break;
        }
        let value = u32::from_be_bytes(draw);
        // Rejection sampling keeps each of the 24 positions equally likely.
        let bound = (u32::MAX / 24) * 24;
        if value < bound {
            chosen.insert(usize::try_from(value % 24).unwrap_or(0) + 1);
        }
    }
    chosen.into_iter().collect()
}

/// Whether the typed words equal the phrase's words at `positions`, normalised like
/// [`parse_recovery_phrase`].
pub fn confirmation_matches(
    phrase: &RecoveryPhrase,
    positions: &[usize],
    typed: &TypedPhrase,
) -> bool {
    let normalized: String = typed.0.nfkd().collect::<String>().to_lowercase();
    let answer: Vec<&str> = normalized.split_whitespace().collect();
    let mut indices = indices_of(&*phrase.entropy);
    let words = bip39::words();
    let matches = answer.len() == positions.len()
        && positions.iter().zip(answer.iter()).all(|(position, word)| {
            position
                .checked_sub(1)
                .and_then(|at| indices.get(at))
                .and_then(|index| words.get(usize::from(*index)))
                .is_some_and(|expected| expected == word)
        });
    indices.zeroize();
    matches
}

#[cfg(test)]
#[path = "phrase_tests.rs"]
mod tests;
