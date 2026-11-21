use maho_omo_task::model_routing_rows::{
    ModelRoutingKind, ModelRoutingPlannerInputs, ModelRoutingRow, ModelRoutingSource,
    resolve_model_routing_rows,
};
use maho_omo_task::model_routing_rpc::{
    MODEL_ROUTING_RESOLVE_RPC_METHOD, ModelRoutingConfig, ModelRoutingRpcDeps, answer_model_routing,
};
use maho_omo_task::planner::create_task_child_planner;
use senpi_task::{
    agents::{AGENT_FALLBACK_CHAINS, AgentResolutionResult, ResolveAgentOptions, resolve_agent},
    category::{
        BUILTIN_CATEGORY_DEFAULTS, CATEGORY_FALLBACK_CHAINS, CategoryResolutionResult,
        ResolveCategoryOptions, resolve_category,
    },
    host::{HostError, SenpiModelRegistry},
    manager::types::ManagerStartSpec,
    resolve_task_agents,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Registry(Vec<Value>);

impl SenpiModelRegistry for Registry {
    fn get_available(&self) -> Result<Value, HostError> {
        Ok(Value::Array(self.0.clone()))
    }

    fn find(&self, provider: &str, id: &str) -> Option<Value> {
        self.0
            .iter()
            .find(|model| model["provider"] == provider && model["id"] == id)
            .cloned()
    }
}

fn user_lab(id: &str) -> Value {
    json!({ "provider": "user-lab", "id": id })
}

fn full() -> Registry {
    let mut models: Vec<Value> = CATEGORY_FALLBACK_CHAINS
        .iter()
        .chain(AGENT_FALLBACK_CHAINS.iter())
        .flat_map(|(_, chain)| chain.iter())
        .filter_map(|rung| {
            rung.providers
                .first()
                .map(|provider| json!({ "provider": provider, "id": rung.model }))
        })
        .collect();
    models.push(user_lab("second-model"));
    Registry(models)
}

fn dead() -> Registry {
    Registry(vec![json!({ "provider": "nobody", "id": "nothing" })])
}

fn inputs(config: &Value) -> ModelRoutingPlannerInputs {
    ModelRoutingPlannerInputs {
        agents: resolve_task_agents(config),
        parent_service_tier: None,
    }
}

fn row<'a>(rows: &'a [ModelRoutingRow], kind: ModelRoutingKind, name: &str) -> &'a ModelRoutingRow {
    let found: Vec<&ModelRoutingRow> = rows
        .iter()
        .filter(|candidate| candidate.kind == kind && candidate.name == name)
        .collect();
    assert_eq!(found.len(), 1, "{kind:?} {name}: {rows:?}");
    found[0]
}

#[test]
fn category_rows_are_exactly_the_builtin_categories_without_user_config() {
    let rows = resolve_model_routing_rows(&json!({}), &full(), &inputs(&json!({})));
    let names: Vec<&str> = rows
        .iter()
        .filter(|candidate| candidate.kind == ModelRoutingKind::Category)
        .map(|candidate| candidate.name.as_str())
        .collect();
    let mut expected: Vec<&str> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name)
        .collect();
    expected.sort();
    assert_eq!(names, expected);
}

#[test]
fn every_builtin_category_row_is_the_resolvers_own_pick() {
    let registry = full();
    let rows = resolve_model_routing_rows(&json!({}), &registry, &inputs(&json!({})));
    for definition in BUILTIN_CATEGORY_DEFAULTS.iter() {
        let target = row(&rows, ModelRoutingKind::Category, definition.name);
        match resolve_category(
            definition.name,
            &json!({}),
            &registry,
            &ResolveCategoryOptions::default(),
        )
        .expect("resolve")
        {
            CategoryResolutionResult::Resolved { spec, .. } => {
                let variant = spec
                    .reasoning
                    .clone()
                    .or_else(|| spec.reasoning_effort.clone())
                    .or_else(|| spec.variant.clone());
                assert_eq!(
                    target,
                    &ModelRoutingRow {
                        kind: ModelRoutingKind::Category,
                        name: definition.name.to_string(),
                        effective: Some(format!("{}/{}", spec.provider, spec.model_id)),
                        source: ModelRoutingSource::ProviderFallback,
                        variant,
                    }
                );
            }
            _ => assert_eq!(target.source, ModelRoutingSource::Unresolved),
        }
    }
}

