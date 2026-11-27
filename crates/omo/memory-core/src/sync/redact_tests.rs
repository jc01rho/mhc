use std::sync::LazyLock;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_redact_url_when_https_url_carrying_token_then_credential_is_masked() {
    let url = "https://ghp_abcdef1234567890@github.com/acme/memory.git";
    let redacted = redact_url(url);
    assert_eq!(redacted, "https://***@github.com/acme/memory.git");
    assert!(!redacted.contains("ghp_abcdef1234567890"));
}

#[test]
fn test_redact_url_when_https_url_with_user_and_password_then_both_halves_are_masked() {
    let url = "https://alice:s3cr3t-pat@gitlab.example.com/team/memory.git";
    let redacted = redact_url(url);
    assert_eq!(
        redacted,
        "https://***:***@gitlab.example.com/team/memory.git"
    );
    assert!(!redacted.contains("s3cr3t-pat"));
    assert!(!redacted.contains("alice"));
}

#[test]
fn test_redact_url_when_ssh_scp_style_url_then_user_info_is_masked() {
    let url = "git@github.com:acme/memory.git";
    let redacted = redact_url(url);
    assert_eq!(redacted, "***@github.com:acme/memory.git");
    assert!(!redacted.contains("git@"));
}

#[test]
fn test_redact_url_when_ssh_url_with_user_info_then_user_info_is_masked() {
    let url = "ssh://deploy:key123@git.example.com:2222/srv/memory.git";
    let redacted = redact_url(url);
    assert_eq!(
        redacted,
        "ssh://***:***@git.example.com:2222/srv/memory.git"
    );
    assert!(!redacted.contains("key123"));
}

#[test]
fn test_redact_url_when_urls_without_credentials_then_pass_through_unchanged() {
    let urls = [
        "https://github.com/acme/memory.git",
        "file:///tmp/mirror.git",
        "/srv/mirrors/memory.git",
    ];

    for url in urls {
        assert_eq!(redact_url(url), url);
    }
}

#[test]
fn test_redact_url_when_free_text_containing_credentialed_url_then_masked() {
    let line = "fatal: could not read from https://x-token:abc123@github.com/acme/memory.git";
    let redacted = redact_url(line);
    assert!(redacted.contains("https://***:***@github.com/acme/memory.git"));
    assert!(!redacted.contains("abc123"));
}

#[test]
fn test_redact_url_when_secret_like_material_in_sync_output_then_predicate_matches_and_redaction_clears_it()
 {
    let value = "token=abc123 and AKIA1234567890ABCDEF";
    let redacted = redact_url(value);
    assert!(contains_secret_like_material(value));
    assert_eq!(redacted, "*** and ***");
    assert!(!contains_secret_like_material(&redacted));
}

#[test]
fn test_contains_secret_like_material_when_common_credential_forms_then_recognized() {
    for value in [
        "password=hunter2",
        "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.abc",
        "sk-proj-AAAABBBBCCCCDDDD",
    ] {
        assert!(
            contains_secret_like_material(value),
            "not recognized: {value}"
        );
    }
}

#[test]
fn test_redact_url_when_repeated_aws_keys_then_every_key_is_masked() {
    let value = "AKIA1234567890ABCDEF and AKIAABCDEFGHIJKLMNOP";
    let redacted = redact_url(value);
    assert_eq!(redacted, "*** and ***");
    assert!(!contains_secret_like_material(&redacted));
}

#[test]
fn test_redact_url_when_repeated_token_pairs_then_every_pair_is_masked() {
    let value = "token=aaa token=bbb";
    let redacted = redact_url(value);
    assert_eq!(redacted, "*** ***");
    assert!(!contains_secret_like_material(&redacted));
}

#[test]
fn test_redact_url_when_repeated_secrets_and_url_userinfo_then_all_are_masked() {
    let value = "token=aaa https://alice:password@example.test token=bbb";
    let redacted = redact_url(value);
    assert_eq!(redacted, "*** https://***:***@example.test ***");
    assert!(!contains_secret_like_material(&redacted));
}

#[test]
fn test_contains_secret_like_material_when_checked_repeatedly_then_stays_true() {
    let value = "token=aaa token=bbb";
    assert!(contains_secret_like_material(value));
    assert!(contains_secret_like_material(value));
}

