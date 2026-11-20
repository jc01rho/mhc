//! Port of omo-nyang `src/model-provider.ts`: the `omomeow-opus` provider every gateway-owned
//! session can switch to, registered unconditionally at startup with the full catalog.

use maho_ai::types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap};
use maho_ext_api::{ExtensionFailure, ProviderConfig, ProviderModelConfig};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const PROVIDER_NAME: &str = "omomeow-opus";
const SOURCE_PROVIDER: &str = "cliproxyapi-last";
const DEFAULT_MAX_TOKENS: u64 = 128_000;

pub struct ModelEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub context_window: u64,
    pub max_tokens: Option<u64>,
    /// True when `model_config` puts max into the id's thinkingLevelMap. Without that entry the runtime clamps a max request
    /// to high unless it infers max from the id, so it is false only for the ids it infers (see `inferred`).
    pub max_thinking: bool,
}

const fn model(
    id: &'static str,
    name: &'static str,
    context_window: u64,
    max_tokens: Option<u64>,
) -> ModelEntry {
    ModelEntry {
        id,
        name,
        context_window,
        max_tokens,
        max_thinking: true,
    }
}

const fn max_model(
    id: &'static str,
    name: &'static str,
    context_window: u64,
    max_tokens: u64,
) -> ModelEntry {
    ModelEntry {
        id,
        name,
        context_window,
        max_tokens: Some(max_tokens),
        max_thinking: true,
    }
}

/// An id the runtime already infers max for (the opus-5 and sonnet-5 families), so it needs no map entry. Every other id
/// declares max (owner, 2026-11-11: any model may run at medium, high or max): the proxy answered HTTP 200 to
/// reasoning.effort "max" for all of them on 2026-11-11 (accepted, not proof that max is applied upstream), and without the
/// entry the runtime clamps the request to high.
const fn inferred(entry: ModelEntry) -> ModelEntry {
    ModelEntry {
        max_thinking: false,
        ..entry
    }
}

