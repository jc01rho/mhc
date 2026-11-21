use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

const FIXTURE: &str = include_str!("fixtures/phrase-v1.json");

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
fn typed_input_is_normalised_before_parsing() {
    let fixture = fixture();
    for input in fixture["inputs"].as_array().expect("inputs") {
        let parsed =
            parse_recovery_phrase(&TypedPhrase::new(input["input"].as_str().unwrap_or("")))
                .unwrap_or_else(|error| {
                    panic!("{}: {error}", input["name"].as_str().unwrap_or(""))
                });
        assert_eq!(
            &parsed.entropy[..],
            hex_bytes(input["entropy_hex"].as_str().unwrap_or("")).as_slice(),
            "{}",
            input["name"].as_str().unwrap_or("")
        );
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
    let mut swapped: Vec<&str> = words.split(' ').collect();
    swapped.swap(0, 1);
    let refusal =
        parse_recovery_phrase(&TypedPhrase::new(swapped.join(" "))).expect_err("checksum");
    assert_eq!(refusal, PhraseParseFailure::ChecksumMismatch);
    assert!(!refusal.to_string().contains(swapped[0]));
}

#[test]
fn no_text_or_debug_surface_carries_a_word_or_a_key() {
    let fixture = fixture();
    let vector = &fixture["vectors"][0];
    let words = vector["words"].as_str().unwrap_or("");
    let phrase = parse_recovery_phrase(&TypedPhrase::new(words)).expect("phrase");
    let typed = TypedPhrase::new(words);
    let keys = derive_phrase_keys(
        &phrase,
        vector["account"].as_str().unwrap_or(""),
        &b64(vector["enrollment_nonce"].as_str().unwrap_or("")),
    )
    .expect("keys");
    let surfaces = [
        format!("{phrase:?}"),
        format!("{phrase}"),
        format!("{typed:?}"),
        format!("{typed}"),
        format!("{keys:?}"),
        format!("{keys}"),
        format!("{:?}", keys.public_key_text()),
    ];
    let leaked_word = words
        .split(' ')
        .filter(|word| word.len() > 4)
        .find(|word| surfaces.iter().any(|surface| surface.contains(word)));
    assert_eq!(leaked_word, None, "{surfaces:?}");
    let leaked_key = URL_SAFE_NO_PAD.encode(keys.sign_seed());
    assert!(
        surfaces
            .iter()
            .all(|surface| !surface.contains(&leaked_key))
    );
    assert_eq!(format!("{phrase:?}"), "[recovery phrase]");
    assert_eq!(format!("{typed}"), "[typed recovery phrase]");
    assert_eq!(format!("{keys}"), "[recovery keys]");
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
        &TypedPhrase::new(drawn.join("  "))
    ));
    assert!(!confirmation_matches(
        &phrase,
        &positions,
        &TypedPhrase::new(drawn[..2].join(" "))
    ));
    let mut wrong = drawn.clone();
    wrong[1] = "abandon";
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
    assert!(phrase_salt("account", &[0u8; 31]).is_err());
    let phrase = RecoveryPhrase::from_entropy([7u8; 32]);
    assert!(derive_phrase_keys(&phrase, "account", &[]).is_err());
}
