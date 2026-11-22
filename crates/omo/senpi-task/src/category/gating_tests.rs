use pretty_assertions::assert_eq;
use serde_json::json;

use super::tests::{
    chain, contains, expect_resolved, expect_unavailable, models_of, resolve, spec_triple,
};
use crate::host::fake::FakeRegistry;

// ---- category/gating.test.ts ----

const PRE_GATING: &[(&str, &str)] = &[
    ("google", "gemini-3.1-pro"),
    ("anthropic", "claude-opus-5-5"),
    ("opencode-go", "glm-5.2"),
    ("kimi-coding", "k3"),
];

fn pre_gating(extra: &[(&str, &str)]) -> FakeRegistry {
    let mut pairs = PRE_GATING.to_vec();
    pairs.extend_from_slice(extra);
    models_of(&pairs)
}

#[test]
fn architect_on_cross_family_models_is_unavailable_and_never_falls_back() {
    let unavailable = expect_unavailable(resolve("architect", json!({}), &pre_gating(&[])));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("anthropic/claude-fable-5-2")
    );
    assert!(!contains(&unavailable.available_categories, "architect"));
}

#[test]
fn architect_resolves_on_fable_5_1_at_xhigh() {
    let models = pre_gating(&[("anthropic", "claude-fable-5-1")]);
    let resolved = expect_resolved(resolve("architect", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("anthropic", "claude-fable-5-1", Some("xhigh"))
    );
    assert!(contains(&resolved.available_categories, "architect"));
}

#[test]
fn architect_resolves_on_copilots_dotted_fable_id() {
    let models = models_of(&[("github-copilot", "claude-fable-5.1")]);
    let resolved = expect_resolved(resolve("architect", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("github-copilot", "claude-fable-5.1", Some("xhigh"))
    );
    assert!(contains(&resolved.available_categories, "architect"));
}

#[test]
fn deep_low_is_unavailable_on_gpt_6_sol_alone() {
    let result = resolve(
        "deep-low",
        json!({}),
        &models_of(&[("openai", "gpt-6-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "deep-low"));
}

#[test]
fn an_explicit_architect_model_bypasses_the_gate() {
    let resolved = expect_resolved(resolve(
        "architect",
        json!({ "categories": { "architect": { "model": "kimi-coding/k3" } } }),
        &models_of(&[("kimi-coding", "k3")]),
    ));
    assert_eq!(resolved.spec.model_id, "k3");
}

#[test]
fn a_description_only_architect_entry_bypasses_the_gate_and_stays_listed() {
    let result = resolve(
        "architect",
        json!({ "categories": { "architect": { "description": "House architecture consultant" } } }),
        &models_of(&[("anthropic", "claude-opus-5-5")]),
    );
    assert!(contains(result.available_categories(), "architect"));
}

#[test]
fn ultrabrain_on_cross_family_models_is_unavailable_and_never_falls_back() {
    let unavailable = expect_unavailable(resolve("ultrabrain", json!({}), &pre_gating(&[])));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("chatgpt-subscription/gpt-6.2-astra")
    );
    assert!(!contains(&unavailable.available_categories, "ultrabrain"));
}

#[test]
fn ultrabrain_resolves_on_gpt_6_astra_alone_at_max() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &models_of(&[("openai", "gpt-6-astra")]),
    ));
    assert_eq!(
        spec_triple(&resolved),
        ("openai", "gpt-6-astra", Some("max"))
    );
    assert!(contains(&resolved.available_categories, "ultrabrain"));
}

#[test]
fn ultrabrain_falls_back_to_the_sol_rung_at_max() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &models_of(&[("openai", "gpt-5.6-sol")]),
    ));
    assert_eq!(
        spec_triple(&resolved),
        ("openai", "gpt-5.6-sol", Some("max"))
    );
}

#[test]
fn ultrabrain_opens_on_copilots_gpt_5_6_sol() {
    let resolved = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &models_of(&[("github-copilot", "gpt-5.6-sol")]),
    ));
    assert_eq!(resolved.spec.provider, "github-copilot");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn neither_deep_lane_falls_back_to_cross_family_models() {
    let models = pre_gating(&[]);
    let low = resolve("deep-low", json!({}), &models);
    let high = resolve("deep-high", json!({}), &models);
    assert_eq!(low.kind(), "model_unavailable");
    assert_eq!(high.kind(), "model_unavailable");
    assert!(!contains(low.available_categories(), "deep-low"));
    assert!(!contains(low.available_categories(), "deep-high"));
}