/// Every id a gateway-owned session can be set to (the Jev catalog plus the recovery chain). Sessions
/// run with --no-model-fallback, so an id missing here is "Model not found", not a fallback.
pub const MODELS: &[ModelEntry] = &[
    max_model(
        "ollama-deepseek-v4.1-flash",
        "DeepSeek V4.1 Flash (OmOMeow)",
        256_000,
        64_000,
    ),
    inferred(model(
        "claude-opus-5-5",
        "Opus 5.5 (OmOMeow)",
        1_000_000,
        None,
    )),
    inferred(model(
        "claude-sonnet-5-5",
        "Sonnet 5.5 (OmOMeow)",
        1_000_000,
        None,
    )),
    model(
        "gemini-4-carbon-high",
        "Gemini 4 Carbon High (OmOMeow)",
        1_048_576,
        Some(250_000),
    ),
    model(
        "gemini-4-argon-high",
        "Gemini 4 Argon High (OmOMeow)",
        1_048_576,
        Some(250_000),
    ),
    model(
        "gemini-4-hydrogen-xhigh",
        "Gemini 4 Hydrogen XHigh (OmOMeow)",
        1_048_576,
        Some(250_000),
    ),
    model(
        "gemini-4-neon-xhigh",
        "Gemini 4 Neon XHigh (OmOMeow)",
        1_048_576,
        Some(250_000),
    ),
    model(
        "claude-haiku-5-5",
        "Haiku 5.5 (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    model("glm-5.3", "GLM 5.3 (OmOMeow)", 200_000, Some(64_000)),
    model(
        "glm-5.3-flash",
        "GLM 5.3 Flash (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    model(
        "gemini-3.8-flash-high",
        "Gemini 3.8 Flash High (OmOMeow)",
        1_000_000,
        None,
    ),
    max_model(
        "oc-deepseek-v4.1-flash-free",
        "DeepSeek V4.1 Flash Free (OmOMeow)",
        256_000,
        64_000,
    ),
    model(
        "higher-coding",
        "Higher Coding (OmOMeow)",
        1_000_000,
        Some(64_000),
    ),
    model(
        "lower-coding",
        "Lower Coding (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    inferred(model("claude-opus-5", "Opus 5 (OmOMeow)", 1_000_000, None)),
    model(
        "gemini-3.1-pro",
        "Gemini 3.1 Pro (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    model(
        "grok-4.7-high",
        "Grok 4.7 High (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    model(
        "mistral-large-4",
        "Mistral Large 4 (OmOMeow)",
        1_048_576,
        None,
    ),
    model(
        "muse-spark-1.3-high",
        "Muse Spark 1.3 (OmOMeow)",
        1_000_000,
        Some(64_000),
    ),
    model(
        "muse-wonder-1.0-high",
        "Muse Wonder (OmOMeow)",
        200_000,
        Some(64_000),
    ),
    model(
        "kimi-k3-high",
        "Kimi K3 High (OmOMeow)",
        256_000,
        Some(64_000),
    ),
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

/// One registered model, from either the list beside the entry file or the compiled one.
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    pub max_tokens: Option<u64>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
}

impl From<&ModelEntry> for ModelSpec {
    fn from(entry: &ModelEntry) -> Self {
        let thinking_level_map = entry.max_thinking.then(|| {
            let mut map = ThinkingLevelMap::new();
            map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
            map
        });
        ModelSpec {
            id: entry.id.to_owned(),
            name: entry.name.to_owned(),
            context_window: entry.context_window,
            max_tokens: entry.max_tokens,
            thinking_level_map,
        }
    }
}

/// The list the compiled binary carries; only the fallback when no manifest sits beside the entry file.
pub fn compiled_models() -> Vec<ModelSpec> {
    MODELS.iter().map(ModelSpec::from).collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    id: String,
    name: String,
    context_window: u64,
    max_tokens: Option<u64>,
    thinking_level_map: Option<BTreeMap<ModelThinkingLevel, Option<String>>>,
}

/// The file name omo-nyang writes beside `model-provider.ts` (`bun src/model-provider.ts`): the same list the TS
/// entry registers, so a model added there needs no new engine build. Before this the engine registered only the list
/// compiled into it, and a button for a newer model failed with "Model not found" (2026-11-20, gemini-4-neon-xhigh).
pub const MANIFEST_FILE: &str = "omomeow-models.json";

pub fn parse_manifest(text: &str) -> Result<Vec<ModelSpec>, String> {
    let entries: Vec<ManifestEntry> =
        serde_json::from_str(text).map_err(|error| format!("{MANIFEST_FILE}: {error}"))?;
    if entries.is_empty() || entries.iter().any(|entry| entry.id.is_empty()) {
        return Err(format!(
            "{MANIFEST_FILE}: needs at least one model, each with an id"
        ));
    }
    Ok(entries
        .into_iter()
        .map(|entry| ModelSpec {
            id: entry.id,
            name: entry.name,
            context_window: entry.context_window,
            max_tokens: entry.max_tokens,
            thinking_level_map: entry.thinking_level_map,
        })
        .collect())
}

/// The manifest beside the entry file when there is one and it parses, else the compiled list. A missing file is the
/// normal state of a machine that has not been redeployed; an unreadable or invalid one is reported and also falls
/// back, because refusing to start every session would be worse than the older list.
pub fn models_for(manifest: Option<&Path>) -> Vec<ModelSpec> {
    let Some(path) = manifest.filter(|path| path.is_file()) else {
        return compiled_models();
    };
    match std::fs::read_to_string(path)
        .map_err(|error| format!("{}: {error}", path.display()))
        .and_then(|text| parse_manifest(&text))
    {
        Ok(models) => models,
        Err(error) => {
            eprintln!("omomeow models: {error}; using the compiled list");
            compiled_models()
        }
    }
}

fn model_config(entry: &ModelSpec) -> ProviderModelConfig {
    let mut compat = serde_json::Map::new();
    compat.insert("supportsForcedToolChoice".into(), Value::Bool(false));
    let thinking_level_map = entry.thinking_level_map.clone();
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        default_thinking_level: None,
        id: entry.id.clone(),
        name: entry.name.clone(),
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
    load_provider_with(models_path, key_path, base_url_override, compiled_models())
}

pub fn load_provider_with(
    models_path: &Path,
    key_path: &Path,
    base_url_override: Option<&str>,
    models: Vec<ModelSpec>,
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
        models: Some(models.iter().map(model_config).collect()),
        ..ProviderConfig::default()
    })
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `~/.omo/agent/models.json` and `<dir of OMOCAT_CONFIG>/llm-api-key`, as the TS entry reads them.
pub fn default_paths() -> (PathBuf, PathBuf) {
    let config = std::env::var_os("OMOCAT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/omocat/config.json"));
    let directory = config.parent().map(Path::to_path_buf).unwrap_or_default();
    (
        home().join(".omo/agent/models.json"),
        directory.join("llm-api-key"),
    )
}

