use std::collections::HashMap;

use model_core::CapabilitySource;
use model_core::ContextLimitEnv;
use model_core::ContextLimitModelCacheState;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilitiesSnapshot;
use model_core::SnapshotSource;
use model_core::get_bundled_model_capabilities_snapshot;
use model_core::get_model_capabilities;
use model_core::is_claude_fable_or_mythos_model;
use model_core::is_claude_fable5_model;
use model_core::is_claude_opus47_or_later_model;
use model_core::latest_models::LATEST_MODEL_BY_FAMILY;
use model_core::resolve_actual_context_limit_with_env;
use model_core::transform_model_for_provider;
use pretty_assertions::assert_eq;

const MODEL: &str = "claude-fable-5-2";

fn empty_bundled() -> ModelCapabilitiesSnapshot {
    get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot::default())
}

fn limit(provider: &str, model: &str) -> Option<i64> {
    resolve_actual_context_limit_with_env(
        provider,
        model,
        Some(&ContextLimitModelCacheState::default()),
        ContextLimitEnv::default(),
    )
}

#[test]
fn is_the_current_fable_family_pin() {
    let claude_fable = LATEST_MODEL_BY_FAMILY
        .iter()
        .find(|(family, _)| *family == "claudeFable")
        .map(|(_, model)| *model);
    assert_eq!(claude_fable, Some(MODEL));
}

#[test]
fn recognizes_every_fable_and_mythos_5_2_spelling_as_the_adaptive_only_family() {
    let fable_selectors = [
        MODEL,
        "anthropic/claude-fable-5-2",
        "github-copilot/claude-fable-5.2",
        "anthropic.claude-fable-5-2",
        "us.anthropic.claude-fable-5-2",
    ];
    for selector in fable_selectors
        .iter()
        .copied()
        .chain(std::iter::once("claude-mythos-5-2"))
    {
        assert!(is_claude_fable_or_mythos_model(selector), "{selector}");
    }
    for selector in fable_selectors {
        assert!(
            is_claude_opus47_or_later_model(selector),
            "{selector} shares the adaptive-only request surface"
        );
    }
    assert!(is_claude_fable5_model("anthropic/claude-fable-5-2"));
}

#[test]
fn registers_the_launch_capabilities_without_depending_on_a_generated_catalog_row() {
    let snapshot = empty_bundled();

    let result = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "anthropic",
        model_id: MODEL,
        bundled_snapshot: Some(&snapshot),
        ..Default::default()
    });

    assert_eq!(result.family.as_deref(), Some("claude-fable"));
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::BundledSnapshot);
    assert_eq!(result.max_output_tokens, Some(128_000));
    assert_eq!(result.tool_call, Some(true));
    let modalities = result.modalities.as_ref().expect("launch modalities");
    assert_eq!(
        modalities.input.as_deref(),
        Some(["text", "image", "pdf"].as_slice())
    );
    assert_eq!(modalities.output.as_deref(), Some(["text"].as_slice()));
    assert_eq!(
        result.diagnostics.max_output_tokens,
        CapabilitySource::BundledSnapshot
    );
}

#[test]
fn keeps_ids_just_outside_the_5_2_boundary_at_the_default_window() {
    let outside = [
        ("anthropic", "claude-fable-5-20"),
        ("anthropic", "claude-fable-5-2-2026110"),
        ("anthropic", "claude-fable-5-21"),
        ("anthropic", "claude-opus-5-2"),
        ("anthropic", "claude-fable-5-1-20260601"),
        ("aws-bedrock-anthropic", "anthropic.claude-fable-5-20"),
    ];
    let limits: Vec<Option<i64>> = outside
        .iter()
        .map(|(provider, model)| limit(provider, model))
        .collect();
    assert_eq!(
        limits,
        outside.iter().map(|_| Some(200_000)).collect::<Vec<_>>()
    );
}

#[test]
fn uses_the_1m_context_window_on_every_anthropic_family_spelling() {
    let spellings = [
        ("anthropic", MODEL),
        ("anthropic", "claude-fable-5.2"),
        ("anthropic", "claude-fable-5-2-20261109"),
        ("google-vertex-anthropic", "claude-fable-5-2@default"),
        ("aws-bedrock-anthropic", "anthropic.claude-fable-5-2"),
        ("aws-bedrock-anthropic", "us.anthropic.claude-fable-5-2"),
        ("anthropic", "claude-mythos-5-2"),
    ];
    let limits: Vec<Option<i64>> = spellings
        .iter()
        .map(|(provider, model)| limit(provider, model))
        .collect();
    assert_eq!(
        limits,
        spellings
            .iter()
            .map(|_| Some(1_000_000))
            .collect::<Vec<_>>()
    );
}

#[test]
fn keeps_a_provider_that_does_not_serve_5_2_on_its_own_listed_limit() {
    let state = ContextLimitModelCacheState {
        anthropic_context_1m_enabled: false,
        model_context_limits_cache: Some(HashMap::from([(
            "opengateway/claude-fable-5-2".to_string(),
            345_678,
        )])),
    };

    assert_eq!(
        resolve_actual_context_limit_with_env(
            "opengateway",
            MODEL,
            Some(&state),
            ContextLimitEnv::default()
        ),
        Some(345_678)
    );
}

#[test]
fn routes_copilot_to_its_dotted_id_and_leaves_the_anthropic_id_unchanged() {
    assert_eq!(
        transform_model_for_provider("github-copilot", MODEL),
        "claude-fable-5.2"
    );
    assert_eq!(transform_model_for_provider("anthropic", MODEL), MODEL);
}
