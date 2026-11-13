//! Port of omo-cpa `src/pricing.ts`. Per-model call pricing resolved from the models.dev catalog.
//!
//! CPA's four list endpoints carry no price, so models.dev is the source. Matching is deliberately
//! conservative: a plausible-but-wrong price is worse than no price, and an unresolved id keeps the
//! zero cost the plugin has always published.

use crate::tier_types::CatalogModel;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
pub const DEFAULT_PRICING_TTL_MS: i64 = 6 * 60 * 60 * 1000;
pub const DEFAULT_PRICING_TIMEOUT_MS: u64 = 3_000;
pub const DEFAULT_PRICING_FAILURE_TTL_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

pub const ZERO_COST: ModelCost = ModelCost {
    input: 0.0,
    output: 0.0,
    cache_read: 0.0,
    cache_write: 0.0,
};

#[derive(Clone, Debug, PartialEq)]
pub struct PricingEntry {
    pub provider: String,
    pub model_id: String,
    pub status: Option<String>,
    pub cost: ModelCost,
}

/// Lowercased id -> entries offering it, and normalized display name -> entries carrying that name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PricingIndex {
    pub by_id: BTreeMap<String, Vec<PricingEntry>>,
    pub by_name: BTreeMap<String, Vec<PricingEntry>>,
}

const OFFICIAL_PROVIDERS: &[(&str, &[&str])] = &[
    ("openai", &["openai", "azure", "azure-cognitive-services"]),
    ("anthropic", &["anthropic", "google-vertex-anthropic"]),
    ("deepseek", &["deepseek", "siliconflow", "siliconflow-cn"]),
    (
        "glm",
        &["zai", "zhipuai", "zai-coding-plan", "zhipuai-coding-plan"],
    ),
    ("qwen", &["alibaba", "aliyun-bailian", "alibaba-cn"]),
    ("google", &["google", "google-vertex"]),
    ("xai", &["xai"]),
    (
        "minimax",
        &[
            "minimax",
            "minimax-coding-plan",
            "minimax-cn",
            "minimax-cn-coding-plan",
        ],
    ),
    (
        "moonshot",
        &["moonshotai", "kimi-for-coding", "moonshotai-cn"],
    ),
    ("mistral", &["mistral"]),
];

/// Family patterns, applied in order to the lowercased identity.
const FAMILY_PATTERNS: &[(&str, &[&str])] = &[
    ("openai", &["gpt", "chatgpt", "o1", "o3", "o4"]),
    ("anthropic", &["claude"]),
    ("deepseek", &["deepseek"]),
    ("glm", &["glm"]),
    ("qwen", &["qwen"]),
    ("google", &["gemini"]),
    ("xai", &["grok"]),
    ("minimax", &["minimax"]),
    ("moonshot", &["moonshot", "kimi"]),
    (
        "mistral",
        &[
            "mistral",
            "devstral",
            "codestral",
            "magistral",
            "ministral",
            "mixtral",
            "pixtral",
            "voxtral",
        ],
    ),
];

fn official_providers(family: &str) -> Option<&'static [&'static str]> {
    OFFICIAL_PROVIDERS
        .iter()
        .find(|(name, _)| *name == family)
        .map(|(_, providers)| *providers)
}

/// Subscription plans publish a nominal zero; it must never win a comparison.
fn is_plan_zero(entry: &PricingEntry) -> bool {
    if entry.cost.input != 0.0 || entry.cost.output != 0.0 {
        return false;
    }
    let provider = entry.provider.to_lowercase();
    provider.contains("coding-plan")
        || provider.contains("token-plan")
        || provider == "kimi-for-coding"
}

