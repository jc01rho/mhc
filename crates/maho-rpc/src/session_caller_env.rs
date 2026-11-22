use crate::host_daemon_env::is_transient_daemon_env_name;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type SessionEnv = BTreeMap<String, String>;

pub const CALLER_ENV_NAMES_ENV: &str = "SENPI_RPC_CALLER_ENV_NAMES";
pub const DEFAULT_CALLER_ENV_NAMES: [&str; 1] = ["OMOCAT_LANE"];

const MAX_SESSION_ENV_ENTRIES: usize = 16;
const MAX_SESSION_ENV_VALUE_LENGTH: usize = 4096;
const MAX_ECHOED_NAME_LENGTH: usize = 64;
const ENV_NAME_PATTERN: &str = "^[A-Z_][A-Z0-9_]*$";

fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == b'_') {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn echoed_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() > MAX_ECHOED_NAME_LENGTH {
        let prefix: String = chars[..MAX_ECHOED_NAME_LENGTH].iter().collect();
        format!("{prefix}...")
    } else {
        name.to_owned()
    }
}

pub fn caller_env_passable_names(host_env: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = DEFAULT_CALLER_ENV_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    if let Some(raw_list) = host_env.get(CALLER_ENV_NAMES_ENV) {
        for raw in raw_list.split(',') {
            let name = raw.trim();
            if is_valid_env_name(name) && !is_transient_daemon_env_name(name) {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

pub fn caller_env_passable_names_from_process() -> BTreeSet<String> {
    let host_env: BTreeMap<String, String> = std::env::vars().collect();
    caller_env_passable_names(&host_env)
}

pub fn session_env_shape_error(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let Some(entries) = value.as_object() else {
        return Some("env must be an object of strings.".into());
    };
    if entries.len() > MAX_SESSION_ENV_ENTRIES {
        return Some(format!(
            "env allows at most {MAX_SESSION_ENV_ENTRIES} entries."
        ));
    }
    for (name, entry) in entries {
        if !is_valid_env_name(name) {
            return Some(format!(
                "env name \"{}\" must match {ENV_NAME_PATTERN}.",
                echoed_name(name)
            ));
        }
        let Some(text) = entry.as_str() else {
            return Some(format!("env.{} must be a string.", echoed_name(name)));
        };
        if text.contains('\0') {
            return Some(format!("env.{} must not contain NUL.", echoed_name(name)));
        }
        if text.encode_utf16().count() > MAX_SESSION_ENV_VALUE_LENGTH {
            return Some(format!(
                "env.{} must be at most {MAX_SESSION_ENV_VALUE_LENGTH} characters.",
                echoed_name(name)
            ));
        }
    }
    None
}

pub fn session_env_error(
    value: Option<&Value>,
    host_env: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(shape) = session_env_shape_error(value) {
        return Some(shape);
    }
    let entries = value?.as_object()?;
    let passable = caller_env_passable_names(host_env);
    let passable_list: Vec<&str> = passable.iter().map(String::as_str).collect();
    for name in entries.keys() {
        if !passable.contains(name) {
            return Some(format!(
                "env.{} is not a caller-passable name (passable: {}).",
                echoed_name(name),
                passable_list.join(", ")
            ));
        }
    }
    None
}

pub fn session_env_error_from_process(value: Option<&Value>) -> Option<String> {
    let host_env: BTreeMap<String, String> = std::env::vars().collect();
    session_env_error(value, &host_env)
}

pub fn parse_session_env(value: Option<&Value>) -> Option<SessionEnv> {
    let entries = value?.as_object()?;
    let mut env = SessionEnv::new();
    for (name, entry) in entries {
        if let Some(text) = entry.as_str() {
            env.insert(name.clone(), text.to_owned());
        }
    }
    (!env.is_empty()).then_some(env)
}

pub fn inherited_session_env(
    parent: Option<&SessionEnv>,
    requested: Option<&SessionEnv>,
) -> Option<SessionEnv> {
    let mut merged = parent.cloned().unwrap_or_default();
    if let Some(requested) = requested {
        for (name, value) in requested {
            merged.insert(name.clone(), value.clone());
        }
    }
    (!merged.is_empty()).then_some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn passable_names_default_to_omocat_lane_and_ignore_transient_or_invalid_names() {
        assert_eq!(
            caller_env_passable_names(&BTreeMap::new()),
            BTreeSet::from(["OMOCAT_LANE".to_owned()])
        );
        let configured = caller_env_passable_names(&BTreeMap::from([(
            CALLER_ENV_NAMES_ENV.to_owned(),
            " MY_TRACE , lower , 1BAD , PI_SESSION_ID , SENPI_ENGINE_TRUST , EXTRA_LANE "
                .to_owned(),
        )]));
        assert_eq!(
            configured,
            BTreeSet::from([
                "OMOCAT_LANE".to_owned(),
                "MY_TRACE".to_owned(),
                "EXTRA_LANE".to_owned(),
            ])
        );
    }

    #[test]
    fn validates_shape_and_passable_set_without_leaking_values() {
        let host_env = BTreeMap::new();
        assert_eq!(session_env_error(None, &host_env), None);
        assert_eq!(
            session_env_error(Some(&json!({"OMOCAT_LANE": "lane-1"})), &host_env),
            None
        );
        assert_eq!(
            session_env_error(Some(&json!("not-an-object")), &host_env).as_deref(),
            Some("env must be an object of strings.")
        );
        assert_eq!(
            session_env_error(Some(&json!({"OMOCAT_LANE": 42})), &host_env).as_deref(),
            Some("env.OMOCAT_LANE must be a string.")
        );
        assert_eq!(
            session_env_error(Some(&json!({"OMOCAT_LANE": "a\0b"})), &host_env).as_deref(),
            Some("env.OMOCAT_LANE must not contain NUL.")
        );
        let refusal = session_env_error(
            Some(&json!({"SECRET_KEY": "super-secret-value"})),
            &host_env,
        )
        .expect("refused");
        assert_eq!(
            refusal,
            "env.SECRET_KEY is not a caller-passable name (passable: OMOCAT_LANE)."
        );
        assert!(!refusal.contains("super-secret-value"));
    }

    #[test]
    fn inherited_session_env_merges_parent_and_child_with_child_winning() {
        let parent = SessionEnv::from([
            ("OMOCAT_LANE".to_owned(), "parent-lane".to_owned()),
            ("EXTRA".to_owned(), "from-parent".to_owned()),
        ]);
        let child = SessionEnv::from([("OMOCAT_LANE".to_owned(), "child-lane".to_owned())]);
        assert_eq!(inherited_session_env(None, None), None);
        assert_eq!(
            inherited_session_env(Some(&parent), None),
            Some(parent.clone())
        );
        assert_eq!(
            inherited_session_env(Some(&parent), Some(&child)),
            Some(SessionEnv::from([
                ("OMOCAT_LANE".to_owned(), "child-lane".to_owned()),
                ("EXTRA".to_owned(), "from-parent".to_owned()),
            ]))
        );
    }
}