#[test]
fn a_user_models_chain_whose_head_is_gone_routes_to_the_promoted_user_rung() {
    for name in ["quick", "unspecified-high"] {
        let config = json!({ "categories": { (name): {
            "models": ["user-lab/absent-model", "user-lab/second-model"],
        } } });
        let rows = resolve_model_routing_rows(&config, &full(), &inputs(&config));
        let target = row(&rows, ModelRoutingKind::Category, name);
        assert_eq!(target.effective.as_deref(), Some("user-lab/second-model"));
        assert_eq!(target.source, ModelRoutingSource::Override);
    }
}

#[test]
fn a_custom_category_and_a_disabled_builtin_each_keep_their_own_row() {
    let config = json!({ "categories": {
        "my-lane": { "model": "user-lab/second-model" },
        "quick": { "disable": true },
    } });
    let rows = resolve_model_routing_rows(&config, &full(), &inputs(&config));
    assert_eq!(
        row(&rows, ModelRoutingKind::Category, "my-lane"),
        &ModelRoutingRow {
            kind: ModelRoutingKind::Category,
            name: "my-lane".to_string(),
            effective: Some("user-lab/second-model".to_string()),
            source: ModelRoutingSource::Override,
            variant: None,
        }
    );
    assert_eq!(
        row(&rows, ModelRoutingKind::Category, "quick"),
        &ModelRoutingRow {
            kind: ModelRoutingKind::Category,
            name: "quick".to_string(),
            effective: None,
            source: ModelRoutingSource::Unresolved,
            variant: None,
        }
    );
}

#[test]
fn every_task_agent_row_is_the_resolvers_own_pick() {
    let registry = full();
    let agents = resolve_task_agents(&json!({}));
    let rows = resolve_model_routing_rows(&json!({}), &registry, &inputs(&json!({})));
    for (name, _) in &agents {
        let target = row(&rows, ModelRoutingKind::Agent, name);
        match resolve_agent(
            name,
            &agents,
            Some(&registry),
            &ResolveAgentOptions {
                omo_config: Some(json!({})),
                ..ResolveAgentOptions::default()
            },
        ) {
            AgentResolutionResult::Resolved(resolution) => {
                let variant = resolution.resolved_model.as_ref().and_then(|record| {
                    record
                        .reasoning
                        .clone()
                        .or_else(|| record.reasoning_effort.clone())
                        .or_else(|| record.variant.clone())
                });
                assert_eq!(
                    target,
                    &ModelRoutingRow {
                        kind: ModelRoutingKind::Agent,
                        name: name.clone(),
                        effective: Some(resolution.model.clone()),
                        source: ModelRoutingSource::ProviderFallback,
                        variant,
                    }
                );
            }
            _ => assert_eq!(target.source, ModelRoutingSource::Unresolved),
        }
    }
}

#[test]
fn a_user_model_on_an_agent_routes_to_it_as_an_override() {
    let config = json!({ "agents": { "explore": { "model": "user-lab/second-model" } } });
    let rows = resolve_model_routing_rows(&config, &full(), &inputs(&config));
    let target = row(&rows, ModelRoutingKind::Agent, "explore");
    assert_eq!(target.effective.as_deref(), Some("user-lab/second-model"));
    assert_eq!(target.source, ModelRoutingSource::Override);
}

#[test]
fn a_dead_registry_leaves_every_row_unresolved() {
    let rows = resolve_model_routing_rows(&json!({}), &dead(), &inputs(&json!({})));
    assert!(!rows.is_empty());
    for target in &rows {
        assert_eq!(target.effective, None, "{target:?}");
        assert_eq!(target.source, ModelRoutingSource::Unresolved, "{target:?}");
    }
}

