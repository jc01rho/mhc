//! Port of omo-cpa `src/redact.ts`. Secret hygiene: neither the inference key nor the
//! management key may ever reach the screen, a log line, or JSON output.

const REDACTED: &str = "<REDACTED>";

/// Replace anything that looks like a credential with a fixed marker.
///
/// The TS module applies four regexes. Rust has no regex crate in this crate's dependency set,
/// so the equivalent scans run as hand-written matchers over the same alphabets.
pub fn redact(text: &str) -> String {
    redact_bearer(&redact_senpi(&redact_sk(&redact_jwt(text))))
}

/// Render a key as a non-reversible presence indicator. Never returns the key.
pub fn key_presence(key: Option<&str>) -> String {
    match key {
        Some(key) if !key.is_empty() => format!("설정됨 ({}자)", key.chars().count()),
        _ => "없음".to_owned(),
    }
}

fn redact_senpi(text: &str) -> String {
    redact_prefixed(text, "senpi-", 4, is_senpi_body)
}

fn redact_sk(text: &str) -> String {
    redact_prefixed(text, "sk-", 8, is_senpi_body)
}

fn is_senpi_body(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn redact_prefixed(text: &str, prefix: &str, min_body: usize, body: fn(u8) -> bool) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || !is_senpi_body(bytes[index - 1]);
        if at_word_start
            && bytes[index..].starts_with(prefix.as_bytes())
            && run_length(&bytes[index + prefix.len()..], body) >= min_body
        {
            out.push_str(REDACTED);
            index += prefix.len() + run_length(&bytes[index + prefix.len()..], body);
            continue;
        }
        let advance = text[index..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[index..index + advance]);
        index += advance;
    }
    out
}

fn redact_bearer(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || !is_senpi_body(bytes[index - 1]);
        let matched = at_word_start
            && bytes[index..]
                .get(..6)
                .is_some_and(|slice| slice.eq_ignore_ascii_case(b"bearer"))
            && bytes.get(index + 6) == Some(&b' ')
            && run_length(&bytes[index + 7..], is_bearer_body) >= 8;
        if matched {
            let consumed = run_length(&bytes[index + 7..], is_bearer_body);
            let mut end = index + 7 + consumed;
            while bytes.get(end) == Some(&b'=') {
                end += 1;
            }
            out.push_str(REDACTED);
            index = end;
            continue;
        }
        let advance = text[index..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[index..index + advance]);
        index += advance;
    }
    out
}

fn is_bearer_body(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'-')
}

fn redact_jwt(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || !is_senpi_body(bytes[index - 1]);
        if at_word_start
            && bytes[index..].starts_with(b"eyJ")
            && let Some(end) = jwt_end(&bytes[index..])
        {
            out.push_str(REDACTED);
            index += end;
            continue;
        }
        let advance = text[index..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[index..index + advance]);
        index += advance;
    }
    out
}

/// `eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+`
fn jwt_end(bytes: &[u8]) -> Option<usize> {
    let first = run_length(&bytes[3..], is_senpi_body);
    if first < 10 || bytes.get(3 + first) != Some(&b'.') {
        return None;
    }
    let second_start = 3 + first + 1;
    let second = run_length(&bytes[second_start..], is_senpi_body);
    if second == 0 || bytes.get(second_start + second) != Some(&b'.') {
        return None;
    }
    let third_start = second_start + second + 1;
    let third = run_length(&bytes[third_start..], is_senpi_body);
    if third == 0 {
        return None;
    }
    Some(third_start + third)
}

fn run_length(bytes: &[u8], predicate: fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|byte| predicate(**byte)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_each_credential_shape() {
        assert_eq!(redact("key senpi-abc12345"), "key <REDACTED>");
        assert_eq!(redact("key sk-abcdefgh123"), "key <REDACTED>");
        assert_eq!(redact("auth Bearer abcdefgh12=="), "auth <REDACTED>");
        assert_eq!(
            redact("jwt eyJabcdefghijk.abcdefgh.abcdefgh done"),
            "jwt <REDACTED> done"
        );
    }

    #[test]
    fn keeps_short_or_embedded_lookalikes() {
        assert_eq!(redact("senpi-abc"), "senpi-abc");
        assert_eq!(redact("mysenpi-abcdefgh"), "mysenpi-abcdefgh");
        assert_eq!(redact("Bearer short"), "Bearer short");
        assert_eq!(redact("normal text"), "normal text");
    }

    #[test]
    fn key_presence_never_returns_the_key() {
        assert_eq!(key_presence(Some("abcd")), "설정됨 (4자)");
        assert_eq!(key_presence(Some("")), "없음");
        assert_eq!(key_presence(None), "없음");
    }
}
