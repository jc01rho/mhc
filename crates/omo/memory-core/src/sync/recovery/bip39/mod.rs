//! The BIP39 English wordlist (bip39/english.txt, 2048 words), embedded so bundled builds need no
//! file read. `english.txt` is vendored byte-for-byte; `phrase.rs` asserts its pinned SHA-256 at load
//! and the phrase tests prove the embedded text is that same file.

pub const BIP39_ENGLISH_TEXT: &str = include_str!("english.txt");
