//! Port of senpi `packages/coding-agent/src/core/credential-proxy/declarations.ts` (6d4426a28; senpi#6828).
//!
//! `credentialProxy.credentials` (senpi#6820): which engine environment variables are credentials that
//! agent-spawned processes never receive, and the one upstream each may be used against through the loopback
//! proxy. Read from the user's global settings only: a project's settings could otherwise point a credential at
//! a host of its choosing.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialDeclaration {
    /// The engine environment variable holding the value; agent children get a placeholder instead.
    pub env: String,
    /// The only upstream the value is sent to: `https://host[:port][/base/path]` (plain `http` only for loopback).
    pub origin: String,
    /// The request header the proxy sets, lowercase. Default `authorization`.
    pub header: String,
    /// Prefix written before the value, separated by one space (`Bearer`).
    pub scheme: Option<String>,
    /// A child environment variable set to the proxy route, for clients that read a base URL.
    pub base_url_env: Option<String>,
    /// When given, only request paths (below `origin`'s path) starting with one of these are forwarded.
    pub path_prefixes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedCredentialDeclarations {
    pub credentials: Vec<CredentialDeclaration>,
    /// Every `env` name found, valid entry or not: these are withheld from children even when an entry is rejected.
    pub withheld_names: Vec<String>,
    pub problems: Vec<String>,
}