/// Cut a provider namespace: "kilo/xiaomi/mimo" -> "mimo". A ":free" suffix is part of the model id
/// on both sides, so it stays.
pub fn strip_model_prefix(model_id: &str) -> String {
    let trimmed = model_id.trim();
    if trimmed.is_empty() || trimmed.to_lowercase().starts_with("ft:") {
        return trimmed.to_owned();
    }
    match trimmed.rfind('/') {
        Some(slash) => trimmed[slash + 1..].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// models.dev writes a version dot where some ids carry a hyphen ("gemini-3.1" vs "gemini-3-1").
fn dot_to_dash(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut previous_dot = false;
    for character in id.chars() {
        if character == '.' && !previous_dot {
            out.push('-');
            previous_dot = true;
            continue;
        }
        out.push(character);
        previous_dot = false;
    }
    out
}

pub fn normalize_key(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn looks_like_id(token: &str) -> bool {
    let mut characters = token.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && token.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        && token.contains(['.', '-'])
}

/// Resolve a family from a live displayName that is itself already an id ("glm-5.3-flash"). Prose
/// names are skipped: "Free Models Router" is a router, not a vendor family.
fn family_from_name_token(display_name: &str) -> String {
    let token = strip_model_prefix(display_name.trim());
    if token.is_empty() || token.to_lowercase().starts_with("ft:") || !looks_like_id(&token) {
        return String::new();
    }
    family_of(&token, None)
}

fn family_of(model_id: &str, display_name: Option<&str>) -> String {
    let stripped = strip_model_prefix(model_id);
    let identity = if stripped.is_empty() {
        model_id.to_lowercase()
    } else {
        stripped.to_lowercase()
    };
    for (family, prefixes) in FAMILY_PATTERNS {
        if prefixes.iter().any(|prefix| identity.starts_with(prefix)) {
            return (*family).to_owned();
        }
    }
    display_name.map(family_from_name_token).unwrap_or_default()
}

fn candidate_keys(model_id: &str) -> Vec<String> {
    let stripped = strip_model_prefix(model_id);
    let mut keys: Vec<String> = Vec::new();
    for key in [
        model_id.to_owned(),
        stripped.clone(),
        dot_to_dash(model_id),
        dot_to_dash(&stripped),
    ] {
        let normalized = key.trim().to_lowercase();
        if !normalized.is_empty() && !keys.contains(&normalized) {
            keys.push(normalized);
        }
    }
    keys
}

fn read_cost(value: Option<&Value>) -> Option<ModelCost> {
    let raw = value?.as_object()?;
    let input = raw.get("input").and_then(Value::as_f64)?;
    let output = raw.get("output").and_then(Value::as_f64)?;
    if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
        return None;
    }
    let read = |key: &str| {
        raw.get(key)
            .and_then(Value::as_f64)
            .filter(|value| *value >= 0.0 && value.is_finite())
            .unwrap_or(0.0)
    };
    Some(ModelCost {
        input,
        output,
        cache_read: read("cache_read"),
        cache_write: read("cache_write"),
    })
}

fn push(map: &mut BTreeMap<String, Vec<PricingEntry>>, key: String, entry: &PricingEntry) {
    map.entry(key).or_default().push(entry.clone());
}

/// Build the lookup index from a parsed models.dev catalog. Pure: no network, no cache.
pub fn build_pricing_index(catalog: &Value) -> PricingIndex {
    let mut index = PricingIndex::default();
    let Some(providers) = catalog.as_object() else {
        return index;
    };

    for provider_catalog in providers.values() {
        let Some(provider) = provider_catalog.as_object() else {
            continue;
        };
        let provider_id = provider
            .get("id")
            .map(|value| match value.as_str() {
                Some(text) => text.trim().to_owned(),
                None => value.to_string(),
            })
            .unwrap_or_default();
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        if provider_id.is_empty() {
            continue;
        }

        for (key, raw) in models {
            let Some(record) = raw.as_object() else {
                continue;
            };
            let Some(cost) = read_cost(record.get("cost")) else {
                continue;
            };
            let model_id = record
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map_or_else(|| key.clone(), str::to_owned);
            let entry = PricingEntry {
                provider: provider_id.clone(),
                model_id: model_id.clone(),
                status: record
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                cost,
            };

            let mut ids: Vec<String> = vec![model_id, key.clone()];
            if let Some(canonical) = record.get("canonical_model_id").and_then(Value::as_str)
                && canonical.contains('/')
            {
                ids.push(canonical[canonical.rfind('/').map_or(0, |slash| slash + 1)..].to_owned());
            }
            for id in ids {
                let lower = id.trim().to_lowercase();
                for variant in [lower.clone(), dot_to_dash(&lower)] {
                    if !variant.is_empty() {
                        push(&mut index.by_id, variant, &entry);
                    }
                }
            }
            if let Some(name) = record.get("name").and_then(Value::as_str)
                && !name.trim().is_empty()
            {
                push(&mut index.by_name, normalize_key(name), &entry);
            }
        }
    }
    index
}

fn choose(
    model_id: &str,
    display_name: Option<&str>,
    candidates: &[PricingEntry],
) -> Option<ModelCost> {
    let mut unique: BTreeMap<String, &PricingEntry> = BTreeMap::new();
    for entry in candidates {
        unique.insert(format!("{}\u{0}{}", entry.provider, entry.model_id), entry);
    }
    let mut list: Vec<&PricingEntry> = unique.into_values().collect();
    if list.iter().any(|entry| !is_plan_zero(entry)) {
        list.retain(|entry| !is_plan_zero(entry));
    }

    // A price is only trusted when the model's vendor family is identifiable AND that family's
    // official catalog carries the row.
    let family = family_of(model_id, display_name);
    let official = official_providers(&family)?;
    list.retain(|entry| official.contains(&entry.provider.to_lowercase().as_str()));
    if list.is_empty() {
        return None;
    }
    let priority = |entry: &PricingEntry| -> usize {
        official
            .iter()
            .position(|provider| *provider == entry.provider.to_lowercase())
            .unwrap_or(official.len())
    };
    list.sort_by(|left, right| {
        priority(left)
            .cmp(&priority(right))
            .then_with(|| {
                i32::from(left.status.as_deref() == Some("deprecated"))
                    .cmp(&i32::from(right.status.as_deref() == Some("deprecated")))
            })
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    list.first().map(|entry| entry.cost)
}

/// Resolve one model's price. Exact-id keys win over an exact display-name key; nothing here does
/// fuzzy matching, so an unknown alias resolves to `None`.
pub fn lookup_cost(
    index: &PricingIndex,
    model_id: &str,
    display_name: Option<&str>,
) -> Option<ModelCost> {
    for key in candidate_keys(model_id) {
        if let Some(hits) = index.by_id.get(&key)
            && !hits.is_empty()
            && let Some(cost) = choose(model_id, display_name, hits)
        {
            return Some(cost);
        }
    }
    if let Some(name) = display_name.filter(|name| !name.trim().is_empty())
        && let Some(hits) = index.by_name.get(&normalize_key(name))
        && !hits.is_empty()
    {
        return choose(model_id, display_name, hits);
    }
    None
}

pub fn resolve_catalog_costs(
    index: &PricingIndex,
    models: &[CatalogModel],
) -> BTreeMap<String, ModelCost> {
    let mut costs = BTreeMap::new();
    for model in models {
        if let Some(cost) = lookup_cost(index, &model.id, model.display_name.as_deref()) {
            costs.insert(model.id.clone(), cost);
        }
    }
    costs
}

/// Pricing is best-effort: a fetch failure leaves every model at the zero cost it had before and
/// never blocks registration.
pub async fn fetch_pricing_index(
    client: &reqwest::Client,
    url: &str,
    timeout_ms: u64,
) -> Option<PricingIndex> {
    let response = client
        .get(url)
        .timeout(Duration::from_millis(timeout_ms))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    Some(build_pricing_index(&body))
}

/// `OMO_CPA_PRICING_URL` when set, else models.dev.
pub fn pricing_url() -> String {
    std::env::var("OMO_CPA_PRICING_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| MODELS_DEV_URL.to_owned())
}

/// `OMO_CPA_PRICING_TTL_MS`, falling back to the module default.
pub fn pricing_ttl_from_env() -> i64 {
    env_ms("OMO_CPA_PRICING_TTL_MS", DEFAULT_PRICING_TTL_MS)
}

/// `OMO_CPA_PRICING_FAILURE_TTL_MS`, falling back to the module default.
pub fn pricing_failure_ttl_from_env() -> i64 {
    env_ms(
        "OMO_CPA_PRICING_FAILURE_TTL_MS",
        DEFAULT_PRICING_FAILURE_TTL_MS,
    )
}

fn env_ms(name: &str, fallback: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or(fallback, |value| value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "anthropic": {"id": "anthropic", "models": {
                "claude-opus-5": {"id": "claude-opus-5", "name": "Claude Opus 5", "cost": {"input": 15, "output": 75, "cache_read": 1.5, "cache_write": 18.75}}
            }},
            "zai": {"id": "zai", "models": {
                "glm-5.3": {"id": "glm-5.3", "name": "GLM 5.3", "cost": {"input": 0.6, "output": 2.2}}
            }},
            "zai-coding-plan": {"id": "zai-coding-plan", "models": {
                "glm-5.3": {"id": "glm-5.3", "name": "GLM 5.3", "cost": {"input": 0, "output": 0}}
            }},
            "reseller": {"id": "reseller", "models": {
                "parrot": {"id": "parrot", "name": "Parrot", "cost": {"input": 9, "output": 9}}
            }},
            "google": {"id": "google", "models": {
                "gemini-3.1": {"id": "gemini-3.1", "name": "Gemini 3.1", "status": "deprecated", "cost": {"input": 2, "output": 8}},
                "gemini-3.1-preview": {"id": "gemini-3.1-preview", "name": "Gemini 3.1", "cost": {"input": 1, "output": 4}}
            }}
        })
    }

    #[test]
    fn exact_ids_resolve_through_the_official_vendor() {
        let index = build_pricing_index(&fixture());
        let cost = lookup_cost(&index, "claude-opus-5", None).expect("anthropic price");
        assert_eq!(cost.input, 15.0);
        assert_eq!(cost.cache_write, 18.75);
    }

    #[test]
    fn a_plan_zero_never_beats_a_real_price() {
        let index = build_pricing_index(&fixture());
        let cost = lookup_cost(&index, "glm-5.3", None).expect("glm price");
        assert_eq!(cost.input, 0.6);
        assert_eq!(cost.output, 2.2);
    }

    #[test]
    fn an_opaque_alias_borrows_nothing_from_a_reseller() {
        let index = build_pricing_index(&fixture());
        assert_eq!(lookup_cost(&index, "parrot", None), None);
    }

    #[test]
    fn a_dotted_version_matches_its_dashed_form() {
        let index = build_pricing_index(&fixture());
        let cost = lookup_cost(&index, "gemini-3-1-preview", None).expect("gemini price");
        assert_eq!(cost.input, 1.0);
    }

    #[test]
    fn a_deprecated_row_loses_to_a_live_one() {
        let index = build_pricing_index(&fixture());
        let cost = lookup_cost(&index, "gemini-3.1-preview", None).expect("gemini price");
        assert_eq!(cost.input, 1.0);
    }

    #[test]
    fn names_prefixes_and_bad_costs_behave_as_documented() {
        assert_eq!(strip_model_prefix("kilo/xiaomi/mimo"), "mimo");
        assert_eq!(strip_model_prefix("  gemini-3.1  "), "gemini-3.1");
        assert_eq!(strip_model_prefix("ft:gpt-5.5:acme"), "ft:gpt-5.5:acme");
        assert_eq!(normalize_key("Claude Opus 5!"), "claudeopus5");
        assert_eq!(dot_to_dash("5.3-flash"), "5-3-flash");
        assert_eq!(read_cost(Some(&json!({"input": -1, "output": 2}))), None);
        assert_eq!(read_cost(Some(&json!({"input": 1}))), None);
    }

    #[test]
    fn unknown_ids_stay_unpriced() {
        let index = build_pricing_index(&fixture());
        assert_eq!(lookup_cost(&index, "totally-unknown", None), None);
        let costs = resolve_catalog_costs(
            &index,
            &[
                CatalogModel::new("claude-opus-5"),
                CatalogModel::new("totally-unknown"),
            ],
        );
        assert_eq!(costs.len(), 1);
        assert!(costs.contains_key("claude-opus-5"));
    }

    #[test]
    fn a_family_from_an_id_shaped_display_name_still_resolves() {
        let index = build_pricing_index(&fixture());
        let cost =
            lookup_cost(&index, "opaque", Some("glm-5.3")).expect("glm price via name token");
        assert_eq!(cost.input, 0.6);
    }
}