#[test]
fn test_redact_url_when_malformed_pem_shaped_input_then_unchanged_and_real_pem_is_masked() {
    let hostile = format!(
        "-----BEGIN {}-----{}",
        "A".repeat(100_000),
        "B".repeat(100_000)
    );
    assert_eq!(redact_url(&hostile), hostile);

    let pem = "-----BEGIN PRIVATE KEY-----secret-----END PRIVATE KEY-----";
    assert_eq!(redact_url(pem), "***");
}

#[test]
fn test_contains_secret_like_material_when_neighbour_is_not_ascii_word_then_ascii_boundary_still_matches()
 {
    // JavaScript `\b` is ASCII-only, so a non-ASCII neighbour is not a word
    // character and the AWS key stays bounded; an ASCII word character is not.
    assert!(contains_secret_like_material("\u{e9}AKIA1234567890ABCDEF"));
    assert!(contains_secret_like_material("AKIA1234567890ABCDEF\u{e9}"));
    assert!(!contains_secret_like_material("xAKIA1234567890ABCDEF"));
}

#[test]
fn test_redact_url_when_token_exceeds_the_upstream_256_character_bound_then_only_the_bound_is_replaced()
 {
    assert_eq!(redact_url(&format!("token={}", "a".repeat(256))), "***");
    assert_eq!(redact_url(&format!("token={}", "a".repeat(257))), "***a");
}

#[test]
fn test_redact_url_when_token_bound_counts_utf16_units_then_128_astral_characters_are_fully_masked()
{
    // 128 supplementary characters are 256 UTF-16 code units, exactly `\S{1,256}`.
    let token = "\u{1F600}".repeat(128);
    assert_eq!(redact_url(&format!("token={token}")), "***");
}

#[test]
fn test_redact_url_when_token_bound_counts_utf16_units_then_129_astral_characters_keep_the_last() {
    // 129 supplementary characters are 258 code units; the bound keeps 256, so
    // the last character survives.
    let token = "\u{1F600}".repeat(129);
    assert_eq!(redact_url(&format!("token={token}")), "***\u{1F600}");
}

#[test]
fn test_redact_url_when_javascript_whitespace_separator_then_feff_joins_and_0085_does_not() {
    // JavaScript `\s` includes U+FEFF, so it separates `token` from `=`.
    assert_eq!(redact_url("token\u{FEFF}=abc"), "***");
    // JavaScript `\s` excludes U+0085, so `token\u{0085}=abc` is not a token pair.
    assert_eq!(redact_url("token\u{0085}=abc"), "token\u{0085}=abc");
}

#[test]
fn test_contains_secret_like_material_when_case_differs_then_case_sensitive_patterns_do_not_match()
{
    assert!(!contains_secret_like_material("akia1234567890abcdef"));
    assert!(!contains_secret_like_material("SK-AAAABBBBCCCCDDDD"));
    assert!(!contains_secret_like_material("GHP-abcdefghijkl"));
    assert!(contains_secret_like_material("PASSWORD=hunter2"));
    assert!(contains_secret_like_material("authorization: bearer abc"));
}

#[test]
fn test_redact_url_when_pem_blocks_then_matching_labels_replace_and_mismatched_end_is_skipped() {
    let mismatched =
        "-----BEGIN PRIVATE KEY-----a-----END CERTIFICATE-----b-----END PRIVATE KEY-----";
    assert_eq!(redact_url(mismatched), "***");

    let two = "-----BEGIN A-----x-----END A----- -----BEGIN B-----y-----END B-----";
    assert_eq!(redact_url(two), "*** ***");

    let illegal = "-----BEGIN BAD_LABEL-----x-----END BAD_LABEL-----";
    assert_eq!(redact_url(illegal), illegal);

    let over_bound = format!(
        "-----BEGIN {}-----x-----END {}-----",
        "A".repeat(65),
        "A".repeat(65)
    );
    assert_eq!(redact_url(&over_bound), over_bound);
}

#[test]
fn test_redact_url_when_empty_url_then_empty_string_returned() {
    assert_eq!(redact_url(""), "");
}

#[test]
fn test_redact_url_when_credential_contains_zero_width_obfuscation_then_entire_original_span_is_masked()
 {
    let value = "to\u{200B}ken=abc123";
    assert!(contains_secret_like_material(value));
    assert_eq!(redact_url(value), "***");
}

const OLD_URL_USERINFO: &str = r"([a-z][a-z0-9+.-]*://)([^/@\s:]{1,256})(?::([^/@\s]{0,256}))?@";