const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];
/// Headers the proxy owns or that describe the hop, never a credential header.
const RESERVED_HEADERS: [&str; 10] = [
    "connection",
    "content-length",
    "content-encoding",
    "host",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An HTTP token (`!#$%&'*+.^_`|~`, digits, letters, `-`).
fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c))
}

fn parse_origin(value: Option<&Value>) -> Result<String, &'static str> {
    let Some(text) = value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    else {
        return Err("origin must be a URL string");
    };
    let url = reqwest::Url::parse(text).map_err(|_| "origin is not a valid URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("origin must not carry credentials");
    }
    if url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return Err("origin must not have a query or fragment");
    }
    let loopback = url
        .host_str()
        .is_some_and(|host| LOOPBACK_HOSTS.contains(&host));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("origin must use https (plain http only for a loopback host)");
    }
    Ok(format!(
        "{}{}",
        url.origin().ascii_serialization(),
        url.path().trim_end_matches('/')
    ))
}

fn parse_path_prefixes(value: Option<&Value>) -> Result<Option<Vec<String>>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(list) = value.as_array().filter(|list| !list.is_empty()) else {
        return Err("pathPrefixes must be a non-empty array");
    };
    let mut prefixes = Vec::new();
    for entry in list {
        match entry.as_str() {
            Some(prefix) if prefix.starts_with('/') && !has_unsafe_path_segment(prefix) => {
                prefixes.push(prefix.to_owned());
            }
            _ => return Err("each pathPrefixes entry must be a plain absolute path"),
        }
    }
    Ok(Some(prefixes))
}

/// A path the proxy refuses: dot segments, encoded separators or dots, backslashes. `..;x` and `.;x` normalise
/// to `..`/`.` on servers that strip path parameters (Tomcat-style).
pub fn has_unsafe_path_segment(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path.contains('\\')
    {
        return true;
    }
    path.split('/')
        .any(|segment| segment == "." || segment.starts_with("..") || segment.starts_with(".;"))
}

pub fn parse_credential_declarations(raw: Option<&Value>) -> ParsedCredentialDeclarations {
    let Some(list) = raw
        .and_then(|raw| raw.as_object())
        .and_then(|raw| raw.get("credentials"))
    else {
        return ParsedCredentialDeclarations::default();
    };
    let Some(list) = list.as_array() else {
        return ParsedCredentialDeclarations {
            problems: vec!["credentialProxy.credentials must be an array".into()],
            ..Default::default()
        };
    };
    let mut parsed = ParsedCredentialDeclarations::default();
    let mut base_url_names: Vec<String> = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        let at = format!("credentialProxy.credentials[{index}]");
        let Some(entry) = entry.as_object() else {
            parsed.problems.push(format!("{at} must be an object"));
            continue;
        };
        let Some(env) = entry
            .get("env")
            .and_then(Value::as_str)
            .filter(|env| is_env_name(env))
        else {
            parsed
                .problems
                .push(format!("{at}.env must be an environment variable name"));
            continue;
        };
        let fail = |parsed: &mut ParsedCredentialDeclarations, problem: &str| {
            parsed.problems.push(format!(
                "{at} ({env}): {problem}; the value is withheld and not proxied"
            ));
        };
        if parsed.withheld_names.iter().any(|name| name == env) {
            fail(&mut parsed, "declared more than once");
            continue;
        }
        parsed.withheld_names.push(env.to_owned());
        let origin = match parse_origin(entry.get("origin")) {
            Ok(origin) => origin,
            Err(problem) => {
                fail(&mut parsed, problem);
                continue;
            }
        };
        let header = match entry.get("header") {
            None => "authorization".to_owned(),
            Some(Value::String(header))
                if is_token(header)
                    && !RESERVED_HEADERS.contains(&header.to_ascii_lowercase().as_str()) =>
            {
                header.to_ascii_lowercase()
            }
            Some(_) => {
                fail(
                    &mut parsed,
                    "header must be a request header name the proxy may set",
                );
                continue;
            }
        };
        let scheme = match entry.get("scheme") {
            None => (header == "authorization").then(|| "Bearer".to_owned()),
            Some(Value::String(scheme)) if is_token(scheme) => Some(scheme.clone()),
            Some(_) => {
                fail(&mut parsed, "scheme must be a single token such as Bearer");
                continue;
            }
        };
        let base_url_env = match entry.get("baseUrlEnv") {
            None => None,
            Some(Value::String(name)) if is_env_name(name) => Some(name.clone()),
            Some(_) => {
                fail(
                    &mut parsed,
                    "baseUrlEnv must be an environment variable name",
                );
                continue;
            }
        };
        if let Some(name) = &base_url_env
            && (base_url_names.contains(name) || name == env)
        {
            fail(&mut parsed, "baseUrlEnv is already used");
            continue;
        }
        let path_prefixes = match parse_path_prefixes(entry.get("pathPrefixes")) {
            Ok(prefixes) => prefixes,
            Err(problem) => {
                fail(&mut parsed, problem);
                continue;
            }
        };
        if let Some(name) = &base_url_env {
            base_url_names.push(name.clone());
        }
        parsed.credentials.push(CredentialDeclaration {
            env: env.to_owned(),
            origin,
            header,
            scheme,
            base_url_env,
            path_prefixes,
        });
    }
    let conflicts: Vec<String> = parsed
        .credentials
        .iter()
        .filter_map(|credential| credential.base_url_env.clone())
        .filter(|name| parsed.withheld_names.contains(name))
        .map(|name| format!("credentialProxy: {name} is both a credential and a baseUrlEnv"))
        .collect();
    parsed.problems.extend(conflicts);
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_valid_declaration_takes_its_defaults() {
        let parsed = parse_credential_declarations(Some(&json!({"credentials":[
            {"env":"OPENAI_API_KEY","origin":"https://api.openai.com/v1/","baseUrlEnv":"OPENAI_BASE_URL"},
            {"env":"X_KEY","origin":"http://localhost:8080","header":"X-Api-Key"}
        ]})));
        assert_eq!(parsed.problems, Vec::<String>::new());
        assert_eq!(parsed.withheld_names, vec!["OPENAI_API_KEY", "X_KEY"]);
        assert_eq!(parsed.credentials[0].origin, "https://api.openai.com/v1");
        assert_eq!(parsed.credentials[0].header, "authorization");
        assert_eq!(parsed.credentials[0].scheme.as_deref(), Some("Bearer"));
        assert_eq!(
            parsed.credentials[0].base_url_env.as_deref(),
            Some("OPENAI_BASE_URL")
        );
        assert_eq!(parsed.credentials[1].origin, "http://localhost:8080");
        assert_eq!(parsed.credentials[1].header, "x-api-key");
        assert_eq!(parsed.credentials[1].scheme, None);
    }

    #[test]
    fn a_rejected_entry_is_still_withheld() {
        let parsed = parse_credential_declarations(Some(&json!({"credentials":[
            {"env":"A","origin":"http://example.com"},
            {"env":"B","origin":"https://user:pw@example.com"},
            {"env":"C","origin":"https://example.com","header":"Host"},
            {"env":"D","origin":"https://example.com","pathPrefixes":["/v1/../admin"]},
            {"env":"A","origin":"https://example.com"},
            {"env":"1bad","origin":"https://example.com"},
            "nope"
        ]})));
        assert!(parsed.credentials.is_empty());
        assert_eq!(parsed.withheld_names, vec!["A", "B", "C", "D"]);
        assert_eq!(parsed.problems.len(), 7);
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for path in [
            "/a/../b",
            "/a/%2e%2e/b",
            "/a%2Fb",
            "/a\\b",
            "/..;x",
            "/.;x",
            "/./a",
        ] {
            assert!(has_unsafe_path_segment(path), "{path}");
        }
        for path in ["/v1/chat", "/", "/a.b/c..d"] {
            assert!(!has_unsafe_path_segment(path), "{path}");
        }
    }

    #[test]
    fn absent_settings_declare_nothing() {
        assert_eq!(
            parse_credential_declarations(None),
            ParsedCredentialDeclarations::default()
        );
        assert_eq!(
            parse_credential_declarations(Some(&json!({"credentials":{}}))).problems,
            vec!["credentialProxy.credentials must be an array"]
        );
    }
}
