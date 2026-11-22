//! `tools/task/gated-categories.test.ts`

use serde_json::{Value, json};

use crate::category::category_gate_models;
use crate::tools::task::categories::{TaskCategoryInfo, list_task_categories};

fn required_annotation(name: &str) -> String {
    let gate_models = category_gate_models(name)
        .unwrap_or_else(|| panic!("builtin category {name} is not model-gated"));
    format!("(requires {})", gate_models.join(" or "))
}

fn entry_for(name: &str, config: &Value) -> Option<TaskCategoryInfo> {
    list_task_categories(config)
        .into_iter()
        .find(|entry| entry.name == name)
}

fn description_of(name: &str, config: &Value) -> String {
    entry_for(name, config)
        .and_then(|entry| entry.description)
        .unwrap_or_default()
}

#[test]
fn given_builtin_only_gated_category_when_listed_then_architect_carries_required_model_annotation()
{
    assert!(
        description_of("architect", &json!({}))
            .contains("(requires claude-fable-5-2 or claude-fable-5-1)")
    );
}

#[test]
fn given_builtin_only_gated_category_when_listed_then_ultrabrain_carries_required_model_annotation()
{
    assert!(description_of("ultrabrain", &json!({})).contains(&required_annotation("ultrabrain")));
}

#[test]
fn given_builtin_only_gated_category_when_listed_then_each_deep_lane_carries_its_own_annotation() {
    let low = description_of("deep-low", &json!({}));
    let high = description_of("deep-high", &json!({}));
    assert!(low.contains(
        "(requires gpt-6.2-sol or gpt-6.2-sol-fast or gpt-6.1-sol or gpt-6.1-sol-fast or gpt-5.6-sol-fast or gpt-5.6-sol)"
    ));
    assert!(low.contains(&required_annotation("deep-low")));
    assert!(!low.contains("gpt-6-sol"));
    assert!(high.contains("(requires gpt-6.2-astra or gpt-6-astra)"));
    assert!(high.contains(&required_annotation("deep-high")));
}

#[test]
fn given_gated_category_in_omo_json_when_listed_then_annotation_is_dropped() {
    let config = json!({ "categories": { "architect": { "model": "kimi-coding/k3" } } });
    assert!(!description_of("architect", &config).contains("requires"));
}

#[test]
fn given_gated_category_description_override_when_listed_then_user_text_is_verbatim() {
    let config = json!({ "categories": { "architect": { "description": "House architect" } } });
    let entry = entry_for("architect", &config).expect("architect listed");
    pretty_assertions::assert_eq!(entry.description.as_deref(), Some("House architect"));
}

#[test]
fn given_ungated_builtin_category_when_listed_then_no_annotation_is_added() {
    let entry = entry_for("quick", &json!({})).expect("quick listed");
    let description = entry.description.expect("quick has a description");
    assert!(!description.contains("requires"));
}
