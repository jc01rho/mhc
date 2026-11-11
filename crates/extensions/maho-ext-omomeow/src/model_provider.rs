//! Port of omo-nyang `src/model-provider.ts`: the `omomeow-opus` provider every gateway-owned
//! session can switch to, registered unconditionally at startup with the full catalog.

use maho_ai::types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap};
use maho_ext_api::{ExtensionFailure, ProviderConfig, ProviderModelConfig};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PROVIDER_NAME: &str = "omomeow-opus";
const SOURCE_PROVIDER: &str = "cliproxyapi-last";
const DEFAULT_MAX_TOKENS: u64 = 128_000;

pub struct ModelEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub context_window: u64,
    pub max_tokens: Option<u64>,
}

const fn model(id: &'static str, name: &'static str, context_window: u64, max_tokens: Option<u64>) -> ModelEntry {
    ModelEntry { id, name, context_window, max_tokens }
}

/// Every id a gateway-owned session can be set to (the Jev catalog plus the recovery chain). Sessions
/// run with --no-model-fallback, so an id missing here is "Model not found", not a fallback.
pub const MODELS: &[ModelEntry] = &[
    model("ollama-deepseek-v4.1-flash", "DeepSeek V4.1 Flash (OmOMeow)", 256_000, Some(64_000)),
    model("claude-opus-5-5", "Opus 5.5 (OmOMeow)", 1_000_000, None),
    model("claude-sonnet-5-5", "Sonnet 5.5 (OmOMeow)", 1_000_000, None),
    model("gemini-4-carbon-high", "Gemini 4 Carbon High (OmOMeow)", 1_048_576, Some(250_000)),
    model("claude-haiku-5-5", "Haiku 5.5 (OmOMeow)", 200_000, Some(64_000)),
    model("glm-5.3", "GLM 5.3 (OmOMeow)", 200_000, Some(64_000)),
    model("glm-5.3-flash", "GLM 5.3 Flash (OmOMeow)", 200_000, Some(64_000)),
    model("gemini-3.8-flash-high", "Gemini 3.8 Flash High (OmOMeow)", 1_000_000, None),
    model("oc-beam", "OC Beam (OmOMeow)", 872_000, Some(64_000)),
    model("beam-free", "Beam Free (OmOMeow)", 872_000, Some(64_000)),
    model("crouton-alpha-free", "Crouton Alpha Free (OmOMeow)", 1_048_576, Some(64_000)),
    model("oc-deepseek-v4.1-flash-free", "DeepSeek V4.1 Flash Free (OmOMeow)", 256_000, Some(64_000)),
    model("higher-coding", "Higher Coding (OmOMeow)", 1_000_000, Some(64_000)),
    model("lower-coding", "Lower Coding (OmOMeow)", 200_000, Some(64_000)),
    model("claude-opus-5", "Opus 5 (OmOMeow)", 1_000_000, None),
    model("gemini-3.1-pro", "Gemini 3.1 Pro (OmOMeow)", 200_000, Some(64_000)),
    model("grok-4.7-high", "Grok 4.7 High (OmOMeow)", 200_000, Some(64_000)),
    model("mistral-large-4", "Mistral Large 4 (OmOMeow)", 1_048_576, None),
    model("muse-spark-1.3-high", "Muse Spark 1.3 (OmOMeow)", 1_000_000, Some(64_000)),
    model("muse-wonder-1.0-high", "Muse Wonder (OmOMeow)", 200_000, Some(64_000)),
    model("kimi-k3-high", "Kimi K3 High (OmOMeow)", 256_000, Some(64_000)),
];

/// The shared proxy endpoint: the hub passes it in `OMOMEOW_PROVIDER_BASE_URL` (workers have no
/// proxy entry of their own); otherwise the `cliproxyapi-last` entry of models.json. Its credential
/// is never reused.
pub fn proxy_base_url(models_path: &Path, override_url: Option<&str>) -> Result<String, String> {
    if let Some(url) = override_url.filter(|url| !url.is_empty()) {
        return Ok(url.to_owned());
    }
    let text = std::fs::read_to_string(models_path)
        .map_err(|error| format!("{}: {error}", models_path.display()))?;
    let config: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{}: {error}", models_path.display()))?;
    config
        .get("providers")
        .and_then(|providers| providers.get(SOURCE_PROVIDER))
        .ok_or_else(|| "Existing proxy provider is required for OmOMeow models".to_owned())?
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{SOURCE_PROVIDER}.baseUrl must be a string"))
}

