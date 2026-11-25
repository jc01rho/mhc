use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::tests::{
    contains, entry, expect_resolved, expect_unavailable, models_of, resolve, spec_triple, ungated,
};
use super::*;
use crate::agents::{
    AgentDefinition, AgentResolutionResult, BUILTIN_AGENT_DEFAULTS, ResolveAgentOptions,
    ResolvedAgent, resolve_agent,
};
use crate::host::SenpiModelRegistry;
use crate::host::fake::FakeRegistry;
use crate::state::ResolvedModelSource;

fn builtin_agents() -> Vec<(String, AgentDefinition)> {
    BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect()
}

fn resolve_builtin_agent(
    name: &str,
    agents: &[(String, AgentDefinition)],
    models: &FakeRegistry,
) -> AgentResolutionResult {
    let registry: &dyn SenpiModelRegistry = models;
    resolve_agent(
        name,
        agents,
        Some(registry),
        &ResolveAgentOptions {
            omo_config: Some(ungated(json!({}))),
            ..ResolveAgentOptions::default()
        },
    )
}

fn expect_resolved_agent(result: AgentResolutionResult) -> ResolvedAgent {
    match result {
        AgentResolutionResult::Resolved(resolved) => *resolved,
        other => panic!("Expected resolved agent, got {other:?}"),
    }
}

// ---- category/fallback-chains.test.ts ----

const CATEGORY_NAMES: [&str; 10] = [
    "visual-engineering",
    "architect",
    "ultrabrain",
    "deep-low",
    "deep-high",
    "artistry",
    "quick",
    "unspecified-low",
    "unspecified-high",
    "writing",
];

#[test]
fn exactly_the_ten_category_names_carry_a_chain() {
    let mut names: Vec<&str> = CATEGORY_FALLBACK_CHAINS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    names.sort_unstable();
    let mut expected = CATEGORY_NAMES.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
}

