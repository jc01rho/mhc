use super::phrase_hints::{PhraseHintRefusal, parse_phrase_hints};
use super::{
    RecoveryError,
    derive::public_key_text,
    phrase::{
        BIP39_ENGLISH_SHA256, PhraseParseFailure, PhraseParseResult, RecoveryPhrase, TypedPhrase,
        confirmation_matches, confirmation_positions_with, derive_phrase_keys,
        generate_recovery_phrase, generate_recovery_phrase_with, parse_recovery_phrase,
        phrase_salt,
    },
};
use crate::{support::sha256::hex, sync::crypto::signatures::sha256};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

const PHRASE_V1_SHA256: &str = "474cb3779f849b3af5f16aaaf61605f6f444a99fc60f4c5f8cedb35548ad8d98";
const FIXTURE: &str = include_str!("fixtures/phrase-v1.json");
const WORDLIST: &str = include_str!("bip39/english.txt");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("committed phrase-v1 vectors")
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).expect("base64url fixture")
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&text[offset..offset + 2], 16).expect("hex fixture"))
        .collect()
}

fn from_entropy(entropy_hex: &str) -> RecoveryPhrase {
    let bytes = decode_hex(entropy_hex);
    generate_recovery_phrase_with(|entropy| entropy.copy_from_slice(&bytes))
}

