use super::*;
use crate::support::sha256::{hex, sha256_hex};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

const FIXTURE: &str = include_str!("fixtures/phrase-v1.json");
const PHRASE_V1_SHA256: &str = "474cb3779f849b3af5f16aaaf61605f6f444a99fc60f4c5f8cedb35548ad8d98";

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).expect("the vendored phrase-v1 fixture parses")
}

fn hex_bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16).unwrap_or(0))
        .collect()
}

fn b64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap_or_default()
}

#[test]
fn the_fixture_pins_the_same_wordlist_as_the_embed() {
    let fixture = fixture();
    assert_eq!(
        fixture["wordlist"]["sha256"].as_str(),
        Some(bip39::BIP39_ENGLISH_SHA256)
    );
}

#[test]
fn every_official_vector_encodes_and_derives_exactly() {
    let fixture = fixture();
    for vector in fixture["vectors"].as_array().expect("vectors") {
        let name = vector["name"].as_str().unwrap_or("");
        let mut entropy = [0u8; ENTROPY_BYTES];
        let bytes = hex_bytes(vector["entropy_hex"].as_str().unwrap_or(""));
        entropy.copy_from_slice(&bytes[..ENTROPY_BYTES]);
        let phrase = RecoveryPhrase::from_entropy(entropy);
        assert_eq!(
            phrase.reveal().join(" "),
            vector["words"].as_str().unwrap_or(""),
            "{name}: words"
        );
        let parsed =
            parse_recovery_phrase(&TypedPhrase::new(vector["words"].as_str().unwrap_or("")))
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            *parsed.entropy, entropy,
            "{name}: a parsed phrase carries its entropy"
        );
        let account = vector["account"].as_str().unwrap_or("");
        let nonce = b64(vector["enrollment_nonce"].as_str().unwrap_or(""));
        assert_eq!(
            phrase_salt(account, &nonce).expect("salt"),
            b64(vector["salt"].as_str().unwrap_or("")),
            "{name}: salt"
        );
        let keys = derive_phrase_keys(&phrase, account, &nonce).expect("keys");
        let (sealing, signing) = keys.public_key_text();
        assert_eq!(
            sealing,
            vector["sealing_public_key"].as_str().unwrap_or(""),
            "{name}: seal public"
        );
        assert_eq!(
            signing,
            vector["signing_public_key"].as_str().unwrap_or(""),
            "{name}: sign public"
        );
    }
}

#[test]
fn every_fixture_input_parses_or_refuses_as_pinned() {
    let fixture = fixture();
    for input in fixture["inputs"].as_array().expect("inputs") {
        let name = input["name"].as_str().unwrap_or("");
        let result =
            parse_recovery_phrase(&TypedPhrase::new(input["input"].as_str().unwrap_or("")));
        match input["error"].as_str() {
            None => {
                let parsed = result.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(
                    &parsed.entropy[..],
                    hex_bytes(input["entropy_hex"].as_str().unwrap_or("")).as_slice(),
                    "{name}"
                );
            }
            Some(expected) => {
                let refusal = result.err().unwrap_or_else(|| panic!("{name}: parsed"));
                let code = match refusal {
                    PhraseParseFailure::WrongWordCount => "wrong-word-count",
                    PhraseParseFailure::UnknownWord { position } => {
                        assert_eq!(Some(position as u64), input["position"].as_u64(), "{name}");
                        "unknown-word"
                    }
                    PhraseParseFailure::ChecksumMismatch => "checksum-mismatch",
                };
                assert_eq!(code, expected, "{name}");
            }
        }
    }
}

#[test]
fn derivation_is_bound_to_the_account() {
    let fixture = fixture();
    let accounts = fixture["cross_account_negative"]
        .as_array()
        .expect("accounts");
    let vector = &fixture["vectors"][0];
    let mut entropy = [0u8; ENTROPY_BYTES];
    entropy
        .copy_from_slice(&hex_bytes(vector["entropy_hex"].as_str().unwrap_or(""))[..ENTROPY_BYTES]);
    let phrase = RecoveryPhrase::from_entropy(entropy);
    let nonce = b64(vector["enrollment_nonce"].as_str().unwrap_or(""));
    let first =
        derive_phrase_keys(&phrase, accounts[0].as_str().unwrap_or(""), &nonce).expect("keys");
    let second =
        derive_phrase_keys(&phrase, accounts[1].as_str().unwrap_or(""), &nonce).expect("keys");
    assert_ne!(first.sign_public(), second.sign_public());
    assert_ne!(first.seal_public(), second.seal_public());
}

