use super::{
    RecoveryError,
    bip39::BIP39_ENGLISH_TEXT,
    derive::{RecoveryKeyPairs, framed, x25519_public_key},
};
use crate::{
    support::sha256::hex,
    sync::crypto::{signatures::sha256, vendor::sync_format::keys::hkdf_sha256},
};
use aws_lc_rs::{
    rand::{SecureRandom, SystemRandom},
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{collections::HashMap, fmt, sync::OnceLock};
use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, Zeroizing};

/// SHA-256 of the vendored BIP39 `english.txt` (2048 words, LF, trailing LF, 13116 bytes).
pub const BIP39_ENGLISH_SHA256: &str =
    "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda";
const PHRASE_SALT_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-salt";
const PHRASE_SEAL_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-seal";
const PHRASE_SIGN_LABEL: &[u8] = b"omo-sync/v1/recovery-phrase-sign";
const ENTROPY_BYTES: usize = 32;
pub const WORD_COUNT: usize = 24;
const NONCE_BYTES: usize = 32;
const WORDLIST_WORDS: usize = 2048;
const REDACTED_PHRASE: &str = "[recovery phrase]";
const REDACTED_TYPED: &str = "[typed recovery phrase]";

fn wordlist() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        assert_eq!(
            hex(&sha256(BIP39_ENGLISH_TEXT.as_bytes())),
            BIP39_ENGLISH_SHA256,
            "the embedded BIP39 English wordlist does not match its pinned SHA-256"
        );
        let words: Vec<&'static str> = BIP39_ENGLISH_TEXT.lines().collect();
        assert_eq!(
            words.len(),
            WORDLIST_WORDS,
            "the BIP39 English wordlist must hold 2048 words"
        );
        words
    })
}

fn index() -> &'static HashMap<&'static str, u16> {
    static INDEX: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
    INDEX.get_or_init(|| {
        wordlist()
            .iter()
            .enumerate()
            .map(|(position, word)| (*word, position as u16))
            .collect()
    })
}

fn checksum_byte(entropy: &[u8; ENTROPY_BYTES]) -> u8 {
    sha256(entropy)[0]
}

/// 24 big-endian 11-bit indices over E || first byte of SHA-256(E).
fn indices_of(entropy: &[u8; ENTROPY_BYTES]) -> [u16; WORD_COUNT] {
    let mut indices = [0u16; WORD_COUNT];
    let checksum = checksum_byte(entropy);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    let mut next = 0usize;
    for byte in entropy.iter().copied().chain(std::iter::once(checksum)) {
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        if bits >= 11 {
            bits -= 11;
            indices[next] = ((accumulator >> bits) & 0x7ff) as u16;
            next += 1;
            accumulator &= (1 << bits) - 1;
        }
    }
    indices
}

/// The 33 bytes E || checksum the 24 indices carry.
fn bytes_of(indices: &[u16; WORD_COUNT]) -> [u8; ENTROPY_BYTES + 1] {
    let mut out = [0u8; ENTROPY_BYTES + 1];
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    let mut next = 0usize;
    for &index in indices {
        accumulator = (accumulator << 11) | u32::from(index);
        bits += 11;
        while bits >= 8 {
            bits -= 8;
            out[next] = ((accumulator >> bits) & 0xff) as u8;
            next += 1;
        }
        accumulator &= (1 << bits) - 1;
    }
    out
}

/// A recovery phrase: E lives in a zeroizing field, every string form is "[recovery phrase]", only
/// `reveal` yields the words, and `dispose` leaves the phrase unusable.
pub struct RecoveryPhrase {
    entropy: Zeroizing<[u8; ENTROPY_BYTES]>,
    disposed: bool,
}

impl RecoveryPhrase {
    fn from_entropy(entropy: [u8; ENTROPY_BYTES]) -> Self {
        Self {
            entropy: Zeroizing::new(entropy),
            disposed: false,
        }
    }
    fn entropy(&self) -> Result<&[u8; ENTROPY_BYTES], RecoveryError> {
        if self.disposed {
            return Err(RecoveryError::InvalidInput(
                "the recovery phrase was disposed",
            ));
        }
        Ok(&*self.entropy)
    }
    /// The 24 words, for the display surface only.
    pub fn reveal(&self) -> Result<Vec<&'static str>, RecoveryError> {
        let mut indices = indices_of(self.entropy()?);
        let words = wordlist();
        let revealed = indices
            .iter()
            .map(|index| words[usize::from(*index)])
            .collect();
        indices.zeroize();
        Ok(revealed)
    }
    pub fn dispose(&mut self) {
        self.entropy.zeroize();
        self.disposed = true;
    }
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

/// Text a person typed as a recovery phrase, redacted the same way; `parse_recovery_phrase`
/// consumes it.
pub struct TypedPhrase(Zeroizing<String>);