#[test]
fn deep_high_resolves_on_gpt_6_astra_while_deep_low_stays_unavailable() {
    let models = models_of(&[("openai", "gpt-6-astra")]);
    let resolved = expect_resolved(resolve("deep-high", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("openai", "gpt-6-astra", Some("high"))
    );
    assert!(contains(&resolved.available_categories, "deep-high"));
    assert!(!contains(&resolved.available_categories, "deep-low"));
    assert_eq!(
        resolve("deep-low", json!({}), &models).kind(),
        "model_unavailable"
    );
}

#[test]
fn deep_low_resolves_on_gpt_5_6_sol_while_deep_high_stays_unavailable() {
    let models = models_of(&[("openai", "gpt-5.6-sol")]);
    let resolved = expect_resolved(resolve("deep-low", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("openai", "gpt-5.6-sol", Some("medium"))
    );
    assert!(contains(&resolved.available_categories, "deep-low"));
    assert!(!contains(&resolved.available_categories, "deep-high"));
    assert_eq!(
        resolve("deep-high", json!({}), &models).kind(),
        "model_unavailable"
    );
}

#[test]
fn an_explicit_deep_low_model_bypasses_the_gate() {
    let resolved = expect_resolved(resolve(
        "deep-low",
        json!({ "categories": { "deep-low": { "model": "anthropic/claude-opus-5-5" } } }),
        &models_of(&[("anthropic", "claude-opus-5-5")]),
    ));
    assert_eq!(resolved.spec.model_id, "claude-opus-5-5");
}

#[test]
fn visual_engineering_on_kimi_alone_is_a_real_chain_fallback() {
    let resolved = expect_resolved(resolve(
        "visual-engineering",
        json!({}),
        &models_of(&[("kimi-coding", "k3.1-pro")]),
    ));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
}

#[test]
fn artistry_on_fable_alone_is_a_primary_hit() {
    let resolved = expect_resolved(resolve(
        "artistry",
        json!({}),
        &models_of(&[("anthropic", "claude-fable-5-2")]),
    ));
    assert!(!resolved.selection.matched_fallback);
}

#[test]
fn a_model_whose_last_segment_collides_with_a_gate_model_opens_no_gate() {
    for (category, colliding) in [
        ("architect", ("custom", "unrelated/claude-fable-5-1")),
        ("ultrabrain", ("custom", "unrelated/gpt-5.6-sol")),
        ("ultrabrain", ("custom", "unrelated/gpt-6-astra")),
        ("ultrabrain", ("vercel", "openai/gpt-5.6-sol")),
    ] {
        let result = resolve(category, json!({}), &models_of(&[colliding]));
        assert_eq!(
            result.kind(),
            "model_unavailable",
            "{category} {colliding:?}"
        );
        assert!(!contains(result.available_categories(), category));
    }
}

#[test]
fn an_ungated_category_keeps_the_pre_gating_fallback_behaviour() {
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &models_of(&[("chatgpt-subscription", "gpt-6-luna-fast")]),
    ));
    assert_eq!(resolved.spec.model_id, "gpt-6-luna-fast");
    assert!(contains(&resolved.available_categories, "quick"));
}

#[test]
fn unmet_gated_categories_leave_the_others_listed() {
    let result = resolve(
        "quick",
        json!({}),
        &models_of(&[("chatgpt-subscription", "gpt-6-luna-fast")]),
    );
    let names = result.available_categories();
    for gated in ["architect", "ultrabrain", "deep-low", "deep-high"] {
        assert!(!contains(names, gated), "{gated}");
    }
    assert!(contains(names, "quick"));
}

// ---- category/dead-chain.test.ts ----

fn opus_only() -> FakeRegistry {
    models_of(&[
        ("omo-mock", "mock-parent"),
        ("anthropic", "claude-opus-5-5"),
    ])
}

#[test]
fn a_dead_builtin_chain_fails_with_attempted_chain_and_missing_providers() {
    let unavailable = expect_unavailable(resolve("quick", json!({}), &opus_only()));
    assert_eq!(unavailable.attempted_chain, Some(chain("quick")));
    assert_eq!(
        unavailable.missing_providers,
        Some(
            [
                "chatgpt-subscription",
                "openai",
                "anthropic-subscription",
                "anthropic-api",
                "github-copilot",
                "deepseek",
                "qwen-token-plan",
                "alibaba-token-plan",
                "bailian-coding-plan",
                "opencode-go",
                "xai",
                "zai",
                "zai-coding-cn",
                "xiaomi",
            ]
            .map(str::to_string)
            .to_vec()
        )
    );
}