#[test]
fn the_mirrored_fallback_table_matches_the_independent_transcription() {
    let expected: Vec<(&str, Vec<crate::delegate_adapter::DelegateFallbackEntry>)> = vec![
        (
            "visual-engineering",
            vec![
                entry(
                    &["google", "google-vertex", "opencode"],
                    "gemini-4-argon",
                    Some("low"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-2",
                    Some("max"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-1",
                    Some("max"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-opus-5-5",
                    Some("max"),
                ),
                entry(
                    &[
                        "kimi-coding",
                        "kimi-for-coding",
                        "moonshotai",
                        "opencode-go",
                    ],
                    "kimi-k3.1-pro",
                    Some("max"),
                ),
            ],
        ),
        (
            "architect",
            vec![
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-2",
                    Some("xhigh"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-1",
                    Some("xhigh"),
                ),
            ],
        ),
        (
            "ultrabrain",
            vec![entry(
                &["chatgpt-subscription", "openai", "opencode"],
                "gpt-6.2-astra",
                Some("max"),
            )],
        ),
        (
            "deep-low",
            vec![entry(
                &["chatgpt-subscription", "openai"],
                "gpt-6.2-sol",
                Some("medium"),
            )],
        ),
        (
            "deep-high",
            vec![entry(
                &["chatgpt-subscription", "openai", "opencode"],
                "gpt-6.2-astra",
                Some("high"),
            )],
        ),
        (
            "artistry",
            vec![
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-2",
                    Some("max"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-fable-5-1",
                    Some("max"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-opus-5-5",
                    Some("max"),
                ),
                entry(
                    &[
                        "kimi-coding",
                        "kimi-for-coding",
                        "moonshotai",
                        "opencode-go",
                    ],
                    "kimi-k3.1-pro",
                    Some("max"),
                ),
            ],
        ),
        (
            "quick",
            vec![
                entry(
                    &["chatgpt-subscription", "openai"],
                    "gpt-6-luna-fast",
                    Some("low"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                    ],
                    "claude-haiku-5-5",
                    Some("medium"),
                ),
                entry(&["deepseek"], "deepseek-flash", Some("off")),
                entry(
                    &[
                        "qwen-token-plan",
                        "alibaba-token-plan",
                        "bailian-coding-plan",
                    ],
                    "qwen3.6-flash",
                    Some("low"),
                ),
                entry(&["opencode-go"], "minimax-m3", None),
                entry(&["opencode-go"], "minimax-m2.7", None),
                entry(&["xai"], "grok-4.20-0309-non-reasoning", None),
                entry(&["zai", "zai-coding-cn"], "glm-5.3-flash", Some("low")),
                entry(&["xiaomi"], "mimo-v2.6-flash", Some("low")),
            ],
        ),
        (
            "unspecified-low",
            vec![
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-sonnet-5-5",
                    Some("medium"),
                ),
                entry(&["xiaomi", "opencode-go"], "mimo-v2.6-pro", Some("high")),
                entry(
                    &["xai", "github-copilot", "opencode-go"],
                    "grok-4.7",
                    Some("xhigh"),
                ),
                entry(
                    &["zai", "zai-coding-cn", "opencode-go"],
                    "glm-5.3",
                    Some("high"),
                ),
                entry(
                    &["chatgpt-subscription", "openai"],
                    "gpt-6.2-sol",
                    Some("medium"),
                ),
                entry(
                    &["chatgpt-subscription", "openai"],
                    "gpt-6.1-sol",
                    Some("medium"),
                ),
                entry(
                    &[
                        "chatgpt-subscription",
                        "openai",
                        "github-copilot",
                        "opencode",
                    ],
                    "gpt-5.6-sol",
                    Some("medium"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-sonnet-5",
                    Some("low"),
                ),
                entry(
                    &[
                        "qwen-token-plan",
                        "alibaba-token-plan",
                        "qwen-token-plan-cn",
                        "alibaba-token-plan-cn",
                    ],
                    "qwen3.8-max-preview",
                    Some("high"),
                ),
                entry(&["deepseek", "opencode-go"], "deepseek-v4-pro", Some("max")),
                entry(&["xiaomi", "opencode-go"], "mimo-v2.5-pro", Some("high")),
            ],
        ),
        (
            "unspecified-high",
            vec![
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-opus-5-5",
                    Some("medium"),
                ),
                entry(
                    &["zai", "zai-coding-cn", "opencode-go"],
                    "glm-5.3",
                    Some("max"),
                ),
                entry(
                    &[
                        "kimi-coding",
                        "kimi-for-coding",
                        "moonshotai",
                        "opencode-go",
                    ],
                    "kimi-k3.1-pro",
                    Some("max"),
                ),
            ],
        ),
        (
            "writing",
            vec![
                entry(
                    &["google", "google-vertex", "opencode"],
                    "gemini-4-argon",
                    Some("low"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-opus-5-5",
                    Some("low"),
                ),
                entry(
                    &[
                        "anthropic-subscription",
                        "anthropic",
                        "anthropic-api",
                        "github-copilot",
                        "opencode",
                    ],
                    "claude-opus-4-6",
                    Some("max"),
                ),
            ],
        ),
    ];
    assert_eq!(*CATEGORY_FALLBACK_CHAINS, expected);
}

#[test]
fn no_rung_lists_vercel_or_quotio_and_only_gpt_rungs_list_the_openai_lane() {
    for (name, rungs) in CATEGORY_FALLBACK_CHAINS.iter() {
        for rung in rungs {
            let listed =
                |provider: &str| rung.providers.iter().any(|candidate| candidate == provider);
            assert!(!listed("vercel"), "{name} rung {} lists vercel", rung.model);
            assert!(
                !listed("quotio-openai"),
                "{name} rung {} lists quotio-openai",
                rung.model
            );
            if !rung.model.starts_with("gpt-") {
                assert!(
                    !listed("openai"),
                    "{name} rung {} lists the openai lane",
                    rung.model
                );
            }
        }
    }
}

#[test]
fn every_gpt_rung_leads_with_the_subscription_lane_then_the_openai_lane() {
    for (name, rungs) in CATEGORY_FALLBACK_CHAINS.iter() {
        for rung in rungs {
            if !rung.model.starts_with("gpt-")
                || rung
                    .providers
                    .iter()
                    .all(|provider| provider == "github-copilot")
            {
                continue;
            }
            let subscription = rung
                .providers
                .iter()
                .position(|provider| provider == "chatgpt-subscription")
                .unwrap_or_else(|| panic!("{name} rung {} skips chatgpt-subscription", rung.model));
            assert_eq!(
                rung.providers.get(subscription + 1).map(String::as_str),
                Some("openai"),
                "{name} rung {}",
                rung.model
            );
        }
    }
}

// ---- category/listed-providers-only.test.ts ----

fn gateway(provider: &str, ids: &[&str]) -> Vec<Value> {
    ids.iter()
        .map(|id| crate::host::fake::model(provider, id))
        .collect()
}

fn openrouter_only() -> Vec<Value> {
    gateway(
        "openrouter",
        &[
            "anthropic/claude-fable-5",
            "anthropic/claude-fable-5.1",
            "anthropic/claude-opus-5.5",
            "anthropic/claude-opus-4.6",
            "anthropic/claude-sonnet-5",
            "anthropic/claude-haiku-4.5",
            "openai/gpt-6-astra",
            "openai/gpt-5.6-sol",
            "openai/gpt-5.6-terra",
            "openai/gpt-6-luna",
            "moonshotai/kimi-k3",
            "moonshotai/kimi-k3.1-pro",
            "z-ai/glm-5.3",
            "~deepseek/deepseek-flash-latest",
            "deepseek/deepseek-v4-pro",
            "xiaomi/mimo-v2.6-pro",
            "xiaomi/mimo-v2.5-pro",
            "x-ai/grok-4.7",
        ],
    )
}

fn opengateway_only() -> Vec<Value> {
    gateway(
        "opengateway",
        &[
            "anthropic/claude-fable-5-1",
            "anthropic/claude-opus-5-5",
            "anthropic/claude-haiku-4-5",
            "openai/gpt-6-astra",
            "openai/gpt-5.6-sol",
            "openai/gpt-5.6-terra",
            "moonshotai/kimi-k3",
            "moonshotai/kimi-k3.1-pro",
            "z-ai/glm-5.3",
            "deepseek/deepseek-flash",
            "deepseek/deepseek-v4-pro",
        ],
    )
}

fn with(mut models: Vec<Value>, extra: &[(&str, &str)]) -> FakeRegistry {
    models.extend(
        extra
            .iter()
            .map(|(provider, id)| crate::host::fake::model(provider, id)),
    );
    crate::host::fake::registry(models)
}

#[test]
fn a_gateway_only_registry_resolves_no_builtin_category_or_agent() {
    for (gateway_name, models) in [
        ("OpenRouter", openrouter_only()),
        ("opengateway", opengateway_only()),
    ] {
        let models = crate::host::fake::registry(models);
        for definition in BUILTIN_CATEGORY_DEFAULTS.iter() {
            let result = resolve(definition.name, json!({}), &models);
            assert_eq!(
                result.kind(),
                "model_unavailable",
                "{gateway_name} {}",
                definition.name
            );
            assert!(!contains(result.available_categories(), definition.name));
        }
        assert_eq!(
            resolve_available_category_names(&ungated(json!({})), &models),
            Vec::<String>::new()
        );
        for (agent, _) in builtin_agents() {
            let result = resolve_builtin_agent(&agent, &builtin_agents(), &models);
            assert!(
                matches!(result, AgentResolutionResult::ModelUnavailable { .. }),
                "{gateway_name} {agent}: {result:?}"
            );
        }
    }
}

#[test]
fn plan_consultant_never_lands_on_the_gateway_and_takes_the_listed_lane_once_connected() {
    let result = resolve_builtin_agent(
        "plan-consultant",
        &builtin_agents(),
        &crate::host::fake::registry(openrouter_only()),
    );
    assert!(matches!(
        result,
        AgentResolutionResult::ModelUnavailable { .. }
    ));
    let resolved = expect_resolved_agent(resolve_builtin_agent(
        "plan-consultant",
        &builtin_agents(),
        &with(openrouter_only(), &[("anthropic", "claude-opus-5-5")]),
    ));
    let record = resolved.resolved_model.expect("resolved model");
    assert_eq!(record.source, ResolvedModelSource::Agent);
    assert_eq!(record.provider, "anthropic");
    assert_eq!(record.model_id, "claude-opus-5-5");
    assert_eq!(record.display, "anthropic/claude-opus-5-5");
    assert_eq!(record.reasoning.or(record.variant).as_deref(), Some("max"));
}

#[test]
fn a_category_hidden_by_a_gateway_names_the_exact_model_to_opt_into() {
    let models = crate::host::fake::registry(openrouter_only());
    let unavailable = expect_unavailable(resolve("writing", json!({}), &models));
    assert_eq!(
        unavailable.unlisted_provider_model.as_deref(),
        Some("openrouter/anthropic/claude-opus-5.5")
    );
    let coverage = resolve_category_coverage(&ungated(json!({})), &models).expect("coverage");
    assert_eq!(coverage.usable, Vec::<String>::new());
    assert_eq!(
        coverage
            .unusable
            .iter()
            .find(|gap| gap.name == "writing")
            .and_then(|gap| gap.unlisted_provider_model.as_deref()),
        Some("openrouter/anthropic/claude-opus-5.5")
    );
    let pinned =
        json!({ "categories": { "writing": { "model": "openrouter/anthropic/claude-opus-5.5" } } });
    assert_eq!(
        resolve("writing", pinned.clone(), &models).kind(),
        "resolved"
    );
    assert_eq!(
        resolve_category_coverage(&ungated(pinned), &models)
            .expect("coverage")
            .usable,
        vec!["writing".to_string()]
    );
}

#[test]
fn an_explicit_gateway_pin_is_honored() {
    let models = crate::host::fake::registry(openrouter_only());
    let visual = expect_resolved(resolve(
        "visual-engineering",
        json!({ "categories": { "visual-engineering": { "model": "openrouter/anthropic/claude-opus-5.5" } } }),
        &models,
    ));
    assert_eq!(visual.spec.provider, "openrouter");
    assert_eq!(visual.spec.model_id, "anthropic/claude-opus-5.5");
    let pinned =
        json!({ "categories": { "ultrabrain": { "models": ["openrouter/openai/gpt-6-astra"] } } });
    let ultrabrain = expect_resolved(resolve("ultrabrain", pinned.clone(), &models));
    assert_eq!(ultrabrain.spec.provider, "openrouter");
    assert_eq!(ultrabrain.spec.model_id, "openai/gpt-6-astra");
    assert_eq!(
        resolve_available_category_names(&ungated(pinned), &models),
        vec!["ultrabrain".to_string()]
    );
    let mut agents = builtin_agents();
    for (name, definition) in &mut agents {
        if name == "plan-consultant" {
            definition.model = Some("openrouter/anthropic/claude-opus-5.5".to_string());
        }
    }
    let agent = expect_resolved_agent(resolve_builtin_agent("plan-consultant", &agents, &models));
    assert_eq!(agent.model, "openrouter/anthropic/claude-opus-5.5");
}

#[test]
fn a_listed_provider_beats_the_gateways_copy() {
    let high = expect_resolved(resolve(
        "unspecified-high",
        json!({}),
        &with(openrouter_only(), &[("anthropic", "claude-opus-5-5")]),
    ));
    assert_eq!(high.spec.provider, "anthropic");
    assert!(
        !high
            .spec
            .fallback_models
            .unwrap_or_default()
            .iter()
            .any(|fallback| fallback.provider == "openrouter")
    );
    let ultrabrain = expect_resolved(resolve(
        "ultrabrain",
        json!({}),
        &with(
            openrouter_only(),
            &[
                ("openrouter", "openai/gpt-6.2-astra"),
                ("opencode", "gpt-6.2-astra"),
            ],
        ),
    ));
    assert_eq!(ultrabrain.spec.provider, "opencode");
    assert_eq!(ultrabrain.spec.model_id, "gpt-6.2-astra");
    let agent = expect_resolved_agent(resolve_builtin_agent(
        "plan-consultant",
        &builtin_agents(),
        &with(openrouter_only(), &[("moonshotai", "kimi-k3.1-pro")]),
    ));
    assert_eq!(agent.model, "moonshotai/kimi-k3.1-pro");
}

// ---- category/quick-single-provider.test.ts ----

const ZAI_MODELS: [&str; 5] = [
    "glm-4.7",
    "glm-5.2",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-highspeed",
];
const XIAOMI_MODELS: [&str; 3] = ["mimo-v2.5-pro", "mimo-v2.6-flash", "mimo-v2.6-pro"];

fn only(provider: &str, ids: &[&str]) -> FakeRegistry {
    crate::host::fake::registry(gateway(provider, ids))
}

#[test]
fn quick_on_a_single_provider_machine_runs_its_flash_model_at_low() {
    for (provider, ids, model) in [
        ("zai", &ZAI_MODELS[..], "glm-5.3-flash"),
        ("zai-coding-cn", &ZAI_MODELS[..], "glm-5.3-flash"),
        ("xiaomi", &XIAOMI_MODELS[..], "mimo-v2.6-flash"),
    ] {
        let resolved = expect_resolved(resolve("quick", json!({}), &only(provider, ids)));
        assert_eq!(spec_triple(&resolved), (provider, model, Some("low")));
        assert!(contains(&resolved.available_categories, "quick"));
    }
}

#[test]
fn quick_prefers_haiku_5_5_medium_over_glm() {
    let mut models = gateway("zai", &ZAI_MODELS);
    models.push(crate::host::fake::model("anthropic", "claude-haiku-5-5"));
    let resolved = expect_resolved(resolve(
        "quick",
        json!({}),
        &crate::host::fake::registry(models),
    ));
    assert_eq!(
        spec_triple(&resolved),
        ("anthropic", "claude-haiku-5-5", Some("medium"))
    );
}

// ---- category/unspecified-low-chain.test.ts ----

const FULL_UNSPECIFIED_LOW: [(&str, &str); 8] = [
    ("anthropic", "claude-sonnet-5-5"),
    ("xiaomi", "mimo-v2.6-pro"),
    ("xai", "grok-4.7"),
    ("chatgpt-subscription", "gpt-5.6-terra"),
    ("anthropic", "claude-sonnet-5"),
    ("qwen-token-plan", "qwen3.8-max-preview"),
    ("deepseek", "deepseek-v4-pro"),
    ("xiaomi", "mimo-v2.5-pro"),
];

fn unspecified_low_without(absent: &[&str]) -> FakeRegistry {
    let pairs: Vec<(&str, &str)> = FULL_UNSPECIFIED_LOW
        .iter()
        .copied()
        .filter(|(_, id)| !absent.contains(id))
        .collect();
    models_of(&pairs)
}

#[test]
fn unspecified_low_walks_its_chain_in_order() {
    for (absent, winner) in [
        (&[][..], ("anthropic", "claude-sonnet-5-5", Some("medium"))),
        (
            &["claude-sonnet-5-5"][..],
            ("xiaomi", "mimo-v2.6-pro", Some("high")),
        ),
        (
            &["claude-sonnet-5-5", "mimo-v2.6-pro"][..],
            ("xai", "grok-4.7", Some("xhigh")),
        ),
    ] {
        let resolved = expect_resolved(resolve(
            "unspecified-low",
            json!({}),
            &unspecified_low_without(absent),
        ));
        assert_eq!(spec_triple(&resolved), winner);
    }
}

#[test]
fn the_retired_grok_4_6_no_longer_resolves_unspecified_low() {
    let result = resolve(
        "unspecified-low",
        json!({}),
        &models_of(&[("xai", "grok-4.6")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
}

#[test]
fn glm_5_3_wins_ahead_of_gpt_without_the_sonnet_head() {
    let resolved = expect_resolved(resolve(
        "unspecified-low",
        json!({}),
        &models_of(&[
            ("zai", "glm-5.3"),
            ("chatgpt-subscription", "gpt-6.1-sol"),
            ("chatgpt-subscription", "gpt-5.6-sol"),
            ("anthropic-subscription", "claude-sonnet-5"),
        ]),
    ));
    assert_eq!(spec_triple(&resolved), ("zai", "glm-5.3", Some("high")));
}

// ---- category/argon-tier1.test.ts ----

#[test]
fn argon_low_heads_visual_engineering_and_writing() {
    for name in ["visual-engineering", "writing"] {
        let chain = category_fallback_chain(name).expect("chain");
        assert_eq!(
            chain[0],
            entry(
                &["google", "google-vertex", "opencode"],
                "gemini-4-argon",
                Some("low")
            )
        );
        let config = builtin_category(name).expect("builtin").config;
        assert_eq!(
            (config.model, config.variant),
            ("google/gemini-4-argon", Some("low"))
        );
    }
}

#[test]
fn a_registry_serving_argon_makes_it_a_primary_hit() {
    let models = models_of(&[
        ("google", "gemini-4-argon"),
        ("anthropic", "claude-fable-5-1"),
        ("anthropic", "claude-opus-5-5"),
    ]);
    for name in ["visual-engineering", "writing"] {
        let resolved = expect_resolved(resolve(name, json!({}), &models));
        assert_eq!(
            spec_triple(&resolved),
            ("google", "gemini-4-argon", Some("low"))
        );
        assert!(!resolved.selection.matched_fallback);
    }
}

#[test]
fn without_argon_both_land_on_their_previous_head_rung() {
    let models = models_of(&[
        ("anthropic", "claude-fable-5-1"),
        ("anthropic", "claude-opus-5-5"),
    ]);
    let visual = expect_resolved(resolve("visual-engineering", json!({}), &models));
    let writing = expect_resolved(resolve("writing", json!({}), &models));
    assert_eq!(
        spec_triple(&visual),
        ("anthropic", "claude-fable-5-1", Some("max"))
    );
    assert_eq!(
        spec_triple(&writing),
        ("anthropic", "claude-opus-5-5", Some("low"))
    );
}

// ---- category/anthropic-lane.test.ts ----

fn subscription_and_metered() -> FakeRegistry {
    let mut pairs = Vec::new();
    for id in ["claude-fable-5-1", "claude-opus-5-5", "claude-sonnet-4-6"] {
        pairs.push(("opencode", id));
        pairs.push(("anthropic-subscription", id));
    }
    models_of(&pairs)
}

#[test]
fn the_subscription_lane_wins_for_architect_unspecified_high_and_plan_consultant() {
    let models = subscription_and_metered();
    let architect = expect_resolved(resolve("architect", json!({}), &models));
    assert_eq!(
        spec_triple(&architect),
        ("anthropic-subscription", "claude-fable-5-1", Some("xhigh"))
    );
    assert_eq!(
        architect
            .selection
            .fallback_entry
            .as_ref()
            .and_then(|entry| entry.providers.first())
            .map(String::as_str),
        Some("anthropic-subscription")
    );
    let high = expect_resolved(resolve("unspecified-high", json!({}), &models));
    assert_eq!(
        spec_triple(&high),
        ("anthropic-subscription", "claude-opus-5-5", Some("medium"))
    );
    let agent = expect_resolved_agent(resolve_builtin_agent(
        "plan-consultant",
        &builtin_agents(),
        &models,
    ));
    assert_eq!(agent.model, "anthropic-subscription/claude-fable-5-1");
    let record = agent.resolved_model.expect("resolved model");
    assert_eq!(record.provider, "anthropic-subscription");
    assert_eq!(record.model_id, "claude-fable-5-1");
}

#[test]
fn without_the_subscription_lane_selection_matches_the_pre_lane_table() {
    let api_key = models_of(&[
        ("opencode", "claude-fable-5-1"),
        ("github-copilot", "claude-fable-5-1"),
        ("anthropic", "claude-fable-5-1"),
        ("opencode", "claude-sonnet-4-6"),
        ("anthropic", "claude-sonnet-4-6"),
    ]);
    let metered_only = models_of(&[
        ("opencode", "claude-fable-5-1"),
        ("opencode", "claude-opus-5-5"),
    ]);
    let architect = expect_resolved(resolve("architect", json!({}), &api_key));
    assert_eq!(
        spec_triple(&architect),
        ("anthropic", "claude-fable-5-1", Some("xhigh"))
    );
    assert_eq!(
        architect
            .spec
            .requested_model
            .as_ref()
            .map(|record| record.display.as_str()),
        Some("anthropic/claude-fable-5-2")
    );
    assert_eq!(
        architect.selection.selected_model,
        "anthropic/claude-fable-5-1"
    );
    assert!(architect.selection.matched_fallback);
    let metered_architect = expect_resolved(resolve("architect", json!({}), &metered_only));
    assert_eq!(
        spec_triple(&metered_architect),
        ("opencode", "claude-fable-5-1", Some("xhigh"))
    );
    assert_eq!(
        metered_architect.selection.selected_model,
        "opencode/claude-fable-5-1"
    );
    assert!(metered_architect.selection.matched_fallback);
    let rung = metered_architect
        .selection
        .fallback_entry
        .as_ref()
        .expect("fallback entry");
    assert_eq!(
        (rung.model.as_str(), rung.variant.as_deref()),
        ("claude-fable-5-1", Some("xhigh"))
    );
    let metered_high = expect_resolved(resolve("unspecified-high", json!({}), &metered_only));
    assert_eq!(
        spec_triple(&metered_high),
        ("opencode", "claude-opus-5-5", Some("medium"))
    );
    assert_eq!(
        metered_high
            .spec
            .requested_model
            .as_ref()
            .map(|record| record.display.as_str()),
        Some("anthropic/claude-opus-5-5")
    );
    let agent = expect_resolved_agent(resolve_builtin_agent(
        "plan-consultant",
        &builtin_agents(),
        &api_key,
    ));
    assert_eq!(agent.model, "anthropic/claude-fable-5-1");
    assert_eq!(
        agent
            .resolved_model
            .as_ref()
            .map(|record| record.display.as_str()),
        Some("anthropic/claude-fable-5-1")
    );
    assert!(agent.fallback_models.unwrap_or_default().is_empty());
}

// ---- category/openai-lane.test.ts ----

const GPT_CATEGORY_CASES: [(&str, &str, &str); 3] = [
    ("ultrabrain", "gpt-6.2-astra", "max"),
    ("deep-high", "gpt-6.2-astra", "high"),
    ("deep-low", "gpt-6.2-sol", "medium"),
];

#[test]
fn the_subscription_lane_wins_when_both_gpt_lanes_serve_the_model() {
    for (category, model_id, variant) in GPT_CATEGORY_CASES {
        let models = models_of(&[("openai", model_id), ("chatgpt-subscription", model_id)]);
        let resolved = expect_resolved(resolve(category, json!({}), &models));
        assert_eq!(
            spec_triple(&resolved),
            ("chatgpt-subscription", model_id, Some(variant)),
            "{category}"
        );
    }
}

#[test]
fn the_subscription_lane_wins_even_when_the_api_lane_is_listed_first_and_no_lane_carries_cross_model_fallback()
 {
    let models = models_of(&[
        ("openai", "gpt-6.2-astra"),
        ("openai", "gpt-6.2-sol"),
        ("openai", "gpt-6-astra"),
        ("openai", "gpt-5.6-sol"),
        ("chatgpt-subscription", "gpt-5.6-sol"),
        ("chatgpt-subscription", "gpt-6-astra"),
        ("chatgpt-subscription", "gpt-6.2-sol"),
        ("chatgpt-subscription", "gpt-6.2-astra"),
    ]);
    let results: Vec<_> = ["ultrabrain", "deep-high", "deep-low"]
        .into_iter()
        .map(|category| expect_resolved(resolve(category, json!({}), &models)))
        .collect();
    let displays: Vec<String> = results
        .iter()
        .map(|resolved| format!("{}/{}", resolved.spec.provider, resolved.spec.model_id))
        .collect();
    assert_eq!(
        displays,
        vec![
            "chatgpt-subscription/gpt-6.2-astra".to_string(),
            "chatgpt-subscription/gpt-6.2-astra".to_string(),
            "chatgpt-subscription/gpt-6.2-sol".to_string(),
        ]
    );
    for resolved in results {
        assert!(resolved.spec.fallback_models.unwrap_or_default().is_empty());
    }
}

#[test]
fn the_listed_api_lane_alone_keeps_each_gpt_category_usable() {
    for (category, model_id, variant) in GPT_CATEGORY_CASES {
        let resolved = expect_resolved(resolve(
            category,
            json!({}),
            &models_of(&[("openai", model_id)]),
        ));
        assert_eq!(spec_triple(&resolved), ("openai", model_id, Some(variant)));
        assert!(contains(&resolved.available_categories, category));
    }
}

#[test]
fn deep_high_on_the_subscription_lane_requests_gpt_6_2_astra() {
    let resolved = expect_resolved(resolve(
        "deep-high",
        json!({}),
        &models_of(&[("chatgpt-subscription", "gpt-6.2-astra")]),
    ));
    assert_eq!(resolved.spec.provider, "chatgpt-subscription");
    assert_eq!(resolved.spec.model_id, "gpt-6.2-astra");
    assert_eq!(
        resolved
            .spec
            .requested_model
            .as_ref()
            .map(|record| record.display.as_str()),
        Some("chatgpt-subscription/gpt-6.2-astra")
    );
}

// ---- category/gpt-5.6-sol-routing.test.ts ----

#[test]
fn opencode_sol_alone_serves_unspecified_low_while_exact_gpt62_lanes_are_unavailable() {
    let models = models_of(&[("opencode", "gpt-5.6-sol")]);
    for category in ["ultrabrain", "deep-low", "deep-high"] {
        let result = resolve(category, json!({}), &models);
        assert_eq!(result.kind(), "model_unavailable", "{category}");
        assert!(!contains(result.available_categories(), category));
    }
    let resolved = expect_resolved(resolve("unspecified-low", json!({}), &models));
    assert_eq!(
        spec_triple(&resolved),
        ("opencode", "gpt-5.6-sol", Some("medium"))
    );
}

#[test]
fn unspecified_low_uses_the_xhigh_grok_rung() {
    let resolved = expect_resolved(resolve(
        "unspecified-low",
        json!({}),
        &models_of(&[("xai", "grok-4.7")]),
    ));
    assert_eq!(spec_triple(&resolved), ("xai", "grok-4.7", Some("xhigh")));
}

#[test]
fn an_unlisted_vercel_terra_never_serves_unspecified_low() {
    let result = resolve(
        "unspecified-low",
        json!({}),
        &models_of(&[("vercel", "openai/gpt-5.6-terra")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
}