#[test]
fn refusals_name_the_problem_and_never_echo_a_word() {
    let fixture = fixture();
    let words = fixture["vectors"][0]["words"].as_str().unwrap_or("");
    let short = words.split(' ').take(23).collect::<Vec<_>>().join(" ");
    let refusal = parse_recovery_phrase(&TypedPhrase::new(short)).expect_err("23 words");
    assert_eq!(refusal, PhraseParseFailure::WrongWordCount);
    let unknown = words.replacen("abandon", "notaword", 1);
    let refusal = parse_recovery_phrase(&TypedPhrase::new(unknown)).expect_err("unknown");
    assert_eq!(refusal, PhraseParseFailure::UnknownWord { position: 1 });
    assert!(!refusal.to_string().contains("notaword"));
    let real = "cable bamboo matrix book bundle brave ski forward daughter general fence cereal brisk hunt hunt rich brush letter dust grief oyster spawn vast sense";
    let mut swapped: Vec<&str> = real.split(' ').collect();
    swapped.swap(0, 1);
    let refusal =
        parse_recovery_phrase(&TypedPhrase::new(swapped.join(" "))).expect_err("checksum");
    assert_eq!(refusal, PhraseParseFailure::ChecksumMismatch);
    assert!(!refusal.to_string().contains(swapped[0]));
}

#[test]
fn the_committed_vector_file_is_the_pinned_oracle_output() {
    assert_eq!(sha256_hex(FIXTURE.as_bytes()), PHRASE_V1_SHA256);
    assert!(fixture()["vectors"].as_array().expect("vectors").len() >= 7);
}

#[test]
fn thirty_two_zero_bytes_encode_to_abandon_then_art() {
    let words = RecoveryPhrase::from_entropy([0u8; ENTROPY_BYTES]).reveal();
    assert_eq!(words.len(), WORD_COUNT);
    assert!(words[..23].iter().all(|word| *word == "abandon"));
    assert_eq!(words[23], "art");
}