pub fn register(api: &maho_ext_api::ExtensionApi) -> Result<(), ExtensionFailure> {
    let (models_path, key_path) = default_paths();
    let override_url = std::env::var("OMOMEOW_PROVIDER_BASE_URL").ok();
    let manifest = crate::fleet_tool::script_directory(&api.registered.source_info.path)
        .map(|directory| directory.join(MANIFEST_FILE));
    let models = models_for(manifest.as_deref());
    let config = load_provider_with(&models_path, &key_path, override_url.as_deref(), models)
        .map_err(ExtensionFailure::new)?;
    api.register_provider(PROVIDER_NAME, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, contents: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omomeow-provider-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn base_url_comes_from_override_then_source_provider() {
        let models = temp(
            "models.json",
            r#"{"providers":{"cliproxyapi-last":{"baseUrl":"http://proxy/v1","apiKey":"k"}}}"#,
        );
        assert_eq!(
            proxy_base_url(&models, Some("http://hub/v1")).unwrap(),
            "http://hub/v1"
        );
        assert_eq!(
            proxy_base_url(&models, Some("")).unwrap(),
            "http://proxy/v1"
        );
        assert_eq!(proxy_base_url(&models, None).unwrap(), "http://proxy/v1");
        let other = temp(
            "other.json",
            r#"{"providers":{"x":{"baseUrl":"u","apiKey":"k"}}}"#,
        );
        assert!(
            proxy_base_url(&other, None)
                .unwrap_err()
                .contains("Existing proxy provider")
        );
    }

    #[test]
    fn provider_carries_every_model_with_its_limits() {
        let models = temp(
            "m2.json",
            r#"{"providers":{"cliproxyapi-last":{"baseUrl":"http://proxy/v1","apiKey":"k"}}}"#,
        );
        let key = temp("llm-api-key", "  secret-key\n");
        let config = load_provider(&models, &key, None).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("secret-key"));
        assert_eq!(config.api.as_deref(), Some("openai-responses"));
        assert_eq!(config.auth_header, Some(true));
        let list = config.models.unwrap();
        assert_eq!(list.len(), MODELS.len());
        let flash = list
            .iter()
            .find(|m| m.id == "ollama-deepseek-v4.1-flash")
            .unwrap();
        assert_eq!(flash.max_tokens, 64_000);
        assert_eq!(
            flash
                .thinking_level_map
                .as_ref()
                .unwrap()
                .get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_owned()))
        );
        let carbon = list
            .iter()
            .find(|m| m.id == "gemini-4-carbon-high")
            .unwrap();
        assert_eq!(
            carbon
                .thinking_level_map
                .as_ref()
                .unwrap()
                .get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_owned()))
        );
        for id in ["gemini-4-argon-high", "gemini-4-hydrogen-xhigh"] {
            let gemini = list.iter().find(|m| m.id == id).unwrap();
            assert_eq!(gemini.context_window, 1_048_576);
            assert_eq!(gemini.max_tokens, 250_000);
            assert_eq!(
                gemini
                    .thinking_level_map
                    .as_ref()
                    .unwrap()
                    .get(&ModelThinkingLevel::Max),
                Some(&Some("max".to_owned()))
            );
        }
        let opus = list.iter().find(|m| m.id == "claude-opus-5-5").unwrap();
        assert_eq!(opus.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(opus.thinking_level_map.is_none());
        assert_eq!(
            opus.compat
                .as_ref()
                .unwrap()
                .0
                .get("supportsForcedToolChoice"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn every_model_declares_max_except_the_ids_the_runtime_infers() {
        let without_max: Vec<&str> = MODELS
            .iter()
            .filter(|entry| !entry.max_thinking)
            .map(|entry| entry.id)
            .collect();
        assert_eq!(
            without_max,
            ["claude-opus-5-5", "claude-sonnet-5-5", "claude-opus-5"]
        );
    }

    #[test]
    fn empty_key_is_refused() {
        let models = temp(
            "m3.json",
            r#"{"providers":{"cliproxyapi-last":{"baseUrl":"u","apiKey":"k"}}}"#,
        );
        let key = temp("empty-key", "  \n");
        assert_eq!(
            load_provider(&models, &key, None).err().as_deref(),
            Some("OmOcat API key is empty")
        );
    }

    #[test]
    fn compiled_list_carries_neon_and_none_of_the_retired_beam_ids() {
        let ids: Vec<String> = compiled_models().into_iter().map(|spec| spec.id).collect();
        assert!(ids.iter().any(|id| id == "gemini-4-neon-xhigh"));
        for retired in ["oc-beam", "beam-free", "crouton-alpha-free"] {
            assert!(!ids.iter().any(|id| id == retired), "{retired}");
        }
    }

    #[test]
    fn manifest_entries_keep_limits_and_level_vetoes() {
        let models = parse_manifest(
            r#"[
              {"id":"future-model","name":"Future (OmOMeow)","contextWindow":123456,"maxTokens":7000,"thinkingLevelMap":{"max":"max"}},
              {"id":"free","name":"Free (OmOMeow)","contextWindow":256000,"thinkingLevelMap":{"off":null,"low":null,"max":"max"}},
              {"id":"inferred","name":"Inferred (OmOMeow)","contextWindow":1000000}
            ]"#,
        )
        .unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].max_tokens, Some(7_000));
        assert_eq!(
            models[0]
                .thinking_level_map
                .as_ref()
                .unwrap()
                .get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_owned()))
        );
        let free = models[1].thinking_level_map.as_ref().unwrap();
        assert_eq!(free.get(&ModelThinkingLevel::Off), Some(&None));
        assert_eq!(free.get(&ModelThinkingLevel::Low), Some(&None));
        assert!(models[2].thinking_level_map.is_none());
        assert!(parse_manifest("[]").is_err());
        assert!(parse_manifest("not json").is_err());
    }

    #[test]
    fn a_manifest_beside_the_entry_decides_the_registered_models() {
        let manifest = temp(
            MANIFEST_FILE,
            r#"[{"id":"future-model","name":"Future (OmOMeow)","contextWindow":123456}]"#,
        );
        let models = models_for(Some(&manifest));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "future-model");
        let provider = load_provider_with(
            &temp(
                "m4.json",
                r#"{"providers":{"cliproxyapi-last":{"baseUrl":"http://proxy/v1","apiKey":"k"}}}"#,
            ),
            &temp("manifest-key", "secret"),
            None,
            models,
        )
        .unwrap();
        let registered = provider.models.unwrap();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].id, "future-model");
        assert_eq!(registered[0].max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn without_a_usable_manifest_the_compiled_list_is_registered() {
        let compiled = compiled_models().len();
        assert_eq!(models_for(None).len(), compiled);
        let missing = std::env::temp_dir()
            .join("omomeow-no-such-manifest")
            .join(MANIFEST_FILE);
        assert_eq!(models_for(Some(&missing)).len(), compiled);
        let broken = temp("broken-models.json", "{not json");
        assert_eq!(models_for(Some(&broken)).len(), compiled);
    }
}
