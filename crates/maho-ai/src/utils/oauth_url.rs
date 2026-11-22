use url::Url;

pub fn redact_oauth_url(value: &str) -> Result<String, url::ParseError> {
    let mut url = Url::parse(value)?;
    let has_hint = url.query_pairs().any(|(key, _)| key == "id_token_hint");
    if !has_hint {
        return Ok(value.to_owned());
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "id_token_hint")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (k, v) in &kept {
            pairs.append_pair(k, v);
        }
    }
    if kept.is_empty() {
        url.set_query(None);
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_id_token_hint_and_preserves_other_query_params() {
        let original = "https://auth.openai.com/api/accounts/authorize?client_id=cid&id_token_hint=secret.jwt.token&state=s1";
        let redacted = redact_oauth_url(original).expect("valid url");
        assert_eq!(
            redacted,
            "https://auth.openai.com/api/accounts/authorize?client_id=cid&state=s1"
        );
        assert!(!redacted.contains("secret.jwt.token"));

        let without_hint = "https://auth.openai.com/api/accounts/authorize?client_id=cid&state=s1";
        assert_eq!(
            redact_oauth_url(without_hint).expect("valid url"),
            without_hint
        );
    }
}
