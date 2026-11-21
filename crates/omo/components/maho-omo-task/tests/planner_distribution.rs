use maho_omo_task::planner::create_task_child_planner;
use senpi_task::{
    agents::map_omo_config_agents,
    category::{ResolveCategoryOptions, resolve_category},
    host::{HostError, SenpiModelRegistry},
    manager::types::{ChildPlanner, ManagerStartSpec, ResolvedChildPlan},
};
use serde_json::{Value, json};
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

fn registry() -> Arc<Registry> {
    Arc::new(Registry(
        ["a", "b", "c"]
            .iter()
            .map(|id| json!({ "provider": "test-provider", "id": id }))
            .collect(),
    ))
}

fn spawn(planner: &ChildPlanner, target: &str, name: &str) -> ResolvedChildPlan {
    planner(&ManagerStartSpec {
        prompt: "fixture".into(),
        parent_session_id: "parent".into(),
        depth: 0,
        category: (target == "category").then(|| name.to_owned()),
        subagent_type: (target == "agent").then(|| name.to_owned()),
        ..Default::default()
    })
    .expect("resolved")
}

fn displays(plan: &ResolvedChildPlan) -> Vec<String> {
    plan.fallback_models
        .iter()
        .flatten()
        .map(|record| record.display.clone())
        .collect()
}

#[test]
fn unavailable_starts_skip_forward_and_duplicate_entries_are_never_retried() {
    let config = json!({ "categories": { "distributed": {
        "models": ["test-provider/missing", "test-provider/a", "test-provider/a:low", "test-provider/b"],
        "models_distribution": "round_robin",
    } } });
    let registry = registry();
    let planner =
        create_task_child_planner(config, vec![], Arc::new(move || Some(registry.clone())));
    let plans: Vec<ResolvedChildPlan> = (0..3)
        .map(|_| spawn(&planner, "category", "distributed"))
        .collect();
    assert_eq!(
        plans
            .iter()
            .map(|plan| plan.model.as_str())
            .collect::<Vec<_>>(),
        ["test-provider/a", "test-provider/a", "test-provider/b"]
    );
    assert_eq!(displays(&plans[2]), ["test-provider/a"]);
}

#[test]
fn categories_and_agents_start_and_fall_back_per_their_distribution_policy() {
    for target in ["category", "agent"] {
        for distribution in [None, Some("sequential"), Some("round_robin")] {
            let mut definition = json!({
                "models": ["test-provider/a", { "model": "test-provider/b", "reasoning": "high" }, "test-provider/c"],
            });
            if let Some(distribution) = distribution {
                definition["models_distribution"] = json!(distribution);
            }
            let config = json!({
                "categories": { "distributed": definition.clone(), "independent": definition.clone() },
                "agents": { "distributed": definition.clone(), "independent": definition },
            });
            let agents = map_omo_config_agents(&config);
            let registry = registry();
            let planner_registry = registry.clone();
            let planner = create_task_child_planner(
                config.clone(),
                agents,
                Arc::new(move || Some(planner_registry.clone())),
            );
            let case = format!("{target} {distribution:?}");
            let first = spawn(&planner, target, "distributed");
            // Introspection must not consume a spawn's turn.
            resolve_category(
                "distributed",
                &config,
                registry.as_ref(),
                &ResolveCategoryOptions::default(),
            )
            .expect("read");
            let second = spawn(&planner, target, "distributed");
            assert_eq!(
                spawn(&planner, target, "independent").model,
                "test-provider/a",
                "{case}"
            );
            let third = spawn(&planner, target, "distributed");
            let fourth = spawn(&planner, target, "distributed");
            let rotating = distribution == Some("round_robin");
            let starts = [&first, &second, &third, &fourth].map(|plan| plan.model.as_str());
            if rotating {
                assert_eq!(
                    starts,
                    [
                        "test-provider/a",
                        "test-provider/b",
                        "test-provider/c",
                        "test-provider/a"
                    ],
                    "{case}"
                );
                assert_eq!(
                    displays(&second),
                    ["test-provider/c", "test-provider/a"],
                    "{case}"
                );
                assert_eq!(
                    second
                        .resolved_model
                        .as_ref()
                        .and_then(|record| record.reasoning_effort.as_deref()),
                    Some("high"),
                    "{case}"
                );
            } else {
                assert_eq!(starts, ["test-provider/a"; 4], "{case}");
                assert_eq!(
                    displays(&second),
                    ["test-provider/b", "test-provider/c"],
                    "{case}"
                );
            }
        }
    }
}
