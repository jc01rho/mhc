use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::agents::{
    AgentDefinition, AgentModelEntry, AgentResolutionResult, BUILTIN_AGENT_DEFAULTS,
    ResolveAgentOptions, resolve_agent,
};
use crate::delegate_adapter::DelegateFallbackEntry;
use crate::host::SenpiModelRegistry;
use crate::host::fake::{FakeRegistry, failing_registry, fixed_registry, model, registry};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

/// Upstream's tests predate the Fable execution gate, so every port runs with the gate off;
/// `gate_tests.rs` covers the gate itself.
pub(super) fn ungated(config: Value) -> Value {
    let mut config = config;
    if let Value::Object(object) = &mut config {
        object
            .entry("model_policy")
            .or_insert_with(|| json!({ "block_fable": false }));
    }
    config
}

pub(super) fn resolve(
    category: &str,
    config: Value,
    models: &dyn SenpiModelRegistry,
) -> CategoryResolutionResult {
    resolve_category(
        category,
        &ungated(config),
        models,
        &ResolveCategoryOptions::default(),
    )
    .expect("resolve")
}

pub(super) fn available(config: Value, models: &dyn SenpiModelRegistry) -> Vec<String> {
    resolve_available_category_names(&ungated(config), models)
}

pub(super) struct Resolved {
    pub(super) category: String,
    pub(super) spec: ResolvedChildSpec,
    pub(super) description: Option<String>,
    pub(super) selection: CategoryModelSelection,
    pub(super) available_categories: Vec<String>,
}

pub(super) fn expect_resolved(result: CategoryResolutionResult) -> Resolved {
    match result {
        CategoryResolutionResult::Resolved {
            category,
            spec,
            description,
            model_selection,
            available_categories,
            ..
        } => Resolved {
            category,
            spec: *spec,
            description,
            selection: *model_selection,
            available_categories,
        },
        other => panic!("Expected resolved category, got {}", other.kind()),
    }
}

pub(super) fn expect_unavailable(result: CategoryResolutionResult) -> ModelUnavailable {
    match result {
        CategoryResolutionResult::ModelUnavailable(unavailable) => *unavailable,
        other => panic!("Expected model_unavailable, got {}", other.kind()),
    }
}

pub(super) fn entry(
    providers: &[&str],
    model: &str,
    variant: Option<&str>,
) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

pub(super) fn chain(name: &str) -> Vec<DelegateFallbackEntry> {
    category_fallback_chain(name).expect("chain").to_vec()
}

pub(super) fn contains(names: &[String], name: &str) -> bool {
    names.iter().any(|candidate| candidate == name)
}

pub(super) fn models_of(pairs: &[(&str, &str)]) -> FakeRegistry {
    registry(
        pairs
            .iter()
            .map(|(provider, id)| model(provider, id))
            .collect(),
    )
}

pub(super) fn spec_triple(resolved: &Resolved) -> (&str, &str, Option<&str>) {
    (
        resolved.spec.provider.as_str(),
        resolved.spec.model_id.as_str(),
        resolved.spec.variant.as_deref(),
    )
}

pub(super) const CLAUDE: &[&str] = &[
    "anthropic-subscription",
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
];
pub(super) const GPT_EVERY_LANE: &[&str] = &[
    "chatgpt-subscription",
    "openai",
    "github-copilot",
    "opencode",
];

// ---- category/resolve-category.test.ts ----

