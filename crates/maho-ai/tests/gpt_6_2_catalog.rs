//! Catalog rows for GPT-6.2 Astra and Sol (senpi#6948, senpi v2026.11.19-5 data refresh).

use maho_ai::models::{
    clamp_thinking_level, get_supported_thinking_levels, infer_openai_thinking_level_map,
    supports_configuration_update, supports_max, supports_xhigh,
};
use maho_ai::models_generated::get_builtin_model;
use maho_ai::types::{Model, ModelThinkingLevel as L, ServiceTierPreference};

fn builtin(provider: &str, id: &str) -> Model {
    get_builtin_model(provider, id)
        .unwrap_or_else(|error| panic!("{provider}/{id}: {error}"))
        .clone()
}

#[test]
fn the_four_gpt_6_2_tiers_ship_on_the_openai_and_chatgpt_providers() {
    for (provider, api) in [
        ("openai", "openai-responses"),
        ("chatgpt-subscription", "openai-codex-responses"),
    ] {
        for id in [
            "gpt-6.2-astra",
            "gpt-6.2-astra-fast",
            "gpt-6.2-sol",
            "gpt-6.2-sol-fast",
        ] {
            let model = builtin(provider, id);
            assert_eq!(model.api, api, "{provider}/{id}");
            assert_eq!(model.provider, provider, "{provider}/{id}");
            assert!(model.reasoning, "{provider}/{id}");
            assert_eq!(model.max_tokens, 128_000, "{provider}/{id}");
        }
    }
    for id in ["gpt-6.2-astra", "gpt-6.2-sol"] {
        assert_eq!(
            builtin("azure-openai-responses", id).api,
            "azure-openai-responses"
        );
    }
}

#[test]
fn context_windows_follow_each_tier_and_route() {
    for (provider, astra, sol) in [
        ("openai", 600_000, 400_000),
        ("azure-openai-responses", 600_000, 400_000),
        ("chatgpt-subscription", 600_000, 500_000),
    ] {
        assert_eq!(
            builtin(provider, "gpt-6.2-astra").context_window,
            astra,
            "{provider} astra"
        );
        assert_eq!(
            builtin(provider, "gpt-6.2-sol").context_window,
            sol,
            "{provider} sol"
        );
    }
}

#[test]
fn fast_and_ultrafast_rows_are_priced_service_tier_views_of_their_base() {
    for provider in ["openai", "chatgpt-subscription"] {
        for base_id in ["gpt-6.2-astra", "gpt-6.2-sol"] {
            let base = builtin(provider, base_id);
            let fast = builtin(provider, &format!("{base_id}-fast"));
            assert_eq!(fast.upstream_model_id.as_deref(), Some(base_id));
            assert_eq!(fast.service_tier, Some(ServiceTierPreference::Priority));
            assert_eq!(fast.context_window, base.context_window);
            assert_eq!(fast.cost, base.cost);
            assert!(!fast.name.is_empty() && fast.name.ends_with("Fast"));
        }
    }
    let ultrafast = builtin("chatgpt-subscription", "gpt-6.2-sol-ultrafast");
    assert_eq!(ultrafast.upstream_model_id.as_deref(), Some("gpt-6.2-sol"));
    assert_eq!(
        ultrafast.service_tier,
        Some(ServiceTierPreference::Ultrafast)
    );
    assert_eq!(ultrafast.default_thinking_level, Some(L::Xhigh));
    assert!(get_builtin_model("openai", "gpt-6.2-sol-ultrafast").is_err());
}

#[test]
fn the_effort_ladder_runs_low_to_max_and_off_clamps_to_low() {
    for (provider, id) in [
        ("openai", "gpt-6.2-astra"),
        ("openai", "gpt-6.2-sol"),
        ("chatgpt-subscription", "gpt-6.2-sol-ultrafast"),
        ("azure-openai-responses", "gpt-6.2-astra"),
    ] {
        let model = builtin(provider, id);
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![L::Low, L::Medium, L::High, L::Xhigh, L::Max],
            "{provider}/{id}"
        );
        assert_eq!(clamp_thinking_level(&model, L::Minimal), L::Low);
        assert_eq!(clamp_thinking_level(&model, L::Off), L::Low);
        assert!(supports_xhigh(&model) && supports_max(&model), "{id}");
    }
}

#[test]
fn a_map_less_gpt_6_2_row_infers_the_astra_ladder() {
    for id in ["gpt-6.2-astra", "gpt-6.2-sol", "gpt-6.2-sol-fast"] {
        let mut model = builtin("openai", "gpt-6.2-astra");
        model.id = id.to_owned();
        model.thinking_level_map = None;
        let inferred = infer_openai_thinking_level_map(&model).expect("inferred ladder");
        assert_eq!(inferred.get(&L::Off), Some(&None), "{id}: no off effort");
        assert_eq!(inferred.get(&L::Max), Some(&Some("max".to_owned())), "{id}");
        assert!(supports_xhigh(&model) && supports_max(&model), "{id}");
    }
}

#[test]
fn the_gpt_6_2_rows_opt_into_configuration_updates() {
    for (provider, id) in [
        ("openai", "gpt-6.2-astra"),
        ("openai", "gpt-6.2-sol-fast"),
        ("chatgpt-subscription", "gpt-6.2-sol-ultrafast"),
    ] {
        assert!(
            supports_configuration_update(&builtin(provider, id)),
            "{id}"
        );
    }
}

#[test]
fn fable_5_2_rows_ship_across_upstream_providers() {
    for (provider, id) in [
        ("anthropic", "claude-fable-5-2"),
        ("amazon-bedrock", "anthropic.claude-fable-5-2"),
        ("amazon-bedrock", "global.anthropic.claude-fable-5-2"),
        ("amazon-bedrock", "us.anthropic.claude-fable-5-2"),
        ("github-copilot", "claude-fable-5.2"),
        ("opencode", "claude-fable-5-2"),
        ("openrouter", "anthropic/claude-fable-5.2"),
        ("vercel-ai-gateway", "anthropic/claude-fable-5.2"),
    ] {
        let model = builtin(provider, id);
        assert_eq!(model.id, id, "{provider}/{id}");
    }
}