#[test]
fn the_account_is_nfc_normalised() {
    let fixture = fixture();
    let vector = fixture["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["name"] == "non-ascii-account-nfd-input")
        .expect("nfd vector");
    let account = vector["account"].as_str().expect("account");
    let nfc: String = account.nfc().collect();
    assert_ne!(nfc, account);
    let nonce = b64(vector["enrollment_nonce"].as_str().expect("nonce"));
    assert_eq!(
        phrase_salt(&nfc, &nonce).expect("nfc salt"),
        b64(vector["salt"].as_str().expect("salt"))
    );
}

#[test]
fn a_generated_phrase_is_fresh_listed_words_and_round_trips() {
    let listed = bip39::words();
    let first = RecoveryPhrase::generate().expect("generate").reveal();
    let second = RecoveryPhrase::generate().expect("generate").reveal();
    assert_eq!(first.len(), WORD_COUNT);
    assert!(first.iter().all(|word| listed.contains(word)));
    assert_ne!(first, second);
    let round_tripped =
        parse_recovery_phrase(&TypedPhrase::new(first.join(" "))).expect("round-trip");
    assert_eq!(round_tripped.reveal(), first);
}

#[test]
fn no_text_or_debug_surface_carries_a_word_or_a_key() {
    let fixture = fixture();
    let vector = fixture["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["name"] == "account-a")
        .expect("account-a vector");
    let words = vector["words"].as_str().unwrap_or("");
    let entropy_bytes = hex_bytes(vector["entropy_hex"].as_str().unwrap_or(""));
    let phrase = parse_recovery_phrase(&TypedPhrase::new(words)).expect("phrase");
    let typed = TypedPhrase::new(words);
    let keys = derive_phrase_keys(
        &phrase,
        vector["account"].as_str().unwrap_or(""),
        &b64(vector["enrollment_nonce"].as_str().unwrap_or("")),
    )
    .expect("keys");
    let split: Vec<&str> = words.split(' ').collect();
    let replaced = |pos: usize, word: &str| {
        let mut changed = split.clone();
        changed[pos] = word;
        changed.join(" ")
    };
    let short_err =
        parse_recovery_phrase(&TypedPhrase::new(split[..23].join(" "))).expect_err("23 words");
    let unknown_err =
        parse_recovery_phrase(&TypedPhrase::new(replaced(6, "notaword"))).expect_err("unknown");
    let checksum_err =
        parse_recovery_phrase(&TypedPhrase::new(replaced(23, "sentence"))).expect_err("checksum");
    let nonce_err = derive_phrase_keys(&phrase, "account", &[0u8; 31]).expect_err("short nonce");

    let surfaces = [
        format!("{phrase:?}"),
        format!("{phrase}"),
        format!("{typed:?}"),
        format!("{typed}"),
        format!("{keys:?}"),
        format!("{keys}"),
        format!("{short_err:?}"),
        format!("{short_err}"),
        format!("{unknown_err:?}"),
        format!("{unknown_err}"),
        format!("{checksum_err:?}"),
        format!("{checksum_err}"),
        format!("{nonce_err:?}"),
        format!("{nonce_err}"),
        format!("{:?}", keys.public_key_text()),
    ]
    .join("\n");

    let tokens: Vec<&str> = surfaces
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let leaked_word = split.iter().find(|word| tokens.contains(word));
    assert_eq!(leaked_word, None, "{surfaces}");

    for secret in [&entropy_bytes[..], keys.seal_private(), keys.sign_seed()] {
        for encoding in [
            URL_SAFE_NO_PAD.encode(secret),
            STANDARD.encode(secret),
            hex(secret),
        ] {
            assert!(!surfaces.contains(&encoding), "leaked secret encoding");
        }
    }
    assert_eq!(format!("{phrase:?}"), "[recovery phrase]");
    assert_eq!(format!("{typed}"), "[typed recovery phrase]");
    assert_eq!(format!("{keys}"), "[recovery keys]");
}

#[test]
fn confirmation_positions_reject_the_biased_tail_and_sort() {
    let tail = 0xffff_fff0u32;
    let mut draws = vec![tail, 5, tail, 5, 47, 0];
    let positions = confirmation_positions_with(3, || {
        Ok(if draws.is_empty() {
            tail
        } else {
            draws.remove(0)
        })
    })
    .expect("positions");
    assert_eq!(positions, vec![1, 6, 24]);
    assert!(draws.is_empty());
    assert_eq!(confirmation_positions(30).len(), WORD_COUNT);
}

#[test]
fn a_confirmation_checks_only_the_drawn_positions() {
    let fixture = fixture();
    let words = fixture["vectors"][0]["words"].as_str().unwrap_or("");
    let phrase = parse_recovery_phrase(&TypedPhrase::new(words)).expect("phrase");
    let positions = [1usize, 5, 24];
    let drawn: Vec<&str> = positions
        .iter()
        .map(|p| words.split(' ').nth(p - 1).unwrap_or(""))
        .collect();
    assert!(confirmation_matches(
        &phrase,
        &positions,
        &TypedPhrase::new(format!("  {} ", drawn.join("  ").to_uppercase()))
    ));
    let reversed: Vec<&str> = drawn.iter().copied().rev().collect();
    assert!(!confirmation_matches(
        &phrase,
        &positions,
        &TypedPhrase::new(reversed.join(" "))
    ));
    assert!(!confirmation_matches(
        &phrase,
        &positions,
        &TypedPhrase::new(drawn[..2].join(" "))
    ));
    let mut wrong = drawn.clone();
    wrong[1] = "zoo";
    assert!(!confirmation_matches(
        &phrase,
        &positions,
        &TypedPhrase::new(wrong.join(" "))
    ));
    let drawn_positions = confirmation_positions(3);
    assert_eq!(drawn_positions.len(), 3);
    assert!(drawn_positions.iter().all(|p| (1..=24).contains(p)));
    assert!(drawn_positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_wrong_length_nonce_is_refused_before_any_hash_runs() {
    let phrase = RecoveryPhrase::from_entropy([7u8; 32]);
    for len in [0usize, 16, 31, 33] {
        let bad = vec![0u8; len];
        assert!(matches!(
            phrase_salt("account", &bad),
            Err(RecoveryError::InvalidInput(_))
        ));
        assert!(matches!(
            derive_phrase_keys(&phrase, "account", &bad),
            Err(RecoveryError::InvalidInput(_))
        ));
    }
}
