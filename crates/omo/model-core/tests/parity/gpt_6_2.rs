//! Translation of `gpt-6.2.test.ts` (omo #14149): GPT-6.2 Astra and Sol capabilities and lanes.

use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::ExtendedModelResolutionInput;
use model_core::GetModelCapabilitiesInput;
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
    assert!(!is_gpt6_astra_model(Some(
        "chatgpt-subscription/gpt-6.2-sol"
    )));
}

#[test]
fn deep_low_resolves_gpt_6_2_sol_when_the_registry_serves_it_beside_6_1_sol() {
    assert_eq!(
        model_and_variant(resolve_category(
            "deep-low",
            &[
                "chatgpt-subscription/gpt-6.2-sol",
                "chatgpt-subscription/gpt-6.1-sol"
            ]
        )),
        pair("chatgpt-subscription/gpt-6.2-sol", "medium")
    );
}

#[test]
fn deep_low_keeps_gpt_6_1_sol_on_a_registry_without_6_2() {
    assert_eq!(
        model_and_variant(resolve_category(
            "deep-low",
            &[
                "chatgpt-subscription/gpt-6.1-sol",
                "chatgpt-subscription/gpt-5.6-sol"
            ]
        )),
        pair("chatgpt-subscription/gpt-6.1-sol", "medium")
    );
}

#[test]
fn deep_high_resolves_gpt_6_2_astra_and_falls_back_to_gpt_6_astra_without_6_2() {
    assert_eq!(
        model_and_variant(resolve_category(
            "deep-high",
            &["openai/gpt-6.2-astra", "openai/gpt-6-astra"]
        )),
        pair("openai/gpt-6.2-astra", "high")
    );
    assert_eq!(
        model_and_variant(resolve_category("deep-high", &["openai/gpt-6-astra"])),
        pair("openai/gpt-6-astra", "high")
    );
}

#[test]
fn ultrabrain_leads_with_gpt_6_2_astra_and_keeps_copilot_on_gpt_6_astra_max() {
    assert_eq!(
        model_and_variant(resolve_category(
            "ultrabrain",
            &["openai/gpt-6.2-astra", "openai/gpt-6-astra"]
        )),
        pair("openai/gpt-6.2-astra", "max")
    );
    assert_eq!(
        model_and_variant(resolve_category(
            "ultrabrain",
            &["github-copilot/gpt-6-astra"]
        )),
        pair("github-copilot/gpt-6-astra", "max")
    );
}
