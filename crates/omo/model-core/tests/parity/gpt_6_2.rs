//! Translation of `gpt-6.2.test.ts` (omo #14149): GPT-6.2 Astra and Sol capabilities and lanes.

use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::ExtendedModelResolutionInput;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelResolutionProvenance;
use model_core::ModelResolutionResult;
use model_core::get_model_capabilities;
use model_core::is_gpt6_astra_model;
use model_core::resolve_model_with_fallback;
use pretty_assertions::assert_eq;

use crate::support::bundled_snapshot;
use crate::support::set;

fn resolve_category(category: &str, models: &[&str]) -> Option<ModelResolutionResult> {
    resolve_model_with_fallback(&ExtendedModelResolutionInput {
        fallback_chain: Some(CATEGORY_MODEL_REQUIREMENTS[category].fallback_chain.clone()),
        available_models: set(models),
        system_default_model: Some("system/default".to_string()),
        ..Default::default()
    })
}

fn model_and_variant(result: Option<ModelResolutionResult>) -> (String, Option<String>) {
    let result = result.expect("a model resolves");
    (result.model, result.variant)
}

fn pair(model: &str, variant: &str) -> (String, Option<String>) {
    (model.to_string(), Some(variant.to_string()))
}

#[test]
fn gpt_6_2_models_offer_low_to_max_with_the_documented_limits() {
    let snapshot = bundled_snapshot();
    for model_id in [
        "gpt-6.2-astra",
        "gpt-6.2-astra-fast",
        "gpt-6.2-sol",
        "gpt-6.2-sol-fast",
    ] {
        let capabilities = get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "openai",
            model_id,
            bundled_snapshot: Some(&snapshot),
            ..Default::default()
        });

        assert_eq!(
            capabilities.reasoning_efforts,
            Some(
                ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_string)
                    .to_vec()
            ),
            "{model_id}"
        );
        assert_eq!(capabilities.max_output_tokens, Some(128_000), "{model_id}");
        assert_eq!(capabilities.supports_temperature, Some(false), "{model_id}");
    }
}

#[test]
fn gpt_6_2_astra_is_the_astra_tier_and_gpt_6_2_sol_is_not() {
    assert!(is_gpt6_astra_model(Some(
        "chatgpt-subscription/gpt-6.2-astra"
    )));
    assert!(is_gpt6_astra_model(Some("openai/gpt-6.2-astra-fast")));
    assert!(is_gpt6_astra_model(Some(
        "chatgpt-subscription/gpt-6.2-astra-ultrafast"
    )));
    assert!(!is_gpt6_astra_model(Some(
        "chatgpt-subscription/gpt-6.2-sol"
    )));
    assert!(!is_gpt6_astra_model(Some(
        "chatgpt-subscription/gpt-6.2-sol-ultrafast"
    )));
}

#[test]
fn deep_gpt_6_2_lanes_resolve_only_their_exact_model() {
    for (category, model, variant) in [
        ("ultrabrain", "gpt-6.2-astra", "max"),
        ("deep-high", "gpt-6.2-astra", "high"),
        ("deep-low", "gpt-6.2-sol", "medium"),
    ] {
        let registry = [
            format!("chatgpt-subscription/{model}"),
            "chatgpt-subscription/gpt-6-astra".to_string(),
            "openai/gpt-6.1-sol".to_string(),
        ];
        let refs: Vec<&str> = registry.iter().map(String::as_str).collect();
        assert_eq!(
            model_and_variant(resolve_category(category, &refs)),
            pair(&format!("chatgpt-subscription/{model}"), variant),
            "{category}"
        );
    }
}

#[test]
fn deep_gpt_6_2_lanes_find_no_rung_on_older_gpt_and_copilot_models() {
    let registry = [
        "openai/gpt-6-astra",
        "openai/gpt-6.1-sol",
        "openai/gpt-5.6-sol",
        "github-copilot/gpt-6-astra",
    ];
    for category in ["ultrabrain", "deep-high", "deep-low"] {
        assert_eq!(
            resolve_category(category, &registry),
            Some(ModelResolutionResult {
                model: "system/default".to_string(),
                source: ModelResolutionProvenance::SystemDefault,
                variant: None,
            }),
            "{category}"
        );
    }
}