fn parsed(text: &str) -> RecoveryPhrase {
    match parse_recovery_phrase(&TypedPhrase::new(text)) {
        PhraseParseResult::Parsed(phrase) => phrase,
        PhraseParseResult::Refused(reason) => panic!("fixture input refused: {reason:?}"),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().expect("fixture string")
}

fn vector(name: &str) -> Value {
    fixture()["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|vector| vector["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no vector {name}"))
}

fn derived(vector: &Value) -> (String, String) {
    let keys = derive_phrase_keys(
        &parsed(text(vector, "words")),
        text(vector, "account"),
        &decode(text(vector, "enrollment_nonce")),
    )
    .expect("phrase keys");
    public_key_text(&keys)
}

#[test]
fn the_vendored_wordlist_is_the_pinned_13116_byte_file() {
    // Given the wordlist vendored beside the module and the pin the phrase contract states.
    assert_eq!(WORDLIST.len(), 13116);
    assert_eq!(WORDLIST.lines().count(), 2048);
    // When its bytes are hashed.
    let digest = hex(&sha256(WORDLIST.as_bytes()));
    // Then it is the pinned file the fixture also names.
    assert_eq!(digest, BIP39_ENGLISH_SHA256);
    assert_eq!(digest, text(&fixture()["wordlist"], "sha256"));
    assert_eq!(WORDLIST.lines().next(), Some("abandon"));
    assert_eq!(WORDLIST.lines().last(), Some("zoo"));
}

#[test]
fn thirty_two_zero_bytes_encode_to_abandon_then_art() {
    let words = from_entropy(&"00".repeat(32))
        .reveal()
        .expect("revealed words");
    assert_eq!(words.len(), 24);
    assert!(words[..23].iter().all(|word| *word == "abandon"));
    assert_eq!(words[23], "art");
}

#[test]
fn the_committed_vector_file_is_the_pinned_oracle_output() {
    assert_eq!(hex(&sha256(FIXTURE.as_bytes())), PHRASE_V1_SHA256);
    assert!(fixture()["vectors"].as_array().expect("vectors").len() >= 7);
}

#[test]
fn every_vector_reproduces_encoding_decoding_salt_and_both_public_keys() {
    let fixture = fixture();
    for vector in fixture["vectors"].as_array().expect("vectors") {
        let words = text(vector, "words");
        assert_eq!(
            from_entropy(text(vector, "entropy_hex"))
                .reveal()
                .expect("revealed")
                .join(" "),
            words
        );
        assert_eq!(parsed(words).reveal().expect("revealed").join(" "), words);
        let salt = phrase_salt(
            text(vector, "account"),
            &decode(text(vector, "enrollment_nonce")),
        )
        .expect("salt");
        assert_eq!(b64(&salt), text(vector, "salt"));
        let (sealing, signing) = derived(vector);
        assert_eq!(sealing, text(vector, "sealing_public_key"));
        assert_eq!(signing, text(vector, "signing_public_key"));
    }
}

#[test]
fn a_cross_account_pair_shares_entropy_but_not_keys() {
    let fixture = fixture();
    let pair: Vec<Value> = fixture["cross_account_negative"]
        .as_array()
        .expect("negative pair")
        .iter()
        .map(|name| vector(name.as_str().expect("vector name")))
        .collect();
    let (a, b) = (&pair[0], &pair[1]);
    assert_eq!(text(a, "entropy_hex"), text(b, "entropy_hex"));
    assert_eq!(text(a, "enrollment_nonce"), text(b, "enrollment_nonce"));
    assert_ne!(text(a, "account"), text(b, "account"));
    assert_ne!(derived(a).0, derived(b).0);
    assert_ne!(derived(a).1, derived(b).1);
}

#[test]
fn the_account_is_nfc_normalised() {
    let vector = vector("non-ascii-account-nfd-input");
    let account = text(&vector, "account");
    let nfc: String = account.nfc().collect();
    assert_ne!(nfc, account);
    let nonce = decode(text(&vector, "enrollment_nonce"));
    assert_eq!(
        b64(&phrase_salt(&nfc, &nonce).expect("salt")),
        text(&vector, "salt")
    );
}

#[test]
fn normalised_and_fullwidth_inputs_parse_to_the_same_entropy() {
    let fixture = fixture();
    for input in fixture["inputs"].as_array().expect("inputs") {
        let Some(entropy) = input["entropy_hex"].as_str() else {
            continue;
        };
        assert_eq!(
            parsed(text(input, "input")).reveal().expect("revealed"),
            from_entropy(entropy).reveal().expect("revealed")
        );
    }
}

#[test]
fn refused_inputs_name_the_problem_without_the_words() {
    let fixture = fixture();
    for input in fixture["inputs"].as_array().expect("inputs") {
        let Some(reason) = input["error"].as_str() else {
            continue;
        };
        let expected = match reason {
            "checksum-mismatch" => PhraseParseFailure::ChecksumMismatch,
            "unknown-word" => PhraseParseFailure::UnknownWord {
                position: input["position"].as_u64().expect("position") as usize,
            },
            "wrong-word-count" => PhraseParseFailure::WrongWordCount,
            other => panic!("unexpected fixture reason {other}"),
        };
        match parse_recovery_phrase(&TypedPhrase::new(text(input, "input"))) {
            PhraseParseResult::Refused(actual) => assert_eq!(actual, expected),
            PhraseParseResult::Parsed(_) => panic!("{reason} input was accepted"),
        }
    }
}

#[test]
fn a_generated_phrase_is_fresh_listed_words_and_round_trips() {
    let listed: Vec<&str> = WORDLIST.lines().collect();
    let first = generate_recovery_phrase()
        .expect("random phrase")
        .reveal()
        .expect("revealed");
    let second = generate_recovery_phrase()
        .expect("random phrase")
        .reveal()
        .expect("revealed");
    assert_eq!(first.len(), 24);
    assert!(first.iter().all(|word| listed.contains(word)));
    assert_ne!(first, second);
    assert_eq!(parsed(&first.join(" ")).reveal().expect("revealed"), first);
}

#[test]
fn dispose_zeroes_entropy_and_leaves_the_phrase_unusable() {
    let mut phrase = from_entropy(&"7f".repeat(32));
    phrase.dispose();
    assert_eq!(
        phrase.reveal().expect_err("disposed phrase").to_string(),
        "the recovery phrase was disposed"
    );
    assert!(derive_phrase_keys(&phrase, "account", &[0u8; 32]).is_err());
}

#[test]
fn an_enrollment_nonce_must_be_exactly_32_bytes() {
    let phrase = from_entropy(&"00".repeat(32));
    for length in [0, 16, 31, 33] {
        assert!(matches!(
            derive_phrase_keys(&phrase, "account", &vec![0u8; length]),
            Err(RecoveryError::InvalidInput(_))
        ));
        assert!(matches!(
            phrase_salt("account", &vec![0u8; length]),
            Err(RecoveryError::InvalidInput(_))
        ));
    }
}

#[test]
fn confirmation_positions_reject_the_biased_tail_and_sort() {
    let tail = 0xffff_fff0u32;
    let mut remaining = vec![tail, 5, tail, 5, 47, 0];
    let positions = {
        let mut draw = || -> Result<u32, RecoveryError> { Ok(remaining.remove(0)) };
        confirmation_positions_with(3, &mut draw).expect("positions")
    };
    assert_eq!(positions, vec![1, 6, 24]);
    assert!(remaining.is_empty());
}

#[test]
fn confirmation_matches_only_the_asked_positions_in_order_and_normalised() {
    let phrase = generate_recovery_phrase_with(|entropy| entropy.fill(0));
    let words = phrase.reveal().expect("revealed");
    let asked = [1, 12, 24];
    let right: Vec<&str> = asked.iter().map(|position| words[position - 1]).collect();
    assert!(
        confirmation_matches(
            &phrase,
            &asked,
            &TypedPhrase::new(format!("  {} ", right.join("   ").to_uppercase()))
        )
        .expect("match")
    );
    let reversed = right.iter().rev().copied().collect::<Vec<_>>().join(" ");
    assert!(!confirmation_matches(&phrase, &asked, &TypedPhrase::new(reversed)).expect("match"));
    assert!(
        !confirmation_matches(&phrase, &asked, &TypedPhrase::new(right[..2].join(" ")))
            .expect("match")
    );
}

fn encodings(bytes: &[u8]) -> Vec<String> {
    vec![b64(bytes), STANDARD.encode(bytes), hex(bytes)]
}

#[test]
fn phrase_material_never_reaches_a_string_form() {
    let vector = vector("account-a");
    let words = text(&vector, "words");
    let nonce = decode(text(&vector, "enrollment_nonce"));
    let mut phrase = from_entropy(text(&vector, "entropy_hex"));
    let typed = TypedPhrase::new(words);
    let mut parsed_phrase = parsed(words);
    let keys = derive_phrase_keys(&parsed_phrase, text(&vector, "account"), &nonce).expect("keys");

    let mut canaries = vec![words.to_owned()];
    canaries.extend(encodings(&decode(text(&vector, "entropy_hex"))));
    canaries.extend(encodings(&keys.seal_private[..]));
    canaries.extend(encodings(&keys.sign_seed[..]));

    let split: Vec<&str> = words.split(' ').collect();
    let replaced = |position: usize, word: &'static str| {
        let mut changed = split.clone();
        changed[position] = word;
        changed.join(" ")
    };
    let failures = [
        parse_recovery_phrase(&TypedPhrase::new(split[..23].join(" "))),
        parse_recovery_phrase(&TypedPhrase::new(replaced(6, "notaword"))),
        parse_recovery_phrase(&TypedPhrase::new(replaced(23, "sentence"))),
    ];
    let disposed_error = {
        let mut disposed = from_entropy(text(&vector, "entropy_hex"));
        disposed.dispose();
        disposed.reveal().expect_err("disposed").to_string()
    };
    let Err(short_nonce) = derive_phrase_keys(&phrase, text(&vector, "account"), &[0u8; 31]) else {
        panic!("a 31-byte nonce derived keys");
    };
    let short_nonce_error = short_nonce.to_string();

    let surfaces = [
        format!("{phrase}"),
        format!("{phrase:?}"),
        format!("{typed}"),
        format!("{typed:?}"),
        format!("{parsed_phrase:?}"),
        format!("{failures:?}"),
        disposed_error,
        short_nonce_error,
    ]
    .join("\n");

    for canary in &canaries {
        assert!(!surfaces.contains(canary), "a canary reached a string form");
    }
    for word in &split {
        assert!(!surfaces.contains(word), "a word reached a string form");
    }
    assert_eq!(format!("{phrase}"), "[recovery phrase]");
    assert_eq!(format!("{typed}"), "[typed recovery phrase]");

    let (sealing, signing) = public_key_text(&keys);
    assert!(!sealing.is_empty() && !signing.is_empty());
    phrase.dispose();
    parsed_phrase.dispose();
}

#[test]
fn only_reveal_yields_the_words() {
    let vector = vector("account-a");
    let phrase = from_entropy(text(&vector, "entropy_hex"));
    assert_eq!(
        phrase.reveal().expect("revealed").join(" "),
        text(&vector, "words")
    );
    assert_eq!(format!("{phrase}"), "[recovery phrase]");
}

#[test]
fn served_phrase_hints_keep_served_order_drop_removals_and_bound_both_kinds() {
    // Given a passkey genesis key, two phrase keys and a removal of the second phrase key.
    let phrase = |id: &str| json!({"body":{"kind":"recovery-key","credential_id":id,"enrollment_nonce":"n","sealing_public_key":"s","signing_public_key":"g","survivable":"phrase"}});
    let rows = json!([
        {"body":{"kind":"genesis","bootstrap":{"recovery_key":{"credential_id":"passkey","enrollment_nonce":"n","sealing_public_key":"s","signing_public_key":"g","survivable":"synced"}}}},
        phrase("z-phrase"),
        phrase("a-phrase"),
        {"body":{"kind":"recovery-key-removed","credential_id":"a-phrase"}},
    ]);
    // When the hints are read before verification.
    let hints = parse_phrase_hints(&rows).expect("hints");
    // Then only the live phrase key is a hint.
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].credential_id, "z-phrase");
    // And seventeen live keys of either kind are refused, not truncated.
    let many: Vec<Value> = (0..17).map(|id| phrase(&id.to_string())).collect();
    assert_eq!(
        parse_phrase_hints(&json!(many)),
        Err(PhraseHintRefusal::TooManyRecoveryKeys)
    );
    assert_eq!(
        parse_phrase_hints(&json!({"rows": []})),
        Err(PhraseHintRefusal::Unavailable)
    );
}