#[test]
fn a_dead_chain_builtin_is_excluded_from_the_gated_list() {
    let result = resolve("quick", json!({}), &opus_only());
    assert!(!contains(result.available_categories(), "quick"));
}

#[test]
fn builtins_with_a_live_rung_stay_listed() {
    let result = resolve("quick", json!({}), &opus_only());
    assert!(contains(
        result.available_categories(),
        "visual-engineering"
    ));
    assert!(contains(result.available_categories(), "unspecified-high"));
}

#[test]
fn a_gate_failure_on_a_dead_chain_carries_the_chain_details() {
    let unavailable = expect_unavailable(resolve("architect", json!({}), &opus_only()));
    assert_eq!(unavailable.attempted_chain, Some(chain("architect")));
    let missing = unavailable.missing_providers.expect("missing providers");
    assert!(contains(&missing, "anthropic-api"));
    assert!(!contains(&missing, "anthropic"));
}

#[test]
fn the_kimi_transform_keeps_a_category_alive() {
    let result = resolve(
        "unspecified-high",
        json!({}),
        &models_of(&[("kimi-coding", "k3.1-pro")]),
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "unspecified-high"));
    assert!(!contains(result.available_categories(), "quick"));
}

#[test]
fn copilots_dotted_haiku_id_keeps_quick_alive() {
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &models_of(&[("github-copilot", "claude-haiku-5.5")]),
    ));
    assert_eq!(
        spec_triple(&resolved),
        ("github-copilot", "claude-haiku-5.5", Some("medium"))
    );
    assert!(contains(&resolved.available_categories, "quick"));
}

#[test]
fn writing_without_its_claude_models_never_borrows_another_family() {
    for models in [
        models_of(&[
            ("chatgpt-subscription", "gpt-6-sol"),
            ("openai", "gpt-5.6-sol"),
        ]),
        models_of(&[
            ("anthropic", "claude-fable-5-1"),
            ("github-copilot", "claude-fable-5.1"),
        ]),
    ] {
        let result = resolve("writing", json!({}), &models);
        assert_eq!(result.kind(), "model_unavailable");
        assert!(!contains(result.available_categories(), "writing"));
    }
}

#[test]
fn writing_resolves_on_copilots_dotted_opus_5_5() {
    let result = resolve(
        "writing",
        json!({}),
        &models_of(&[("github-copilot", "claude-opus-5.5")]),
    );
    assert_eq!(result.kind(), "resolved");
}

#[test]
fn a_gateway_copy_that_is_no_rung_provider_keeps_the_chain_dead() {
    let result = resolve(
        "deep-low",
        json!({}),
        &models_of(&[("vercel", "openai/gpt-5.6-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "deep-low"));
}

#[test]
fn a_user_model_is_never_gated_by_a_dead_builtin_chain() {
    let models = models_of(&[("omo-mock", "mock-parent")]);
    let result = resolve(
        "quick",
        json!({ "categories": { "quick": { "model": "omo-mock/mock-parent" } } }),
        &models,
    );
    assert_eq!(result.kind(), "resolved");
    assert!(contains(result.available_categories(), "quick"));
}

#[test]
fn a_missing_user_model_is_a_plain_miss_without_chain_details() {
    let models = models_of(&[("omo-mock", "mock-parent")]);
    let unavailable = expect_unavailable(resolve(
        "quick",
        json!({ "categories": { "quick": { "model": "omo-mock/absent" } } }),
        &models,
    ));
    assert_eq!(unavailable.attempted_chain, None);
    assert!(contains(&unavailable.available_categories, "quick"));
}

#[test]
fn a_disabled_category_still_returns_the_gated_list() {
    let result = resolve(
        "quick",
        json!({ "categories": { "quick": { "disable": true } } }),
        &opus_only(),
    );
    assert_eq!(result.kind(), "disabled");
    let names = result.available_categories();
    assert!(contains(names, "quick"));
    assert!(contains(names, "visual-engineering"));
    assert!(!contains(names, "deep-low"));
    assert!(!contains(names, "deep-high"));
}

#[test]
fn an_unknown_category_still_returns_the_gated_list() {
    let result = resolve("nope", json!({}), &opus_only());
    assert_eq!(result.kind(), "not_found");
    let names = result.available_categories();
    assert!(contains(names, "visual-engineering"));
    assert!(!contains(names, "quick"));
    assert!(!contains(names, "deep-low"));
    assert!(!contains(names, "deep-high"));
}