struct Gpt56Case {
    category: &'static str,
    model_id: &'static str,
    mixed_winner: (&'static str, &'static str, &'static str),
    copilot_variant: &'static str,
    copilot_fallback_entry: DelegateFallbackEntry,
}

fn gpt56_category_cases() -> Vec<Gpt56Case> {
    vec![
        Gpt56Case {
            category: "ultrabrain",
            model_id: "gpt-5.6-sol",
            mixed_winner: ("github-copilot", "gpt-5.6-sol", "max"),
            copilot_variant: "max",
            copilot_fallback_entry: entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
        },
        Gpt56Case {
            category: "deep",
            model_id: "gpt-5.6-sol",
            mixed_winner: ("github-copilot", "gpt-5.6-sol", "medium"),
            copilot_variant: "medium",
            copilot_fallback_entry: entry(GPT_EVERY_LANE, "gpt-5.6-sol", Some("medium")),
        },
        Gpt56Case {
            category: "unspecified-low",
            model_id: "gpt-5.6-sol",
            mixed_winner: ("github-copilot", "gpt-5.6-sol", "medium"),
            copilot_variant: "medium",
            copilot_fallback_entry: entry(GPT_EVERY_LANE, "gpt-5.6-sol", Some("medium")),
        },
    ]
}

#[test]
fn resolve_user_overlay_wins_and_prompt_is_appended() {
    let models = registry(vec![model("anthropic", "claude-opus-4-7")]);
    let config = json!({ "categories": { "ultrabrain": {
        "model": "anthropic/claude-opus-4-7", "variant": "max", "prompt_append": "fixture-overlay"
    } } });
    let resolved = expect_resolved(resolve("ultrabrain", config, &models));
    assert_eq!(resolved.spec.provider, "anthropic");
    assert_eq!(resolved.spec.model_id, "claude-opus-4-7");
    assert_eq!(resolved.spec.variant.as_deref(), Some("max"));
    let prompt_append = resolved.spec.prompt_append.expect("prompt append");
    assert_ne!(prompt_append, "fixture-overlay");
    assert!(prompt_append.ends_with("\n\nfixture-overlay"));
}

#[test]
fn resolve_disabled_overlay_explains_reason() {
    let models = registry(vec![model("openai", "gpt-5.5")]);
    let result = resolve(
        "ultrabrain",
        json!({ "categories": { "ultrabrain": { "disable": true } } }),
        &models,
    );
    let CategoryResolutionResult::Disabled {
        reason,
        available_categories,
        ..
    } = result
    else {
        panic!("Expected disabled result, got {}", result.kind());
    };
    assert!(reason.contains("disabled"));
    assert!(contains(&available_categories, "ultrabrain"));
}

#[test]
fn resolve_omo_fallback_reaches_registry_model() {
    let models = registry(vec![model("google", "gemini-3.1-pro")]);
    let config = json!({ "categories": { "ultrabrain": { "fallback_models": ["google/gemini-3.1-pro high"] } } });
    let resolved = expect_resolved(resolve("ultrabrain", config, &models));
    assert_eq!(resolved.spec.provider, "google");
    assert_eq!(resolved.spec.model_id, "gemini-3.1-pro");
    assert_eq!(resolved.spec.variant.as_deref(), Some("high"));
    assert!(resolved.selection.matched_fallback);
}

#[test]
fn resolve_runtime_fallback_chain_is_retained() {
    let models = registry(vec![
        model("vendor-b", "fallback-model"),
        model("vendor-c", "final-model"),
    ]);
    let config = json!({ "categories": { "quick": {
        "model": "vendor-a/primary-model",
        "fallback_models": ["vendor-b/fallback-model", "vendor-c/final-model"]
    } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-b");
    assert_eq!(resolved.spec.model_id, "fallback-model");
    // Upstream's `toMatchObject` checks only these four fields; the records also inherit quick's
    // builtin variant.
    let identity = |record: &ResolvedModelRecord| {
        (
            record.source,
            record.provider.clone(),
            record.model_id.clone(),
            record.display.clone(),
        )
    };
    let expected = |provider: &str, model_id: &str| {
        identity(&ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            provider,
            model_id,
        ))
    };
    assert_eq!(
        resolved.spec.requested_model.as_ref().map(identity),
        Some(expected("vendor-a", "primary-model"))
    );
    assert_eq!(
        resolved
            .spec
            .fallback_models
            .as_ref()
            .map(|records| records.iter().map(identity).collect::<Vec<_>>()),
        Some(vec![expected("vendor-c", "final-model")])
    );
}

#[test]
fn resolve_canonical_models_chain_carries_reasoning() {
    let models = registry(vec![
        model("vendor-a", "primary-model"),
        model("vendor-b", "fallback-model"),
    ]);
    let config = json!({ "categories": { "quick": { "models": [
        { "model": "vendor-a/primary-model", "reasoning": "high" },
        { "model": "vendor-b/fallback-model", "reasoning": "off" }
    ] } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-a");
    assert_eq!(resolved.spec.model_id, "primary-model");
    assert_eq!(resolved.spec.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        resolved.spec.fallback_models.expect("fallbacks")[0].model_id,
        "fallback-model"
    );
}

#[test]
fn resolve_canonical_models_win_over_legacy_fallbacks() {
    let models = registry(vec![model("vendor-a", "primary-model")]);
    let config = json!({ "categories": { "quick": {
        "models": [{ "model": "vendor-a/primary-model", "reasoning": "high" }],
        "model": "vendor-z/legacy-primary",
        "fallback_models": ["vendor-z/legacy-fallback"]
    } } });
    let resolved = expect_resolved(resolve("quick", config, &models));
    assert_eq!(resolved.spec.provider, "vendor-a");
    assert_eq!(resolved.spec.model_id, "primary-model");
}

#[test]
fn resolve_simple_category_keeps_canonical_reasoning() {
    let models = registry(vec![model("vendor-a", "solo-model")]);
    let config = json!({ "categories": { "deep": { "model": "vendor-a/solo-model", "reasoning": "medium" } } });
    let resolved = expect_resolved(resolve("deep", config, &models));
    assert_eq!(resolved.spec.model_id, "solo-model");
    assert_eq!(resolved.spec.reasoning_effort.as_deref(), Some("medium"));
}

#[test]
fn resolve_quick_chain_reaches_deepseek_flash() {
    let models = registry(vec![model("deepseek", "deepseek-flash")]);
    let resolved = expect_resolved(resolve("quick", json!({}), &models));
    assert_eq!(resolved.spec.provider, "deepseek");
    assert_eq!(resolved.spec.model_id, "deepseek-flash");
    assert_eq!(resolved.spec.variant.as_deref(), Some("off"));
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(&["deepseek"], "deepseek-flash", Some("off")))
    );
}

#[test]
fn resolve_writing_selects_the_opus_4_6_rung_at_max() {
    for models in [
        models_of(&[
            ("anthropic", "claude-fable-5-1"),
            ("anthropic", "claude-opus-4-6"),
        ]),
        models_of(&[("anthropic", "claude-opus-4-6")]),
    ] {
        let resolved = expect_resolved(resolve("writing", json!({}), &models));
        assert_eq!(
            spec_triple(&resolved),
            ("anthropic", "claude-opus-4-6", Some("max"))
        );
        assert!(resolved.selection.matched_fallback);
        assert_eq!(
            resolved.selection.fallback_entry,
            Some(entry(CLAUDE, "claude-opus-4-6", Some("max")))
        );
    }
}

#[test]
fn resolve_visual_engineering_kimi_rung_keeps_max() {
    let models = registry(vec![model("opencode-go", "kimi-k3.1-pro")]);
    let resolved = expect_resolved(resolve("visual-engineering", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("opencode-go", "kimi-k3.1-pro", Some("max"))
    );
    assert!(resolved.selection.matched_fallback);
    assert_eq!(
        resolved.selection.fallback_entry,
        Some(entry(
            &[
                "kimi-coding",
                "kimi-for-coding",
                "moonshotai",
                "opencode-go"
            ],
            "kimi-k3.1-pro",
            Some("max")
        ))
    );
}

#[test]
fn resolve_unlisted_vercel_gateway_resolves_no_gpt_category() {
    for case in gpt56_category_cases() {
        let models = registry(vec![model("vercel", &format!("openai/{}", case.model_id))]);
        let result = resolve(case.category, json!({}), &models);
        assert_eq!(result.kind(), "model_unavailable", "{}", case.category);
    }
}

#[test]
fn resolve_vercel_and_copilot_gpt56_listed_copilot_rung_wins() {
    for case in gpt56_category_cases() {
        let gateway_model_id = format!("openai/{}", case.model_id);
        let models = registry(vec![
            model("github-copilot", case.model_id),
            model("vercel", &gateway_model_id),
        ]);
        let resolved = expect_resolved(resolve(case.category, json!({}), &models));
        let (provider, model_id, variant) = case.mixed_winner;
        assert_eq!(spec_triple(&resolved), (provider, model_id, Some(variant)));
        assert_eq!(
            resolved.selection.fallback_entry.expect("entry").model,
            case.model_id
        );
    }
}

#[test]
fn resolve_copilot_gpt56_uses_copilot_rung() {
    for case in gpt56_category_cases() {
        let models = registry(vec![model("github-copilot", case.model_id)]);
        let resolved = expect_resolved(resolve(case.category, json!({}), &models));
        assert_eq!(
            spec_triple(&resolved),
            ("github-copilot", case.model_id, Some(case.copilot_variant))
        );
        assert_eq!(
            resolved.selection.fallback_entry,
            Some(case.copilot_fallback_entry)
        );
    }
}

#[test]
fn resolve_deep_never_selects_retired_gpt55_rung() {
    let result = resolve(
        "deep",
        json!({}),
        &registry(vec![model("github-copilot", "gpt-5.5")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
}

#[test]
fn resolve_absent_selected_model_names_attempted_and_available() {
    let models = registry(vec![model("anthropic", "claude-sonnet-4-6")]);
    let config = json!({ "categories": { "quick": { "model": "openai/not-installed" } } });
    let unavailable = expect_unavailable(resolve("quick", config, &models));
    assert_eq!(unavailable.category, "quick");
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("openai/not-installed")
    );
    assert_eq!(
        unavailable.available_models,
        vec!["anthropic/claude-sonnet-4-6".to_string()]
    );
    assert_eq!(unavailable.nearest_fallback, None);
}

#[test]
fn resolve_overlay_generation_params_reach_child_spec() {
    let models = registry(vec![model("chatgpt-subscription", "gpt-6-luna-fast")]);
    let config = json!({ "categories": { "quick": {
        "temperature": 0.3, "top_p": 0.8, "maxTokens": 4096,
        "thinking": { "type": "enabled", "budgetTokens": 1024 },
        "reasoningEffort": "medium",
        "tools": { "read": true, "write": false },
        "prompt_append": "fixture-quick-overlay"
    } } });
    let spec = expect_resolved(resolve("quick", config, &models)).spec;
    assert_eq!(spec.temperature, Some(0.3));
    assert_eq!(spec.top_p, Some(0.8));
    assert_eq!(spec.max_tokens, Some(4096));
    assert_eq!(
        spec.thinking,
        Some(json!({ "type": "enabled", "budgetTokens": 1024 }))
    );
    assert_eq!(spec.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(spec.tools, Some(json!({ "read": true, "write": false })));
    let prompt_append = spec.prompt_append.expect("prompt append");
    assert_ne!(prompt_append, "fixture-quick-overlay");
    assert!(prompt_append.ends_with("\n\nfixture-quick-overlay"));
}

#[test]
fn resolve_preserves_custom_description() {
    let models = registry(vec![model("openai", "custom-model")]);
    let config = json!({ "categories": { "custom-review": {
        "model": "openai/custom-model", "description": "Custom review lane"
    } } });
    let resolved = expect_resolved(resolve("custom-review", config, &models));
    assert_eq!(resolved.description.as_deref(), Some("Custom review lane"));
}

#[test]
fn fable_defaults_prefer_5_2_and_fall_back_to_5_1_at_the_same_effort() {
    for (name, variant) in [
        ("architect", "xhigh"),
        ("artistry", "max"),
        ("visual-engineering", "max"),
    ] {
        for ids in [
            &["claude-fable-5-2", "claude-fable-5-1"][..],
            &["claude-fable-5-1"][..],
        ] {
            let models = registry(ids.iter().map(|id| model("anthropic", id)).collect());
            let resolved = expect_resolved(resolve(name, json!({}), &models));
            assert_eq!(
                spec_triple(&resolved),
                ("anthropic", ids[0], Some(variant)),
                "{name}"
            );
        }
    }
}

#[test]
fn builtin_defaults_pin_machine_routing_fields() {
    let defaults: Vec<(&str, &str, Option<&str>)> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| {
            (
                definition.name,
                definition.config.model,
                definition.config.variant,
            )
        })
        .collect();
    assert_eq!(
        defaults,
        vec![
            ("visual-engineering", "google/gemini-4-argon", Some("low")),
            ("artistry", "anthropic/claude-fable-5-2", Some("max")),
            (
                "ultrabrain",
                "chatgpt-subscription/gpt-6.2-astra",
                Some("max")
            ),
            (
                "deep-low",
                "chatgpt-subscription/gpt-6.2-sol",
                Some("medium")
            ),
            (
                "deep-high",
                "chatgpt-subscription/gpt-6.2-astra",
                Some("high")
            ),
            ("quick", "chatgpt-subscription/gpt-6-luna-fast", Some("low")),
            (
                "unspecified-low",
                "anthropic/claude-sonnet-5-5",
                Some("medium")
            ),
            (
                "unspecified-high",
                "anthropic/claude-opus-5-5",
                Some("medium")
            ),
            ("architect", "anthropic/claude-fable-5-2", Some("xhigh")),
            ("writing", "google/gemini-4-argon", Some("low")),
        ]
    );
    let requires: Vec<(&str, Vec<&str>)> = builtin_category_requires_model()
        .into_iter()
        .map(|(name, models)| (name, models.to_vec()))
        .collect();
    assert_eq!(
        requires,
        vec![
            (
                "ultrabrain",
                vec!["gpt-6.2-astra", "gpt-6-astra", "gpt-5.6-sol"]
            ),
            (
                "deep-low",
                vec![
                    "gpt-6.2-sol",
                    "gpt-6.2-sol-fast",
                    "gpt-6.1-sol",
                    "gpt-6.1-sol-fast",
                    "gpt-5.6-sol-fast",
                    "gpt-5.6-sol"
                ]
            ),
            ("deep-high", vec!["gpt-6.2-astra", "gpt-6-astra"]),
            ("architect", vec!["claude-fable-5-2", "claude-fable-5-1"]),
        ]
    );
}

// ---- category/available-categories.test.ts ----

#[test]
fn available_offers_architect_for_either_fable_generation_on_listed_providers_only() {
    for (id, copilot_id) in [
        ("claude-fable-5-2", "claude-fable-5.2"),
        ("claude-fable-5-1", "claude-fable-5.1"),
    ] {
        assert!(contains(
            &available(json!({}), &models_of(&[("anthropic", id)])),
            "architect"
        ));
        assert!(contains(
            &available(json!({}), &models_of(&[("github-copilot", copilot_id)])),
            "architect"
        ));
        assert!(!contains(
            &available(json!({}), &models_of(&[("private-gateway", id)])),
            "architect"
        ));
    }
}

#[test]
fn available_exposes_architect_with_gate_model() {
    let names = available(json!({}), &models_of(&[("anthropic", "claude-fable-5-1")]));
    assert!(contains(&names, "architect"));
}

#[test]
fn available_hides_architect_without_gate_model() {
    let names = available(json!({}), &models_of(&[("omo-mock", "mock-weak")]));
    assert_eq!(names, Vec::<String>::new());
}

#[test]
fn available_explicit_declaration_exposes_architect() {
    let config =
        json!({ "categories": { "architect": { "model": "anthropic/claude-fable-5-1" } } });
    let names = available(config, &models_of(&[("omo-mock", "mock-weak")]));
    assert!(contains(&names, "architect"));
}

#[test]
fn available_throwing_registry_degrades_to_ungated_list() {
    let names = available(json!({}), &failing_registry("registry not ready"));
    assert!(contains(&names, "architect"));
}

// ---- category/anthropic-categories.test.ts ----

#[test]
fn architect_prefers_fable_5_2_at_xhigh_and_accepts_either_generation() {
    let architect = builtin_category("architect").expect("architect");
    assert_eq!(architect.config.model, "anthropic/claude-fable-5-2");
    assert_eq!(architect.config.variant, Some("xhigh"));
    assert_eq!(
        architect.requires_model.to_vec(),
        vec!["claude-fable-5-2", "claude-fable-5-1"]
    );
}

// ---- category/category-routing-policy.test.ts ----

#[test]
fn routed_categories_use_the_requested_primary_model_and_effort() {
    let routing: Vec<(&str, BuiltinCategoryConfig)> = [
        "visual-engineering",
        "quick",
        "unspecified-high",
        "unspecified-low",
    ]
    .into_iter()
    .map(|name| (name, builtin_category(name).expect("builtin").config))
    .collect();
    assert_eq!(
        routing,
        vec![
            (
                "visual-engineering",
                BuiltinCategoryConfig {
                    model: "google/gemini-4-argon",
                    variant: Some("low")
                }
            ),
            (
                "quick",
                BuiltinCategoryConfig {
                    model: "chatgpt-subscription/gpt-6-luna-fast",
                    variant: Some("low")
                }
            ),
            (
                "unspecified-high",
                BuiltinCategoryConfig {
                    model: "anthropic/claude-opus-5-5",
                    variant: Some("medium")
                }
            ),
            (
                "unspecified-low",
                BuiltinCategoryConfig {
                    model: "anthropic/claude-sonnet-5-5",
                    variant: Some("medium")
                }
            ),
        ]
    );
}

#[test]
fn unspecified_low_chain_is_sonnet_5_5_medium_first_and_excludes_luna() {
    let chain = chain("unspecified-low");
    assert!(!chain.iter().any(|rung| rung.model == "gpt-5.6-luna"));
    assert_eq!(
        chain,
        vec![
            entry(CLAUDE, "claude-sonnet-5-5", Some("medium")),
            entry(&["xiaomi", "opencode-go"], "mimo-v2.6-pro", Some("high")),
            entry(
                &["xai", "github-copilot", "opencode-go"],
                "grok-4.7",
                Some("xhigh")
            ),
            entry(
                &["zai", "zai-coding-cn", "opencode-go"],
                "glm-5.3",
                Some("high")
            ),
            entry(
                &["chatgpt-subscription", "openai"],
                "gpt-6.2-sol",
                Some("medium")
            ),
            entry(
                &["chatgpt-subscription", "openai"],
                "gpt-6.1-sol",
                Some("medium")
            ),
            entry(GPT_EVERY_LANE, "gpt-5.6-sol", Some("medium")),
            entry(CLAUDE, "claude-sonnet-5", Some("low")),
            entry(
                &[
                    "qwen-token-plan",
                    "alibaba-token-plan",
                    "qwen-token-plan-cn",
                    "alibaba-token-plan-cn"
                ],
                "qwen3.8-max-preview",
                Some("high")
            ),
            entry(&["deepseek", "opencode-go"], "deepseek-v4-pro", Some("max")),
            entry(&["xiaomi", "opencode-go"], "mimo-v2.5-pro", Some("high")),
        ]
    );
}

// ---- category/resolve-category-boundary.test.ts ----

const QUICK_DEFAULT: &str = "chatgpt-subscription/gpt-6-luna-fast";

fn quick_model() -> Value {
    model("chatgpt-subscription", "gpt-6-luna-fast")
}

fn assert_sanitized(result: CategoryResolutionResult, available: &[&str], hidden: &[&str]) {
    let rendered = format!("{result:?}");
    let unavailable = expect_unavailable(result);
    assert_eq!(unavailable.category, "quick");
    assert_eq!(unavailable.attempted_model.as_deref(), Some(QUICK_DEFAULT));
    assert_eq!(
        unavailable.available_models,
        available
            .iter()
            .map(|model| (*model).to_string())
            .collect::<Vec<_>>()
    );
    for marker in hidden {
        assert!(
            !rendered.contains(marker),
            "{marker} leaked into {rendered}"
        );
    }
}

#[test]
fn boundary_header_bearing_model_is_accepted() {
    let header_model = json!({
        "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "headers": { "User-Agent": "test" }
    });
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &registry(vec![header_model.clone()]),
    ));
    assert_eq!(resolved.spec.provider, "chatgpt-subscription");
    assert_eq!(resolved.spec.model_id, "gpt-6-luna-fast");
    assert_eq!(resolved.spec.model, header_model);
}

#[test]
fn boundary_malformed_available_entry_is_sanitized() {
    let result = resolve("quick", json!({}), &fixed_registry(json!([null]), None));
    assert_eq!(result.kind(), "model_unavailable");
    assert_sanitized(result, &[], &[]);
}

#[test]
fn boundary_non_string_available_identity_is_sanitized() {
    // A throwing accessor has no JSON equivalent; the nearest boundary input is a provider field
    // that is not a string, which must be dropped the same way without leaking its content.
    let entry = json!({ "provider": { "marker": "hidden available accessor marker" }, "id": "gpt-6-luna-fast" });
    let result = resolve("quick", json!({}), &fixed_registry(json!([entry]), None));
    assert_sanitized(result, &[], &["hidden available accessor marker"]);
}

#[test]
fn boundary_malformed_find_results_are_sanitized() {
    let malformed = [
        json!({}),
        json!({ "provider": { "secret": "hidden" }, "id": ["gpt-6-luna-fast"] }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "password": "hidden" }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "accessToken": "hidden" }),
        json!({ "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "privateToken": "hidden" }),
    ];
    for find_result in malformed {
        let models = fixed_registry(json!([quick_model()]), Some(find_result));
        assert_sanitized(
            resolve("quick", json!({}), &models),
            &[QUICK_DEFAULT],
            &["hidden"],
        );
    }
}

#[test]
fn boundary_non_string_find_identity_is_sanitized() {
    // Throwing-accessor analogue for `find`: a non-string provider is rejected without leaking.
    let find_result =
        json!({ "provider": ["hidden find accessor marker"], "id": "gpt-6-luna-fast" });
    let models = fixed_registry(json!([quick_model()]), Some(find_result));
    assert_sanitized(
        resolve("quick", json!({}), &models),
        &[QUICK_DEFAULT],
        &["hidden find accessor marker"],
    );
}

#[test]
fn boundary_empty_or_mismatched_find_identity_is_rejected() {
    let malformed = [
        json!({ "provider": "", "id": "" }),
        json!({ "provider": "evil", "id": "other" }),
        json!({ "provider": "chatgpt-subscription", "id": "" }),
    ];
    for find_result in malformed {
        let models = fixed_registry(json!([quick_model()]), Some(find_result));
        assert_sanitized(
            resolve("quick", json!({}), &models),
            &[QUICK_DEFAULT],
            &["evil", "other"],
        );
    }
}

#[test]
fn boundary_inherited_identity_fields_are_rejected() {
    // JSON has no prototype chain; the equivalent untrusted shape nests the identity one level
    // down, where it must not be read as the model's own fields.
    let find_result = json!({ "__proto__": {
        "provider": "chatgpt-subscription", "id": "gpt-6-luna-fast", "privateToken": "hidden"
    } });
    let models = fixed_registry(json!([quick_model()]), Some(find_result));
    assert_sanitized(
        resolve("quick", json!({}), &models),
        &[QUICK_DEFAULT],
        &["hidden"],
    );
}

#[test]
fn boundary_non_array_availability_is_sanitized() {
    let malformed = [
        Value::Null,
        json!({ "0": quick_model(), "length": 1 }),
        json!("chatgpt-subscription/gpt-6-luna-fast"),
    ];
    for available in malformed {
        let models = fixed_registry(available, Some(quick_model()));
        assert_sanitized(resolve("quick", json!({}), &models), &[], &[]);
    }
}

#[test]
fn boundary_prototype_shaped_names_are_not_found() {
    let models = registry(vec![quick_model()]);
    for category in ["__proto__", "toString", "hasOwnProperty"] {
        let result = resolve(category, json!({}), &models);
        assert_eq!(result.kind(), "not_found");
        assert!(contains(result.available_categories(), "quick"));
    }
}

// ---- model-exclusion.test.ts ----

fn claude_and_kimi() -> FakeRegistry {
    models_of(&[
        ("anthropic", "claude-fable-5-1"),
        ("anthropic", "claude-opus-5-5"),
        ("kimi-for-coding", "k3.1-pro"),
        ("openai", "gpt-6.1-sol"),
    ])
}

fn no_fable(config: Value) -> Value {
    let mut config = config;
    config["exclude_models"] = json!(["claude-fable-*"]);
    config
}

fn resolved_chain(result: CategoryResolutionResult) -> Vec<String> {
    let resolved = expect_resolved(result);
    std::iter::once(format!(
        "{}/{}",
        resolved.spec.provider, resolved.spec.model_id
    ))
    .chain(
        resolved
            .spec
            .fallback_models
            .unwrap_or_default()
            .into_iter()
            .map(|model| model.display),
    )
    .collect()
}

#[test]
fn excluded_fable_head_rung_leads_with_the_next_rung_and_fable_never_runs() {
    let result = resolve("artistry", no_fable(json!({})), &claude_and_kimi());
    assert_eq!(
        resolved_chain(result),
        vec!["anthropic/claude-opus-5-5", "kimi-for-coding/k3.1-pro"]
    );
}

#[test]
fn excluded_user_pin_is_treated_like_a_model_with_no_login() {
    let alone = resolve(
        "artistry",
        json!({
            "exclude_models": ["openai/gpt-6.1-sol"],
            "categories": { "artistry": { "model": "openai/gpt-6.1-sol" } }
        }),
        &claude_and_kimi(),
    );
    assert_eq!(alone.kind(), "model_unavailable");
    let walked = resolve(
        "artistry",
        json!({
            "exclude_models": ["openai/gpt-6.1-sol"],
            "categories": { "artistry": {
                "model": "openai/gpt-6.1-sol",
                "fallback_models": ["anthropic/claude-opus-5-5"]
            } }
        }),
        &claude_and_kimi(),
    );
    assert_eq!(
        resolved_chain(walked),
        vec!["anthropic/claude-opus-5-5", "kimi-for-coding/k3.1-pro"]
    );
}

#[test]
fn category_with_every_rung_excluded_is_gone_from_the_listing() {
    let fable_only = models_of(&[
        ("anthropic", "claude-fable-5-1"),
        ("anthropic", "claude-fable-5"),
    ]);
    assert!(!contains(
        &available(no_fable(json!({})), &fable_only),
        "artistry"
    ));
    assert!(contains(&available(json!({}), &fable_only), "artistry"));
}

#[test]
fn agent_whose_configured_model_is_excluded_skips_it() {
    let mut agents: Vec<(String, AgentDefinition)> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect();
    agents.push((
        "scout".to_string(),
        AgentDefinition {
            models: Some(vec![
                AgentModelEntry::Model("anthropic/claude-fable-5-1".to_string()),
                AgentModelEntry::Model("anthropic/claude-opus-5-5".to_string()),
            ]),
            ..AgentDefinition::named("scout")
        },
    ));
    let models = claude_and_kimi();
    let registry: &dyn SenpiModelRegistry = &models;
    let result = resolve_agent(
        "scout",
        &agents,
        Some(registry),
        &ResolveAgentOptions {
            omo_config: Some(ungated(no_fable(json!({})))),
            ..ResolveAgentOptions::default()
        },
    );
    let AgentResolutionResult::Resolved(resolved) = result else {
        panic!("Expected resolved agent, got {result:?}");
    };
    assert_eq!(resolved.model, "anthropic/claude-opus-5-5");
}

#[test]
fn not_found_listing_observes_excluded_models() {
    let opus_only = models_of(&[("anthropic", "claude-opus-5-5")]);
    let filtered = resolve(
        "no-such-category",
        json!({ "exclude_models": ["*"] }),
        &opus_only,
    );
    assert_eq!(filtered.kind(), "not_found");
    assert!(!contains(filtered.available_categories(), "artistry"));
    let unfiltered = resolve("no-such-category", json!({}), &opus_only);
    assert!(contains(unfiltered.available_categories(), "artistry"));
}

#[test]
fn empty_exclude_models_list_leaves_the_listing_alone() {
    let fable_only = models_of(&[("anthropic", "claude-fable-5-1")]);
    assert_eq!(
        available(json!({ "exclude_models": [] }), &fable_only),
        available(json!({}), &fable_only)
    );
}

#[test]
fn excluded_listing_still_degrades_on_a_malformed_registry() {
    let malformed = fixed_registry(json!({ "not": "an array" }), None);
    let names = available(json!({ "exclude_models": ["*"] }), &malformed);
    assert!(contains(&names, "architect"));
}
