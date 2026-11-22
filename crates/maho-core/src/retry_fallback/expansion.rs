use indexmap::IndexMap;
use maho_ai::types::{Model, ModelThinkingLevel};

pub const MAX_PROVIDERS_PER_FAMILY: usize = 2;
const DENYLIST: &[&str] = &[
    "cursor",
    "openrouter",
    "openrouter-images",
    "opengateway",
    "vercel-ai-gateway",
];
const PRECEDENCE: &[&str] = &["anthropic-subscription", "anthropic", "kimi-coding"];

pub const FIRST_PARTY_PROVIDERS: &[(&[&str], &[&str])] = &[
    (&["claude-"], &["anthropic-subscription", "anthropic"]),
    (&["gpt-"], &["chatgpt-subscription", "openai"]),
    (&["gemini-"], &["google", "google-vertex"]),
    (&["kimi-"], &["kimi-coding", "moonshotai", "moonshotai-cn"]),
    (&["glm-"], &["zai", "zai-coding-cn"]),
    (&["grok-"], &["xai"]),
    (&["deepseek-"], &["deepseek"]),
    (
        &[
            "mistral",
            "codestral",
            "devstral",
            "magistral",
            "ministral",
            "pixtral",
        ],
        &["mistral"],
    ),
    (&["minimax-"], &["minimax", "minimax-cn"]),
    (
        &["mimo-"],
        &[
            "xiaomi",
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-sgp",
            "xiaomi-token-plan-ams",
        ],
    ),
    (
        &["qwen"],
        &[
            "qwen-token-plan",
            "qwen-token-plan-individual",
            "qwen-token-plan-cn",
            "alibaba-token-plan",
        ],
    ),
];

fn is_o_digit(family: &str) -> bool {
    let bytes = family.as_bytes();
    bytes.len() >= 2 && bytes[0] == b'o' && bytes[1].is_ascii_digit()
}

fn is_k_digit(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() >= 2 && bytes[0] == b'k' && bytes[1].is_ascii_digit()
}

pub fn first_party_providers_for(family: &str) -> Option<&'static [&'static str]> {
    let lower = family.to_lowercase();
    if is_o_digit(&lower) {
        return Some(&["chatgpt-subscription", "openai"]);
    }
    if is_k_digit(&lower) {
        return Some(&["kimi-coding", "moonshotai", "moonshotai-cn"]);
    }
    FIRST_PARTY_PROVIDERS
        .iter()
        .find(|(prefixes, _)| prefixes.iter().any(|prefix| lower.starts_with(prefix)))
        .map(|(_, providers)| *providers)
}

pub fn in_bare_scope(provider: &str, family: &str, session_provider: Option<&str>) -> bool {
    let normalized = provider.to_lowercase();
    if DENYLIST.contains(&normalized.as_str()) {
        return false;
    }
    let vendor = first_party_providers_for(family);
    let Some(session) = session_provider.map(str::to_lowercase) else {
        return vendor.is_none_or(|providers| providers.contains(&normalized.as_str()));
    };
    if normalized == session {
        return true;
    }
    let Some(providers) = vendor else {
        return false;
    };
    if providers.contains(&session.as_str()) {
        return false;
    }
    providers.contains(&normalized.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BareSelectorParts {
    pub family: String,
    pub thinking_level: Option<ModelThinkingLevel>,
}

pub struct FallbackAuthTiers<'a> {
    pub is_using_oauth: &'a dyn Fn(&Model) -> bool,
    pub has_configured_auth: Option<&'a dyn Fn(&Model) -> bool>,
    pub is_fallback_eligible: Option<&'a dyn Fn(&Model) -> bool>,
}

pub fn parse_bare_selector(raw: &str) -> Option<BareSelectorParts> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains(['/', '*']) {
        return None;
    }
    if let Some((prefix, suffix)) = raw.rsplit_once(':')
        && let Some(level) = ModelThinkingLevel::parse(&suffix.to_lowercase())
    {
        let family = prefix.trim().to_lowercase();
        return (!family.is_empty()).then_some(BareSelectorParts {
            family,
            thinking_level: Some(level),
        });
    }
    Some(BareSelectorParts {
        family: raw.to_lowercase(),
        thinking_level: None,
    })
}

fn without_namespace(id: &str) -> &str {
    id.rsplit(['.', '/']).next().unwrap_or(id)
}

fn family_ids(model: &Model) -> Vec<String> {
    let id = model.id.to_lowercase();
    let stripped = without_namespace(&id).to_owned();
    let mut ids = vec![id.clone(), stripped];
    if is_k_digit(&id) {
        ids.push(format!("kimi-{id}"));
    }
    ids
}

fn exactness(model: &Model, family: &str) -> u8 {
    let id = model.id.to_lowercase();
    if id == family || without_namespace(&id) == family {
        2
    } else if family_ids(model)
        .iter()
        .any(|candidate| candidate == family)
    {
        1
    } else {
        0
    }
}

