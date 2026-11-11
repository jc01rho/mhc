//! Upstream `components/task/model-error-text.test.ts`.
use maho_omo_task::model_error_text::*;
use senpi_task::{
    DelegateFallbackEntry,
    agents::{AgentDefinition, AgentModelCandidate, AgentModelEntry},
    manager::types::{PlanResolutionCode, PlanResolutionError},
};
use serde_json::json;

fn chain() -> Vec<DelegateFallbackEntry> {
    vec![
        DelegateFallbackEntry {
            providers: vec!["kimi-coding".into(), "kimi-for-coding".into()],
            model: "kimi-for-coding-highspeed".into(),
            variant: None,
        },
        DelegateFallbackEntry {
            providers: vec!["openai".into()],
            model: "gpt-6-luna-fast".into(),
            variant: None,
        },
    ]
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn dead_category_chain_names_every_model_provider_fix_key_and_no_spawn() {
    let missing = strings(&["kimi-coding", "kimi-for-coding", "openai"]);
    let text = chain_unavailable_text(
        ChainFailureTarget::Category("quick"),
        &chain(),
        &missing,
        None,
    );
    for rung in chain() {
        assert!(text.contains(&rung.model), "{text}");
        for provider in &rung.providers {
            assert!(text.contains(provider.as_str()), "{text}");
        }
    }
    assert!(text.contains("categories.quick.model"));
    assert!(text.contains("No child was spawned"));
}

#[test]
fn dead_agent_chain_reads_as_the_agent_and_names_agents_key() {
    let text = chain_unavailable_text(
        ChainFailureTarget::Agent("explore"),
        &chain(),
        &strings(&["kimi-coding"]),
        None,
    );
    assert!(text.starts_with("Agent \"explore\""), "{text}");
    assert!(text.contains("agents.explore.model"));
    assert!(text.contains("No child was spawned"));
}

#[test]
fn only_an_unlisted_provider_serving_a_rung_offers_the_opt_in_pin() {
    let unlisted = "openrouter/moonshotai/kimi-k3";
    let text = chain_unavailable_text(
        ChainFailureTarget::Category("quick"),
        &chain(),
        &strings(&["kimi-coding"]),
        Some(unlisted),
    );
    assert!(
        text.contains(&format!("categories.quick.model = \"{unlisted}\"")),
        "{text}"
    );
    assert!(text.contains("openrouter"));
}

#[test]
fn a_refused_pin_names_the_pin_and_the_settings_key() {
    let pinned = "custom-proxy/gpt-6-astra";
    let text =
        pinned_model_unavailable_text(ChainFailureTarget::Category("quick"), pinned, PinKey::Model);
    assert!(text.contains(pinned));
    assert!(text.contains("categories.quick.model"));
    assert!(text.contains("No child was spawned"));
}

#[test]
fn connected_providers_that_do_not_list_the_rung_models_are_not_called_missing() {
    let text = chain_unavailable_text(ChainFailureTarget::Category("quick"), &chain(), &[], None);
    assert!(!text.contains("not connected"), "{text}");
    assert!(text.contains("none of them lists these model ids"));
    assert!(text.contains("categories.quick.model"));
}

#[test]
fn a_retired_category_alias_names_the_canonical_key() {
    let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, "resolver text");
    error.category = Some("old-alias".into());
    error.attempted_chain = Some(chain());
    error.missing_providers = Some(strings(&["openai"]));
    let text = category_model_failure_text(&error, &json!({}), Some("quick")).expect("refusal");
    assert!(text.contains("categories.quick.model"), "{text}");
    assert!(!text.contains("old-alias"));
}

#[test]
fn a_category_pinned_through_a_models_list_names_that_pin_instead_of_the_dead_chain() {
    let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, "resolver text");
    error.category = Some("quick".into());
    let omo_config = json!({"categories":{"quick":{"models":["custom-proxy/gpt-6-astra"]}}});
    let text = category_model_failure_text(&error, &omo_config, None).expect("refusal");
    assert!(text.contains("custom-proxy/gpt-6-astra"), "{text}");
    assert!(text.contains("is pinned to"));
    assert!(text.contains("categories.quick.models"));
    assert!(!text.contains("its chain needs"));
}

#[test]
fn a_category_with_neither_chain_nor_pin_keeps_the_resolver_text() {
    let mut error = PlanResolutionError::new(PlanResolutionCode::ModelUnavailable, "resolver text");
    error.category = Some("quick".into());
    assert_eq!(category_model_failure_text(&error, &json!({}), None), None);
}

#[test]
fn a_pinned_agent_reached_through_a_retired_alias_is_named_under_the_canonical_agent() {
    let agents = vec![(
        "omo-native-code-reviewer".to_owned(),
        AgentDefinition {
            models: Some(vec![AgentModelEntry::Candidate(AgentModelCandidate {
                model: "custom-proxy/gpt-6-astra".into(),
                ..AgentModelCandidate::default()
            })]),
            ..AgentDefinition::named("omo-native-code-reviewer")
        },
    )];
    let chain = chain();
    let missing = strings(&["openai"]);
    let text = agent_model_failure_text(
        "omo-senpi-code-reviewer",
        AgentUnavailable {
            agent: Some("omo-native-code-reviewer"),
            attempted_model: None,
            attempted_chain: Some(&chain),
            missing_providers: Some(&missing),
        },
        &agents,
    );
    assert!(text.contains("custom-proxy/gpt-6-astra"), "{text}");
    assert!(text.contains("agents.omo-native-code-reviewer.models"));
    assert!(!text.contains("omo-senpi-code-reviewer"));
}
