//! The Fable execution gate (`maho_ai::model_execution_gate`, docs/divergences.md GATE-1) as the
//! category and agent resolvers apply it: default on, a blocked rung is skipped like a model with
//! no login, a direct Fable pin is refused, and `model_policy.block_fable: false` restores upstream.

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::tests::{contains, expect_resolved, expect_unavailable, models_of};
use super::*;
use crate::agents::{
    AgentResolutionResult, BUILTIN_AGENT_DEFAULTS, ResolveAgentOptions, resolve_agent,
};
use crate::host::SenpiModelRegistry;
use crate::host::fake::FakeRegistry;

fn gated(category: &str, config: Value, models: &FakeRegistry) -> CategoryResolutionResult {
    resolve_category(
        category,
        &config,
        models,
        &ResolveCategoryOptions::default(),
    )
    .expect("resolve")
}

fn claude_lane() -> FakeRegistry {
    models_of(&[
        ("anthropic", "claude-fable-5-2"),
        ("anthropic", "claude-fable-5-1"),
        ("anthropic", "claude-opus-5-5"),
    ])
}

#[test]
fn with_the_gate_on_by_default_a_fable_head_rung_resolves_to_the_next_rung() {
    for category in ["artistry", "visual-engineering"] {
        let resolved = expect_resolved(gated(category, json!({}), &claude_lane()));
        assert_eq!(
            (
                resolved.spec.provider.as_str(),
                resolved.spec.model_id.as_str()
            ),
            ("anthropic", "claude-opus-5-5"),
            "{category}"
        );
        assert!(
            !resolved
                .spec
                .fallback_models
                .unwrap_or_default()
                .iter()
                .any(|fallback| fallback.model_id.contains("fable")),
            "{category}: a blocked Fable rung reached the runtime chain"
        );
    }
}

#[test]
fn with_the_gate_on_a_fable_only_category_is_unavailable_and_unlisted() {
    let unavailable = expect_unavailable(gated("architect", json!({}), &claude_lane()));
    assert!(!contains(&unavailable.available_categories, "architect"));
    let listed = resolve_available_category_names(&json!({}), &claude_lane());
    assert!(!contains(&listed, "architect"));
    assert!(contains(&listed, "artistry"));
}

#[test]
fn with_the_gate_on_a_direct_fable_pin_is_refused() {
    let unavailable = expect_unavailable(gated(
        "quick",
        json!({ "categories": { "quick": { "model": "anthropic/claude-fable-5-2" } } }),
        &claude_lane(),
    ));
    assert_eq!(
        unavailable.attempted_model.as_deref(),
        Some("anthropic/claude-fable-5-2")
    );
    assert!(
        !unavailable
            .available_models
            .iter()
            .any(|model| model.contains("fable"))
    );
}

#[test]
fn with_the_gate_off_fable_is_selected_like_upstream() {
    let off = json!({ "model_policy": { "block_fable": false } });
    let resolved = expect_resolved(gated("artistry", off.clone(), &claude_lane()));
    assert_eq!(resolved.spec.model_id, "claude-fable-5-2");
    let architect = expect_resolved(gated("architect", off, &claude_lane()));
    assert_eq!(
        (
            architect.spec.model_id.as_str(),
            architect.spec.variant.as_deref()
        ),
        ("claude-fable-5-2", Some("xhigh"))
    );
}

#[test]
fn an_explicit_true_setting_matches_the_default() {
    let on = json!({ "model_policy": { "block_fable": true } });
    assert_eq!(
        gated("artistry", on, &claude_lane()),
        gated("artistry", json!({}), &claude_lane())
    );
}

#[test]
fn with_the_gate_on_plan_consultant_skips_its_fable_rungs() {
    let agents: Vec<(String, crate::agents::AgentDefinition)> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| (definition.name.clone(), definition.clone()))
        .collect();
    let models = claude_lane();
    let registry: &dyn SenpiModelRegistry = &models;
    for (config, expected) in [
        (json!({}), "anthropic/claude-opus-5-5"),
        (
            json!({ "model_policy": { "block_fable": false } }),
            "anthropic/claude-fable-5-2",
        ),
    ] {
        let result = resolve_agent(
            "plan-consultant",
            &agents,
            Some(registry),
            &ResolveAgentOptions {
                omo_config: Some(config),
                ..ResolveAgentOptions::default()
            },
        );
        let AgentResolutionResult::Resolved(resolved) = result else {
            panic!("Expected resolved agent, got {result:?}");
        };
        assert_eq!(resolved.model, expected);
    }
}
