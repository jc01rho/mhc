//! The BIP39 English wordlist (bip39/english.txt, 2048 words), embedded so bundled builds need no
//! file read. [`words`] proves the embedded text is byte-identical to the vendored file and carries
//! the pinned SHA-256 (contract "phrase-v1" vectors), so a corrupted embed fails loudly.

use crate::sync::crypto::signatures::sha256;

pub const BIP39_ENGLISH_SHA256: &str = "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda";
pub const BIP39_ENGLISH_TXT: &str = include_str!("english.txt");
pub const WORD_COUNT: usize = 2048;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The 2048 words, in file order. Panics only on a corrupted embed: the text must hash to
/// [`BIP39_ENGLISH_SHA256`] and hold exactly [`WORD_COUNT`] lines.
pub fn words() -> Vec<&'static str> {
    assert_eq!(
        hex(&sha256(BIP39_ENGLISH_TXT.as_bytes())),
        BIP39_ENGLISH_SHA256,
        "the embedded BIP39 English wordlist does not match its pinned SHA-256"
    );
    let words: Vec<&'static str> = BIP39_ENGLISH_TXT.lines().collect();
    assert_eq!(
        words.len(),
        WORD_COUNT,
        "the BIP39 English wordlist must hold 2048 words"
    );
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_wordlist_is_the_pinned_file() {
        let words = words();
        assert_eq!(words.len(), WORD_COUNT);
        assert_eq!(words[0], "abandon");
        assert_eq!(words[2047], "zoo");
        assert_eq!(
            BIP39_ENGLISH_TXT,
            words.join("\n") + "\n",
            "the embed must stay byte-identical to english.txt"
        );
    }
}
