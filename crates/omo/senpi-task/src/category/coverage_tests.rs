use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::tests::ungated;
use super::*;
use crate::host::fake::{FakeRegistry, fixed_registry, model, registry};

fn registry_of(provider: &str, ids: &[&str]) -> FakeRegistry {
    registry(ids.iter().map(|id| model(provider, id)).collect())
}

fn coverage(config: Value, models: &FakeRegistry) -> CategoryCoverage {
    resolve_category_coverage(&ungated(config), models).expect("coverage")
}

const ZAI_MODELS: [&str; 7] = [
    "glm-4.7",
    "glm-5-turbo",
    "glm-5.2",
    "glm-5.2-highspeed",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-highspeed",
];
const ANTHROPIC_MODELS: [&str; 17] = [
    "claude-fable-5",
    "claude-fable-5-1",
    "claude-haiku-4-5",
    "claude-haiku-4-5-20251001",
    "claude-haiku-5-5",
    "claude-opus-4-5",
    "claude-opus-4-5-20251101",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-opus-5-5",
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "claude-sonnet-5-5",
];
// The GPT-6.2 Astra lanes list the providers serving that exact model; Copilot does not serve GPT-6.2.
const ASTRA_PROVIDERS: [&str; 3] = ["chatgpt-subscription", "openai", "opencode"];
// GPT-6.2 Sol is served only on the two OpenAI lanes.
const SOL_PROVIDERS: [&str; 2] = ["chatgpt-subscription", "openai"];

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn gap(name: &str, providers: &[&str]) -> UnusableCategory {
    UnusableCategory {
        name: name.to_string(),
        providers: strings(providers),
        unlisted_provider_model: None,
    }
}

// ---- category/coverage.test.ts ----

#[test]
fn a_zai_only_registry_serves_three_categories_and_names_each_gaps_providers() {
    let coverage = coverage(json!({}), &registry_of("zai", &ZAI_MODELS));
    assert_eq!(
        coverage.usable,
        strings(&["quick", "unspecified-high", "unspecified-low"])
    );
    assert_eq!(
        coverage
            .unusable
            .iter()
            .map(|gap| gap.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "architect",
            "artistry",
            "deep-high",
            "deep-low",
            "ultrabrain",
            "visual-engineering",
            "writing"
        ]
    );
    let providers_of = |name: &str| {
        coverage
            .unusable
            .iter()
            .find(|gap| gap.name == name)
            .map(|gap| gap.providers.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        providers_of("architect"),
        strings(&[
            "anthropic-subscription",
            "anthropic",
            "anthropic-api",
            "github-copilot",
            "opencode"
        ])
    );
    assert_eq!(providers_of("deep-high"), strings(&ASTRA_PROVIDERS));
    assert!(
        !providers_of("writing")
            .iter()
            .any(|provider| provider == "zai")
    );
}

#[test]
fn an_anthropic_only_registry_serves_the_claude_categories_and_names_the_gpt_providers() {
    let coverage = coverage(json!({}), &registry_of("anthropic", &ANTHROPIC_MODELS));
    assert_eq!(
        coverage.usable,
        strings(&[
            "architect",
            "artistry",
            "quick",
            "unspecified-high",
            "unspecified-low",
            "visual-engineering",
            "writing",
        ])
    );
    assert_eq!(
        coverage.unusable,
        vec![
            gap("deep-high", &ASTRA_PROVIDERS),
            gap("deep-low", &SOL_PROVIDERS),
            gap("ultrabrain", &ASTRA_PROVIDERS),
        ]
    );
}

#[test]
fn a_pinned_builtin_is_usable_and_a_disabled_one_is_neither_usable_nor_a_gap() {
    let coverage = coverage(
        json!({ "categories": { "quick": { "model": "zai/glm-5.3-flash" }, "writing": { "disable": true } } }),
        &registry_of("zai", &ZAI_MODELS),
    );
    assert_eq!(
        coverage.usable,
        strings(&["quick", "unspecified-high", "unspecified-low"])
    );
    assert!(!coverage.usable.iter().any(|name| name == "writing"));
    assert!(!coverage.unusable.iter().any(|gap| gap.name == "writing"));
}

#[test]
fn deep_high_gaps_leave_out_the_connected_openai_lane() {
    let coverage = coverage(json!({}), &registry_of("openai", &["gpt-4o"]));
    assert_eq!(
        coverage
            .unusable
            .iter()
            .find(|gap| gap.name == "deep-high")
            .map(|gap| gap.providers.clone()),
        Some(strings(&["chatgpt-subscription", "opencode"]))
    );
}

#[test]
fn a_registry_without_a_model_list_is_an_error() {
    assert!(resolve_category_coverage(&json!({}), &fixed_registry(Value::Null, None)).is_err());
}
