use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::host::SenpiModelRegistry;
use crate::host::fake::{model, registry};
use crate::model_chain::ModelDistributor;
use crate::state::ResolvedModelRecord;

fn spawn(
    category: &str,
    config: &Value,
    models: &dyn SenpiModelRegistry,
    distributor: &Arc<ModelDistributor>,
) -> ResolvedChildSpec {
    let options = ResolveCategoryOptions {
        distribute_models: Some(distributor.clone()),
    };
    match resolve_category(category, config, models, &options).expect("resolve") {
        CategoryResolutionResult::Resolved { spec, .. } => *spec,
        other => panic!("expected a resolved category, got {other:?}"),
    }
}

fn displays(records: Option<Vec<ResolvedModelRecord>>) -> Vec<String> {
    records
        .unwrap_or_default()
        .into_iter()
        .map(|record| record.display)
        .collect()
}

fn distributed(policy: Option<&str>) -> Value {
    let mut definition = json!({
        "models": ["test-provider/a", { "model": "test-provider/b", "reasoning": "high" }, "test-provider/c"],
    });
    if let Some(policy) = policy {
        definition["models_distribution"] = json!(policy);
    }
    json!({ "categories": { "distributed": definition } })
}

fn abc() -> crate::host::fake::FakeRegistry {
    registry(vec![
        model("test-provider", "a"),
        model("test-provider", "b"),
        model("test-provider", "c"),
    ])
}

#[test]
fn round_robin_category_rotates_its_start_per_spawn_and_a_read_does_not_advance_it() {
    let config = distributed(Some("round_robin"));
    let models = abc();
    let distributor = Arc::new(ModelDistributor::new());
    let first = spawn("distributed", &config, &models, &distributor);
    let read = resolve_category(
        "distributed",
        &config,
        &models,
        &ResolveCategoryOptions::default(),
    )
    .expect("read");
    assert_eq!(read.kind(), "resolved");
    let second = spawn("distributed", &config, &models, &distributor);
    let third = spawn("distributed", &config, &models, &distributor);
    let fourth = spawn("distributed", &config, &models, &distributor);
    assert_eq!(
        [&first, &second, &third, &fourth].map(|spec| spec.model_id.as_str()),
        ["a", "b", "c", "a"]
    );
    assert_eq!(second.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        displays(second.fallback_models),
        ["test-provider/c", "test-provider/a"]
    );
}

#[test]
fn sequential_and_default_categories_keep_their_start_under_a_distributor() {
    for policy in [None, Some("sequential")] {
        let config = distributed(policy);
        let models = abc();
        let distributor = Arc::new(ModelDistributor::new());
        let starts: Vec<String> = (0..3)
            .map(|_| spawn("distributed", &config, &models, &distributor).model_id)
            .collect();
        assert_eq!(starts, ["a", "a", "a"], "{policy:?}");
        let spec = spawn("distributed", &config, &models, &distributor);
        assert_eq!(
            displays(spec.fallback_models),
            ["test-provider/b", "test-provider/c"],
            "{policy:?}"
        );
    }
}

#[test]
fn round_robin_skips_unavailable_starts_and_never_retries_a_duplicate() {
    let config = json!({ "categories": { "distributed": {
        "models": ["test-provider/missing", "test-provider/a", "test-provider/a:low", "test-provider/b"],
        "models_distribution": "round_robin",
    } } });
    let models = abc();
    let distributor = Arc::new(ModelDistributor::new());
    let plans: Vec<ResolvedChildSpec> = (0..3)
        .map(|_| spawn("distributed", &config, &models, &distributor))
        .collect();
    assert_eq!(
        plans
            .iter()
            .map(|spec| spec.model_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "a", "b"]
    );
    assert_eq!(
        displays(plans[2].fallback_models.clone()),
        ["test-provider/a"]
    );
}

#[test]
fn round_robin_canonical_chain_leaves_out_the_builtin_rungs() {
    // Builtin rungs only extend a chain whose selected model sits in the builtin chain, so the
    // configured list starts with quick's head rung and the next rung is what may leak in.
    let chain = category_fallback_chain("quick").expect("quick has a builtin chain");
    let rung_ref = |index: usize| {
        let rung = &chain[index];
        let provider = rung.providers.first().expect("rung provider");
        (provider.clone(), rung.model.clone())
    };
    let (head_provider, head_model) = rung_ref(0);
    let (next_provider, next_model) = rung_ref(1);
    let head = format!("{head_provider}/{head_model}");
    let next = format!("{next_provider}/{next_model}");
    let models = registry(vec![
        model("test-provider", "a"),
        model(&head_provider, &head_model),
        model(&next_provider, &next_model),
    ]);
    let quick = |policy: &str| {
        json!({ "categories": { "quick": {
            "models": [head.clone(), "test-provider/a"],
            "models_distribution": policy,
        } } })
    };
    let distributor = Arc::new(ModelDistributor::new());
    let sequential = spawn("quick", &quick("sequential"), &models, &distributor);
    assert!(
        displays(sequential.fallback_models).contains(&next),
        "a sequential chain keeps the builtin rungs after the configured models"
    );
    let rotating = spawn("quick", &quick("round_robin"), &models, &distributor);
    assert_eq!(rotating.model_id, head_model);
    assert_eq!(displays(rotating.fallback_models), ["test-provider/a"]);
}