impl TypedPhrase {
    pub fn new(text: impl Into<String>) -> Self {
        Self(Zeroizing::new(text.into()))
    }
    fn normalised(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.as_str().nfkd().collect::<String>().to_lowercase())
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

/// A fresh phrase: E is 32 new random bytes for every enrollment. The `_with` form exists for fixed
/// test vectors; production callers pass nothing.
pub fn generate_recovery_phrase() -> Result<RecoveryPhrase, RecoveryError> {
    let mut entropy = [0u8; ENTROPY_BYTES];
    SystemRandom::new()
        .fill(&mut entropy)
        .map_err(|_| RecoveryError::Crypto)?;
    Ok(RecoveryPhrase::from_entropy(entropy))
}

pub fn generate_recovery_phrase_with(
    fill: impl FnOnce(&mut [u8; ENTROPY_BYTES]),
) -> RecoveryPhrase {
    let mut entropy = [0u8; ENTROPY_BYTES];
    fill(&mut entropy);
    RecoveryPhrase::from_entropy(entropy)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseParseFailure {
    WrongWordCount,
    /// 1-based; the word itself is never reported.
    UnknownWord {
        position: usize,
    },
    ChecksumMismatch,
}

#[derive(Debug)]
pub enum PhraseParseResult {
    Parsed(RecoveryPhrase),
    Refused(PhraseParseFailure),
}

/// NFKD, lowercase, any whitespace run as one separator; then exactly 24 listed words and a matching
/// checksum, all before any derivation can run.
pub fn parse_recovery_phrase(typed: &TypedPhrase) -> PhraseParseResult {
    let normalised = typed.normalised();
    let words: Vec<&str> = normalised.split_whitespace().collect();
    if words.len() != WORD_COUNT {
        return PhraseParseResult::Refused(PhraseParseFailure::WrongWordCount);
    }
    let index = index();
    let mut indices = [0u16; WORD_COUNT];
    for (offset, word) in words.iter().enumerate() {
        match index.get(*word) {
            Some(&position) => indices[offset] = position,
            None => {
                indices.zeroize();
                return PhraseParseResult::Refused(PhraseParseFailure::UnknownWord {
                    position: offset + 1,
                });
            }
        }
    }
    let mut packed = bytes_of(&indices);
    indices.zeroize();
    let mut entropy = [0u8; ENTROPY_BYTES];
    entropy.copy_from_slice(&packed[..ENTROPY_BYTES]);
    let checksum = packed[ENTROPY_BYTES];
    packed.zeroize();
    if checksum != checksum_byte(&entropy) {
        entropy.zeroize();
        return PhraseParseResult::Refused(PhraseParseFailure::ChecksumMismatch);
    }
    PhraseParseResult::Parsed(RecoveryPhrase::from_entropy(entropy))
}

/// salt = SHA-256(label || 0x00 || UTF-8(NFC(account)) || enrollment_nonce[32]).
pub fn phrase_salt(account: &str, enrollment_nonce: &[u8]) -> Result<Vec<u8>, RecoveryError> {
    if enrollment_nonce.len() != NONCE_BYTES {
        return Err(RecoveryError::InvalidInput(
            "an enrollment nonce is exactly 32 bytes",
        ));
    }
    Ok(sha256(&framed(
        PHRASE_SALT_LABEL,
        &[
            account.nfc().collect::<String>().as_bytes(),
            enrollment_nonce,
        ]
        .concat(),
    )))
}

/// seal_private = HKDF-SHA-256(E, salt, seal label, 32) and sign_seed = HKDF-SHA-256(E, salt, sign
/// label, 32) over the account-bound salt. Every private key derived so far is overwritten if a later
/// step fails.
pub fn derive_phrase_keys(
    phrase: &RecoveryPhrase,
    account: &str,
    enrollment_nonce: &[u8],
) -> Result<RecoveryKeyPairs, RecoveryError> {
    let salt = phrase_salt(account, enrollment_nonce)?;
    let ikm = Zeroizing::new(*phrase.entropy()?);
    let seal_private = Zeroizing::new(hkdf_sha256(&ikm[..], &salt, PHRASE_SEAL_LABEL));
    let seal_public = x25519_public_key(&seal_private[..])?;
    let sign_seed = Zeroizing::new(hkdf_sha256(&ikm[..], &salt, PHRASE_SIGN_LABEL));
    let sign_public = Ed25519KeyPair::from_seed_unchecked(&sign_seed[..])
        .map_err(|_| RecoveryError::Crypto)?
        .public_key()
        .as_ref()
        .to_vec();
    Ok(RecoveryKeyPairs {
        seal_private,
        seal_public,
        sign_seed,
        sign_public,
    })
}

/// Distinct 1-based word positions the person must type back, chosen uniformly at random and sorted.
pub fn confirmation_positions(count: usize) -> Result<Vec<usize>, RecoveryError> {
    let random = SystemRandom::new();
    let mut draw = || {
        let mut bytes = [0u8; 4];
        random
            .fill(&mut bytes)
            .map(|()| u32::from_be_bytes(bytes))
            .map_err(|_| RecoveryError::Crypto)
    };
    confirmation_positions_with(count, &mut draw)
}

/// Rejection sampling keeps each of the 24 positions equally likely.
pub fn confirmation_positions_with(
    count: usize,
    draw: &mut dyn FnMut() -> Result<u32, RecoveryError>,
) -> Result<Vec<usize>, RecoveryError> {
    let limit = (u64::from(u32::MAX) + 1) / WORD_COUNT as u64 * WORD_COUNT as u64;
    let mut chosen = std::collections::BTreeSet::new();
    while chosen.len() < count {
        let value = draw()?;
        if u64::from(value) < limit {
            chosen.insert(u64::from(value) as usize % WORD_COUNT + 1);
        }
    }
    Ok(chosen.into_iter().collect())
}

/// Whether the typed words equal the phrase's words at `positions`, normalised like
/// `parse_recovery_phrase`.
pub fn confirmation_matches(
    phrase: &RecoveryPhrase,
    positions: &[usize],
    typed: &TypedPhrase,
) -> Result<bool, RecoveryError> {
    let normalised = typed.normalised();
    let answer: Vec<&str> = normalised.split_whitespace().collect();
    let mut indices = indices_of(phrase.entropy()?);
    let words = wordlist();
    let matched = answer.len() == positions.len()
        && positions
            .iter()
            .zip(answer.iter())
            .all(|(position, answer)| {
                position
                    .checked_sub(1)
                    .and_then(|offset| indices.get(offset))
                    .map(|index| words[usize::from(*index)])
                    == Some(*answer)
            });
    indices.zeroize();
    Ok(matched)
}