pub fn matches_family(model: &Model, family: &str) -> bool {
    let prefix = format!("{family}-");
    family_ids(model)
        .iter()
        .any(|id| id == family || id.starts_with(&prefix))
}

pub fn rank_family_models<'a>(
    models: &'a [Model],
    family: &str,
    tiers: &FallbackAuthTiers<'_>,
    limit: Option<usize>,
) -> Vec<&'a Model> {
    rank_family_models_for_session(models, family, tiers, limit, None)
}

pub fn rank_family_models_for_session<'a>(
    models: &'a [Model],
    family: &str,
    tiers: &FallbackAuthTiers<'_>,
    limit: Option<usize>,
    session_provider: Option<&str>,
) -> Vec<&'a Model> {
    let effective_session_provider = match (first_party_providers_for(family), session_provider) {
        (None, Some(sp))
            if !models
                .iter()
                .any(|m| m.provider.eq_ignore_ascii_case(sp) && matches_family(m, family)) =>
        {
            None
        }
        (_, sp) => sp,
    };
    let mut grouped: IndexMap<&str, Vec<&'a Model>> = IndexMap::new();
    for model in models {
        if !in_bare_scope(&model.provider, family, effective_session_provider)
            || !matches_family(model, family)
            || tiers.is_fallback_eligible.is_some_and(|f| !f(model))
        {
            continue;
        }
        grouped.entry(&model.provider).or_default().push(model);
    }
    if let Some(session_provider) = effective_session_provider
        && let Some(own_key) = grouped
            .keys()
            .find(|provider| provider.eq_ignore_ascii_case(session_provider))
            .copied()
    {
        grouped.retain(|provider, _| *provider == own_key);
    }
    let mut picked: Vec<_> = grouped
        .values_mut()
        .filter_map(|variants| {
            variants.sort_by(|a, b| {
                exactness(b, family)
                    .cmp(&exactness(a, family))
                    .then_with(|| a.id.len().cmp(&b.id.len()))
                    .then_with(|| a.id.cmp(&b.id))
            });
            variants.first().copied()
        })
        .collect();
    let tier = |m: &Model| {
        if (tiers.is_using_oauth)(m) {
            0
        } else if tiers.has_configured_auth.is_some_and(|f| f(m)) {
            1
        } else {
            2
        }
    };
    let precedence = |m: &Model| {
        PRECEDENCE
            .iter()
            .position(|p| *p == m.provider.to_lowercase())
            .unwrap_or(PRECEDENCE.len())
    };
    picked.sort_by(|a, b| {
        tier(a)
            .cmp(&tier(b))
            .then_with(|| precedence(a).cmp(&precedence(b)))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    if let Some(limit) = limit {
        picked.truncate(limit);
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model(provider: &str, id: &str) -> Model {
        let mut m = maho_ai::providers::all::get_builtin_models("openai").remove(0);
        m.provider = provider.into();
        m.id = id.into();
        m.name = id.into();
        m
    }

    #[test]
    fn bare_selectors_stay_on_first_party_and_session_provider_while_matching_kimi_aliases() {
        let models = vec![
            sample_model("anthropic-subscription", "claude-opus-5-5"),
            sample_model("anthropic", "claude-opus-5-5"),
            sample_model("openrouter", "anthropic/claude-opus-5-5"),
            sample_model("kimi-coding", "k3.1-pro"),
            sample_model("moonshotai", "kimi-k3.1-pro"),
        ];
        let tiers = FallbackAuthTiers {
            is_using_oauth: &|m| m.provider == "anthropic-subscription",
            has_configured_auth: Some(&|_| true),
            is_fallback_eligible: None,
        };
        let key_models = rank_family_models(&models, "claude-opus-5-5", &tiers, None);
        let key_providers: Vec<&str> = key_models.iter().map(|m| m.provider.as_str()).collect();
        assert_eq!(key_providers, ["anthropic-subscription", "anthropic"]);

        let same_lane = rank_family_models_for_session(
            &models,
            "claude-opus-5-5",
            &tiers,
            Some(MAX_PROVIDERS_PER_FAMILY),
            Some("anthropic-subscription"),
        );
        assert_eq!(same_lane.len(), 1);
        assert_eq!(same_lane[0].provider, "anthropic-subscription");

        let cross_vendor = rank_family_models_for_session(
            &models,
            "kimi-k3.1-pro",
            &tiers,
            Some(MAX_PROVIDERS_PER_FAMILY),
            Some("anthropic-subscription"),
        );
        let cross_providers: Vec<&str> = cross_vendor.iter().map(|m| m.provider.as_str()).collect();
        assert_eq!(cross_providers, ["kimi-coding", "moonshotai"]);
    }
}