fn model_config(entry: &ModelEntry) -> ProviderModelConfig {
    let mut compat = serde_json::Map::new();
    compat.insert("supportsForcedToolChoice".into(), Value::Bool(false));
    // Every id takes max (owner, 2026-11-11: any model may run at medium, high or max, and the gateway's Jev call picks
    // the level). The proxy answered HTTP 200 to reasoning.effort "max" for all 21 ids on 2026-11-11. Without this map the
    // runtime only offers max to ids whose family it infers (opus-5, sonnet-5, fable-5) and clamps the rest to high.
    let mut thinking_level_map = ThinkingLevelMap::new();
    thinking_level_map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
    let thinking_level_map = Some(thinking_level_map);
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        default_thinking_level: None,
        id: entry.id.to_owned(),
        name: entry.name.to_owned(),
        upstream_model_id: None,
        api: None,
        base_url: None,
        reasoning: true,
        recover_text_tool_calls: None,
        thinking_level_map,
        input: vec![InputModality::Text],
        cost: ModelCost::default(),
        context_window: entry.context_window,
        max_tokens: entry.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        headers: None,
        extra_body: None,
        compat: Some(maho_ai::model::ModelCompat(compat)),
    }
}

pub fn load_provider(
    models_path: &Path,
    key_path: &Path,
    base_url_override: Option<&str>,
) -> Result<ProviderConfig, String> {
    let base_url = proxy_base_url(models_path, base_url_override)?;
    let api_key = std::fs::read_to_string(key_path)
        .map_err(|error| format!("{}: {error}", key_path.display()))?
        .trim()
        .to_owned();
    if api_key.is_empty() {
        return Err("OmOcat API key is empty".into());
    }
    Ok(ProviderConfig {
        base_url: Some(base_url),
        api_key: Some(api_key),
        api: Some("openai-responses".into()),
        auth_header: Some(true),
        models: Some(MODELS.iter().map(model_config).collect()),
        ..ProviderConfig::default()
    })
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `~/.omo/agent/models.json` and `<dir of OMOCAT_CONFIG>/llm-api-key`, as the TS entry reads them.
pub fn default_paths() -> (PathBuf, PathBuf) {
    let config = std::env::var_os("OMOCAT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/omocat/config.json"));
    let directory = config.parent().map(Path::to_path_buf).unwrap_or_default();
    (home().join(".omo/agent/models.json"), directory.join("llm-api-key"))
}

pub fn register(api: &maho_ext_api::ExtensionApi) -> Result<(), ExtensionFailure> {
    let (models_path, key_path) = default_paths();
    let override_url = std::env::var("OMOMEOW_PROVIDER_BASE_URL").ok();
    let config = load_provider(&models_path, &key_path, override_url.as_deref())
        .map_err(ExtensionFailure::new)?;
    api.register_provider(PROVIDER_NAME, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omomeow-provider-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn base_url_comes_from_override_then_source_provider() {
        let models = temp("models.json", r#"{"providers":{"cliproxyapi-last":{"baseUrl":"http://proxy/v1","apiKey":"k"}}}"#);
        assert_eq!(proxy_base_url(&models, Some("http://hub/v1")).unwrap(), "http://hub/v1");
        assert_eq!(proxy_base_url(&models, Some("")).unwrap(), "http://proxy/v1");
        assert_eq!(proxy_base_url(&models, None).unwrap(), "http://proxy/v1");
        let other = temp("other.json", r#"{"providers":{"x":{"baseUrl":"u","apiKey":"k"}}}"#);
        assert!(proxy_base_url(&other, None).unwrap_err().contains("Existing proxy provider"));
    }

    #[test]
    fn provider_carries_every_model_with_its_limits() {
        let models = temp("m2.json", r#"{"providers":{"cliproxyapi-last":{"baseUrl":"http://proxy/v1","apiKey":"k"}}}"#);
        let key = temp("llm-api-key", "  secret-key\n");
        let config = load_provider(&models, &key, None).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("secret-key"));
        assert_eq!(config.api.as_deref(), Some("openai-responses"));
        assert_eq!(config.auth_header, Some(true));
        let list = config.models.unwrap();
        assert_eq!(list.len(), MODELS.len());
        let flash = list.iter().find(|m| m.id == "ollama-deepseek-v4.1-flash").unwrap();
        assert_eq!(flash.max_tokens, 64_000);
        assert_eq!(
            flash.thinking_level_map.as_ref().unwrap().get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_owned()))
        );
        let opus = list.iter().find(|m| m.id == "claude-opus-5-5").unwrap();
        assert_eq!(opus.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(opus.thinking_level_map.is_none());
        assert_eq!(opus.compat.as_ref().unwrap().0.get("supportsForcedToolChoice"), Some(&Value::Bool(false)));
    }

    #[test]
    fn empty_key_is_refused() {
        let models = temp("m3.json", r#"{"providers":{"cliproxyapi-last":{"baseUrl":"u","apiKey":"k"}}}"#);
        let key = temp("empty-key", "  \n");
        assert_eq!(load_provider(&models, &key, None).err().as_deref(), Some("OmOcat API key is empty"));
    }
}
