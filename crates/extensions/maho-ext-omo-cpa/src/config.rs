//! Port of omo-cpa `src/config.ts`. The plugin owns its own models through `registerProvider`, so
//! it reads none of omo's routing configuration: the tier split is derived from the live catalog,
//! and the inference key arrives from `/login` or the environment.

use crate::management_key;
use crate::types::CpaConfig;
use std::path::{Path, PathBuf};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8317";

/// Strip the API-version suffix so every form collapses to one server root.
pub fn to_root(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    for suffix in ["/v1beta", "/v1", "/v0"] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.to_owned();
        }
    }
    trimmed.to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: CpaConfig,
    /// Inference key. Kept out of `CpaConfig` so it cannot leak into reports.
    pub api_key: Option<String>,
    pub management_key: Option<String>,
    pub reason: Option<String>,
}

/// Everything the environment contributes, injected so tests never mutate the process env.
#[derive(Clone, Debug, Default)]
pub struct ConfigEnv {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub management_key: Option<String>,
}

impl ConfigEnv {
    pub fn from_process() -> Self {
        Self {
            base_url: std::env::var("OMO_CPA_BASE_URL").ok(),
            api_key: std::env::var("OMO_CPA_API_KEY").ok(),
            management_key: std::env::var("OMO_CPA_MANAGEMENT_KEY").ok(),
        }
    }
}

/// Resolve endpoint and keys. An absent inference key is not an error here: the extension receives
/// one from its oauth credential.
pub fn load_config(
    api_key_override: Option<&str>,
    base_url_override: Option<&str>,
    management_key_path: &Path,
    env: &ConfigEnv,
) -> LoadedConfig {
    let base = trimmed(base_url_override)
        .or_else(|| trimmed(env.base_url.as_deref()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let root = to_root(&base);
    let api_key = trimmed(api_key_override).or_else(|| trimmed(env.api_key.as_deref()));
    let management_key = trimmed(env.management_key.as_deref())
        .or_else(|| management_key::read_management_key(management_key_path));
    let has_api_key = api_key.is_some();
    let reason = if has_api_key {
        None
    } else {
        Some("추론 키 없음 — /login cliproxyapi 또는 OMO_CPA_API_KEY".to_owned())
    };

    LoadedConfig {
        config: CpaConfig {
            root,
            providers: Vec::new(),
            source: "env".to_owned(),
            has_api_key,
            has_management_key: management_key.is_some(),
        },
        api_key,
        management_key,
        reason,
    }
}

/// `loadConfig()` against the process environment and the default management-key path.
pub fn load_config_from_process(
    api_key_override: Option<&str>,
    base_url_override: Option<&str>,
) -> LoadedConfig {
    load_config(
        api_key_override,
        base_url_override,
        &default_management_key_path(),
        &ConfigEnv::from_process(),
    )
}

pub fn default_management_key_path() -> PathBuf {
    management_key::default_management_key_path()
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ConfigEnv {
        ConfigEnv::default()
    }

    #[test]
    fn strips_every_api_version_suffix() {
        assert_eq!(to_root("http://host:8317"), "http://host:8317");
        assert_eq!(to_root("http://host:8317/"), "http://host:8317");
        assert_eq!(to_root("http://host:8317/v1"), "http://host:8317");
        assert_eq!(to_root("http://host:8317/v1beta"), "http://host:8317");
        assert_eq!(to_root("http://host:8317/v0/"), "http://host:8317");
        assert_eq!(to_root("http://host:8317/v1/x"), "http://host:8317/v1/x");
    }

    #[test]
    fn override_wins_over_env_and_env_over_the_default() {
        let mut with_env = env();
        with_env.base_url = Some("http://env:9999/v1".into());
        let loaded = load_config(
            None,
            Some("http://override:1111/"),
            Path::new("/nonexistent/key"),
            &with_env,
        );
        assert_eq!(loaded.config.root, "http://override:1111");
        let loaded = load_config(None, None, Path::new("/nonexistent/key"), &with_env);
        assert_eq!(loaded.config.root, "http://env:9999");
        let loaded = load_config(None, None, Path::new("/nonexistent/key"), &env());
        assert_eq!(loaded.config.root, DEFAULT_BASE_URL);
    }

    #[test]
    fn a_missing_key_explains_itself_without_failing() {
        let loaded = load_config(None, None, Path::new("/nonexistent/key"), &env());
        assert!(!loaded.config.has_api_key);
        assert!(loaded.api_key.is_none());
        assert!(
            loaded
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("/login cliproxyapi"))
        );
    }

    #[test]
    fn the_env_key_is_trimmed_and_flags_presence_only() {
        let mut env = env();
        env.api_key = Some("  senpi-secret  ".into());
        let loaded = load_config(None, None, Path::new("/nonexistent/key"), &env);
        assert_eq!(loaded.api_key.as_deref(), Some("senpi-secret"));
        assert!(loaded.config.has_api_key);
        assert!(loaded.reason.is_none());
    }
}
