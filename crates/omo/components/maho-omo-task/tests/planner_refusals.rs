//! Upstream `planner.test.ts` "createTaskChildPlanner model refusals": a chain with no connected
//! provider ends in a TYPED failure that names each model and the fix; nothing is substituted and
//! no child is spawned.
use maho_omo_task::planner::create_task_child_planner;
use senpi_task::{
    agents::{AgentDefinition, builtin_agent},
    host::{HostError, SenpiModelRegistry},
    manager::types::{ManagerStartSpec, PlanResolutionCode, PlanResolutionError},
};
use serde_json::{Value, json};
use std::sync::Arc;

struct ProxyOnly;
impl SenpiModelRegistry for ProxyOnly {
    fn get_available(&self) -> Result<Value, HostError> {
        Ok(json!([{"provider":"proxy-router","id":"gpt-6-astra"}]))
    }
    fn find(&self, provider: &str, id: &str) -> Option<Value> {
        (provider == "proxy-router" && id == "gpt-6-astra")
            .then(|| json!({"provider":"proxy-router","id":"gpt-6-astra"}))
    }
}

fn refuse(
    config: Value,
    agents: Vec<(String, AgentDefinition)>,
    spec: ManagerStartSpec,
) -> PlanResolutionError {
    let registry: Arc<dyn SenpiModelRegistry> = Arc::new(ProxyOnly);
    let planner =
        create_task_child_planner(config, agents, Arc::new(move || Some(registry.clone())));
    *planner(&ManagerStartSpec {
        prompt: "Find the auth flow.".into(),
        parent_session_id: "parent-1".into(),
        ..spec
    })
    .expect_err("refusal")
}

fn explore(definition: impl FnOnce(&mut AgentDefinition)) -> Vec<(String, AgentDefinition)> {
    let mut explore = builtin_agent("explore")
        .expect("explore must ship as a builtin agent")
        .clone();
    definition(&mut explore);
    vec![("explore".into(), explore)]
}

fn category(name: &str) -> ManagerStartSpec {
    ManagerStartSpec {
        category: Some(name.into()),
        ..ManagerStartSpec::default()
    }
}

fn agent(name: &str) -> ManagerStartSpec {
    ManagerStartSpec {
        subagent_type: Some(name.into()),
        ..ManagerStartSpec::default()
    }
}

#[test]
fn a_dead_category_chain_carries_the_chain_and_the_settings_fix() {
    let error = refuse(json!({}), vec![], category("quick"));
    assert_eq!(error.code, PlanResolutionCode::ModelUnavailable);
    assert!(
        !error
            .attempted_chain
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(
        error.message.contains("categories.quick.model"),
        "{}",
        error.message
    );
    assert!(error.message.contains("No child was spawned"));
}

#[test]
fn a_dead_explore_chain_carries_the_agent_chain_and_agents_explore_model() {
    let error = refuse(json!({}), explore(|_| {}), agent("explore"));
    assert_eq!(error.code, PlanResolutionCode::ModelUnavailable);
    let chain = error.attempted_chain.as_deref().unwrap_or_default();
    assert!(!chain.is_empty());
    // The chain branch must name a rung the chain actually carries: the pin text also names
    // agents.explore.model and the no-spawn fact.
    assert!(error.message.contains(&chain[0].model), "{}", error.message);
    assert!(error.message.starts_with("Agent \"explore\""));
    assert!(error.message.contains("agents.explore.model"));
    assert!(error.message.contains("No child was spawned"));
    assert!(
        !error
            .missing_providers
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    assert_eq!(error.available_agents, Some(vec!["explore".to_owned()]));
}

#[test]
fn explore_pinned_to_an_unserved_model_names_the_pin_not_its_chain() {
    let agents = explore(|explore| explore.model = Some("openai/gpt-5.5".into()));
    let error = refuse(json!({}), agents, agent("explore"));
    assert_eq!(error.code, PlanResolutionCode::ModelUnavailable);
    assert!(
        error.message.contains("openai/gpt-5.5"),
        "{}",
        error.message
    );
    assert!(error.message.contains("agents.explore.model"));
    assert!(!error.message.contains("Chain, in order:"));
}

#[test]
fn a_category_pinned_to_an_unserved_model_names_the_pin_and_the_settings_key() {
    let config = json!({"categories":{"quick":{"model":"openai/gpt-5.5"}}});
    let error = refuse(config, vec![], category("quick"));
    assert_eq!(error.code, PlanResolutionCode::ModelUnavailable);
    assert!(
        error.message.contains("openai/gpt-5.5"),
        "{}",
        error.message
    );
    assert!(error.message.contains("categories.quick.model"));
}