#[test]
fn a_round_robin_lane_shows_its_first_entry_and_a_read_never_advances_the_rotation() {
    let config = json!({ "categories": { "distributed": {
        "models": ["user-lab/second-model", "user-lab/third-model"],
        "models_distribution": "round_robin",
    } } });
    let registry = Arc::new(Registry(vec![
        user_lab("second-model"),
        user_lab("third-model"),
    ]));
    let planner_registry = registry.clone();
    let planner = create_task_child_planner(
        config.clone(),
        vec![],
        Arc::new(move || Some(planner_registry.clone())),
    );
    let spawn = || {
        planner(&ManagerStartSpec {
            prompt: "fixture".into(),
            parent_session_id: "parent".into(),
            category: Some("distributed".into()),
            ..Default::default()
        })
        .expect("resolved")
        .model
    };
    assert_eq!(spawn(), "user-lab/second-model");
    let rows = resolve_model_routing_rows(
        &config,
        registry.as_ref(),
        &ModelRoutingPlannerInputs::default(),
    );
    assert_eq!(
        row(&rows, ModelRoutingKind::Category, "distributed")
            .effective
            .as_deref(),
        Some("user-lab/second-model")
    );
    assert_eq!(spawn(), "user-lab/third-model");
}

fn deps(
    cwd: PathBuf,
    config: ModelRoutingConfig,
    registry: Option<Arc<Registry>>,
) -> ModelRoutingRpcDeps {
    ModelRoutingRpcDeps {
        cwd: Arc::new(move || cwd.clone()),
        load_config: Arc::new(move |_: &Path| config.clone()),
        registry: Arc::new(move || {
            registry
                .clone()
                .map(|registry| registry as Arc<dyn SenpiModelRegistry>)
        }),
        planner: Arc::new(ModelRoutingPlannerInputs::default),
    }
}

#[test]
fn the_desktop_reads_the_routing_under_its_shipped_method_name() {
    assert_eq!(MODEL_ROUTING_RESOLVE_RPC_METHOD, "omo.modelRouting.resolve");
}

#[test]
fn an_answer_for_a_missing_cwd_is_a_cwd_unavailable_refusal() {
    let missing = tempfile::tempdir().expect("tempdir").path().join("gone");
    let answer = answer_model_routing(&deps(
        missing,
        ModelRoutingConfig::default(),
        Some(Arc::new(full())),
    ));
    assert_eq!(answer["kind"], "error");
    assert_eq!(answer["code"], "cwd_unavailable");
}

#[test]
fn an_answer_over_an_unreadable_layer_is_a_config_invalid_refusal() {
    let cwd = tempfile::tempdir().expect("tempdir");
    let answer = answer_model_routing(&deps(
        cwd.path().to_path_buf(),
        ModelRoutingConfig {
            config: json!({}),
            unreadable: vec!["omo.jsonc: unexpected end of input".to_string()],
        },
        Some(Arc::new(full())),
    ));
    assert_eq!(answer["code"], "config_invalid");
    assert_eq!(answer["message"], "omo.jsonc: unexpected end of input");
}

#[test]
fn an_answer_without_a_registry_is_a_registry_unavailable_refusal() {
    let cwd = tempfile::tempdir().expect("tempdir");
    let answer = answer_model_routing(&deps(
        cwd.path().to_path_buf(),
        ModelRoutingConfig::default(),
        None,
    ));
    assert_eq!(answer["code"], "registry_unavailable");
}

#[test]
fn an_answer_carries_schema_version_one_and_the_rows() {
    let cwd = tempfile::tempdir().expect("tempdir");
    let answer = answer_model_routing(&deps(
        cwd.path().to_path_buf(),
        ModelRoutingConfig {
            config: json!({ "categories": {
                "my-lane": { "model": "user-lab/second-model" },
                "quick": { "disable": true },
            } }),
            unreadable: Vec::new(),
        },
        Some(Arc::new(full())),
    ));
    assert_eq!(answer["schemaVersion"], 1);
    let rows = answer["rows"].as_array().expect("rows");
    assert!(rows.contains(&json!({
        "kind": "category",
        "name": "my-lane",
        "effective": "user-lab/second-model",
        "source": "override",
    })));
    assert!(rows.contains(&json!({
        "kind": "category",
        "name": "quick",
        "effective": null,
        "source": "unresolved",
    })));
}
