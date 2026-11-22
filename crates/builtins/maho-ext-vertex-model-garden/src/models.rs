use maho_ai::{
    model::ModelCompat,
    types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap},
};
use maho_ext_api::ProviderModelConfig;

// This starter list covers open models through the global OpenAI-compatible endpoint.
// Marketplace partner models are excluded: they are card-billed and promotional credits do not apply.
// Prices are the model card's published MaaS list prices, USD per million tokens
// (https://cloud.google.com/gemini-enterprise-agent-platform/generative-ai/pricing,
// "Deepseek's models" and "GLM's models"); DeepSeek V4.1 Flash is listed at no charge through 2026-10-16.
// These endpoints accept no thinking parameters, so no level may be offered as steerable.
fn thinking_levels() -> ThinkingLevelMap {
    ModelThinkingLevel::ALL
        .into_iter()
        .map(|level| {
            let mapped = match level {
                ModelThinkingLevel::Off => Some("off".into()),
                ModelThinkingLevel::Medium => Some("on".into()),
                _ => None,
            };
            (level, mapped)
        })
        .collect()
}

fn thinking_toggle(flag: &str) -> ModelCompat {
    match serde_json::json!({
        "thinkingFormat": "chat-template",
        "supportsReasoningEffort": false,
        "supportsDeveloperRole": false,
        "chatTemplateKwargs": { flag: { "$var": "thinking.enabled" } }
    }) {
        serde_json::Value::Object(map) => ModelCompat(map),
        _ => ModelCompat::default(),
    }
}

fn garden(
    id: &str,
    name: &str,
    cost: ModelCost,
    context_window: u64,
    compat: ModelCompat,
    default_thinking_level: ModelThinkingLevel,
) -> ProviderModelConfig {
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        default_thinking_level: Some(default_thinking_level),
        id: id.into(),
        name: name.into(),
        upstream_model_id: None,
        api: None,
        base_url: None,
        reasoning: true,
        recover_text_tool_calls: None,
        thinking_level_map: Some(thinking_levels()),
        input: vec![InputModality::Text],
        cost,
        context_window,
        max_tokens: 32_000,
        headers: None,
        extra_body: None,
        compat: Some(compat),
    }
}

fn cost(input: f64, output: f64, cache_read: f64) -> ModelCost {
    ModelCost {
        input,
        output,
        cache_read,
        cache_write: 0.0,
        ..Default::default()
    }
}

pub fn vertex_model_garden_models() -> Vec<ProviderModelConfig> {
    vec![
        garden(
            "deepseek-ai/deepseek-v4.1-flash-maas",
            "DeepSeek V4.1 Flash (Vertex)",
            cost(0.3, 1.2, 0.03),
            1_000_000,
            thinking_toggle("thinking"),
            ModelThinkingLevel::Off,
        ),
        garden(
            "zai-org/glm-5.2-maas",
            "GLM 5.2 (Vertex)",
            cost(1.4, 4.4, 0.14),
            200_000,
            thinking_toggle("enable_thinking"),
            ModelThinkingLevel::Medium,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_toggles_expose_off_and_medium_with_per_model_chat_template_flags() {
        let models = vertex_model_garden_models();
        assert_eq!(
            models[0].default_thinking_level,
            Some(ModelThinkingLevel::Off)
        );
        assert_eq!(
            models[0].compat.as_ref().expect("compat").0["chatTemplateKwargs"]["thinking"]["$var"],
            "thinking.enabled"
        );
        assert_eq!(
            models[1].default_thinking_level,
            Some(ModelThinkingLevel::Medium)
        );
        assert_eq!(
            models[1].compat.as_ref().expect("compat").0["chatTemplateKwargs"]["enable_thinking"]["$var"],
            "thinking.enabled"
        );
        for model in models {
            let map = model.thinking_level_map.expect("map");
            assert_eq!(map.len(), 7);
            assert_eq!(map[&ModelThinkingLevel::Off].as_deref(), Some("off"));
            assert_eq!(map[&ModelThinkingLevel::Medium].as_deref(), Some("on"));
            assert!(map[&ModelThinkingLevel::Low].is_none());
            assert_eq!(
                model.compat.expect("compat").0["supportsDeveloperRole"],
                false
            );
        }
    }
}