fn old_url_userinfo_oracle(value: &str) -> String {
    static OLD: LazyLock<regress::Regex> = LazyLock::new(|| {
        regress::Regex::from_unicode(OLD_URL_USERINFO.encode_utf16().map(u32::from), "gi")
            .expect("the old URL userinfo pattern is valid")
    });
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut cursor = 0;
    let mut from = 0;
    while let Some(matched) = OLD.find_from_ucs2(&units, from).next() {
        let range = matched.range();
        let scheme = matched.group(1).expect("group 1 is the scheme");
        out.extend_from_slice(&units[cursor..range.start]);
        out.extend_from_slice(&units[scheme.start..scheme.end]);
        out.extend(MASK.encode_utf16());
        if matched.group(3).is_some() {
            out.push(u16::from(b':'));
            out.extend(MASK.encode_utf16());
        }
        out.push(u16::from(b'@'));
        cursor = range.end;
        from = range.end;
    }
    out.extend_from_slice(&units[cursor..]);
    String::from_utf16_lossy(&out)
}

fn real_url_shapes() -> Vec<String> {
    vec![
        "https://user:pass@example.com/x".to_string(),
        "https://user@example.com".to_string(),
        "chrome-extension://u:p@abc/path".to_string(),
        "git+ssh://git@github.com:a/b.git".to_string(),
        "HtTpS://User:Pw@Host".to_string(),
        "http://u:p@[::1]:8080/".to_string(),
        "https://example.com/no-userinfo".to_string(),
        "file:///etc/passwd".to_string(),
        "see https://a:b@h and ftp://c@d.".to_string(),
        "https://u:@h".to_string(),
        "https://:p@h".to_string(),
        format!("https://{}@h", "u".repeat(300)),
        format!("{}://u:p@h", "s".repeat(40)),
        "weird+.-scheme9://u:p@h".to_string(),
        "1http://u:p@h".to_string(),
        "-x://u@h".to_string(),
        "x://a://b@".to_string(),
        "a://b://c@d".to_string(),
        "https://\u{fc}:\u{e9}@h\u{f6}st/\u{e4}".to_string(),
        "s://u:p@h s://v@h".to_string(),
    ]
}

#[test]
fn test_mask_url_userinfo_when_real_url_shapes_then_the_output_is_the_old_patterns() {
    for value in real_url_shapes() {
        assert_eq!(
            mask_url_userinfo(&value),
            old_url_userinfo_oracle(&value),
            "input {value:?}"
        );
    }
}

#[test]
fn test_mask_url_userinfo_when_generated_corpus_then_every_input_is_masked_as_the_old_pattern_masked_it()
 {
    let seed: u64 = 14_864;
    let mut state = seed;
    let mut next = || {
        state = (state.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % 2_147_483_648;
        state as f64 / 2_147_483_648.0
    };
    let alphabet = [
        "a", "Z", "q", "9", "+", ".", "-", ":", "/", "@", " ", "\n", "\u{e9}", "\u{fc}", "://",
        "https://", "u:p@",
    ];
    for index in 0..20_000 {
        let mut value = String::new();
        let length = 1 + (next() * 30.0) as usize;
        for _ in 0..length {
            value.push_str(alphabet[(next() * alphabet.len() as f64) as usize % alphabet.len()]);
        }
        assert_eq!(
            mask_url_userinfo(&value),
            old_url_userinfo_oracle(&value),
            "seed {seed}, input #{index} {value:?}"
        );
    }
}

#[test]
fn test_mask_url_userinfo_when_scheme_run_doubles_then_the_characters_read_at_most_double() {
    let shapes: [&dyn Fn(usize) -> String; 5] = [
        &|size| format!("x://{}", "u".repeat(size)),
        &|size| format!("{}://", "u".repeat(size)),
        &|size| format!("https://{}:", "u".repeat(size)),
        &|size| format!("{}{}", "u".repeat(size), "9://".repeat(size)),
        &|size| "a://".repeat(size),
    ];
    for (index, shape) in shapes.iter().enumerate() {
        let reads = |value: &str| {
            let mut reads = 0;
            let masked = mask_url_userinfo_counting(value, &mut reads);
            assert_eq!(masked, value, "shape #{index} masks nothing");
            reads
        };
        let small = reads(&shape(5_000));
        let large = reads(&shape(10_000));
        assert!(small > 0, "shape #{index} read nothing");
        assert!(
            large <= 2 * small + 16,
            "shape #{index}: {small} -> {large} reads"
        );
    }
}
