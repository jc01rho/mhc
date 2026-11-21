//! Port of `components/task/model-routing-rows.ts` (omo #14025): the rows of
//! `omo.modelRouting.resolve`, one per category and task agent, each the model a spawn of that lane
//! runs first as the spawn resolvers pick it.

use std::collections::{BTreeSet, HashSet};

use senpi_task::{
    agents::{AgentDefinition, AgentResolutionResult, ResolveAgentOptions, resolve_agent},
    category::{
        BUILTIN_CATEGORY_DEFAULTS, CategoryResolutionResult, ModelSelectionSource,
        ResolveCategoryOptions, resolve_category,
    },
    host::SenpiModelRegistry,
    manager::types::ResolvedChildPlan,
    model_exclusion::is_excluded_model,
};
use serde::Serialize;
use serde_json::Value;

use crate::fast_mode_inheritance::inherit_parent_fast_mode;
use crate::planner::exclude_models;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRoutingKind {
    Category,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRoutingSource {
    Override,
    ProviderFallback,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelRoutingRow {
    pub kind: ModelRoutingKind,
    pub name: String,
    pub effective: Option<String>,
    pub source: ModelRoutingSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// What the planner resolves a spawn against besides the config: its agent table and the
/// parent's service tier.
#[derive(Debug, Clone, Default)]
pub struct ModelRoutingPlannerInputs {
    pub agents: Vec<(String, AgentDefinition)>,
    pub parent_service_tier: Option<String>,
}

fn routed(
    kind: ModelRoutingKind,
    name: String,
    effective: String,
    source: ModelSelectionSource,
    variant: Option<String>,
) -> ModelRoutingRow {
    ModelRoutingRow {
        kind,
        name,
        effective: Some(effective),
        source: match source {
            ModelSelectionSource::User => ModelRoutingSource::Override,
            ModelSelectionSource::Builtin => ModelRoutingSource::ProviderFallback,
        },
        variant,
    }
}

fn unresolved(kind: ModelRoutingKind, name: String) -> ModelRoutingRow {
    ModelRoutingRow {
        kind,
        name,
        effective: None,
        source: ModelRoutingSource::Unresolved,
        variant: None,
    }
}

fn resolution_category(result: &CategoryResolutionResult) -> &str {
    match result {
        CategoryResolutionResult::Resolved { category, .. }
        | CategoryResolutionResult::Disabled { category, .. }
        | CategoryResolutionResult::NotFound { category, .. } => category,
        CategoryResolutionResult::ModelUnavailable(unavailable) => &unavailable.category,
    }
}

/// Every builtin and configured category, then every task agent, resolved by `resolve_category` /
/// `resolve_agent` exactly as the planner resolves a spawn, then the parent fast-tier step. No
/// distributor is consulted, so a read never advances a rotation: a `round_robin` lane shows its
/// first entry.
pub fn resolve_model_routing_rows(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
    planner: &ModelRoutingPlannerInputs,
) -> Vec<ModelRoutingRow> {
    let excluded = exclude_models(config);
    let effective_model = |model: String| {
        let fast = inherit_parent_fast_mode(
            ResolvedChildPlan {
                model: model.clone(),
                ..ResolvedChildPlan::default()
            },
            Some(registry),
            planner.parent_service_tier.as_deref(),
        )
        .model;
        // The -fast sibling is a different model: exclude_models keeps the planned one, as the planner does.
        if is_excluded_model(&fast, &excluded) {
            model
        } else {
            fast
        }
    };
    let category_names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_string())
        .chain(
            config
                .get("categories")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|categories| categories.keys().cloned()),
        )
        .collect();
    let mut rows = Vec::new();
    let mut seen_categories = HashSet::new();
    for name in &category_names {
        let row = match resolve_category(name, config, registry, &ResolveCategoryOptions::default())
        {
            Ok(CategoryResolutionResult::Resolved {
                category,
                spec,
                model_selection,
                ..
            }) => {
                let variant = spec
                    .reasoning
                    .clone()
                    .or_else(|| spec.reasoning_effort.clone())
                    .or_else(|| spec.variant.clone());
                routed(
                    ModelRoutingKind::Category,
                    category,
                    effective_model(format!("{}/{}", spec.provider, spec.model_id)),
                    model_selection.source,
                    variant,
                )
            }
            Ok(other) => unresolved(
                ModelRoutingKind::Category,
                resolution_category(&other).to_string(),
            ),
            Err(_) => unresolved(ModelRoutingKind::Category, name.clone()),
        };
        if seen_categories.insert(row.name.clone()) {
            rows.push(row);
        }
    }
    let mut agent_names: Vec<&String> = planner.agents.iter().map(|(name, _)| name).collect();
    agent_names.sort();
    let mut seen_agents = HashSet::new();
    for name in agent_names {
        let row = match resolve_agent(
            name,
            &planner.agents,
            Some(registry),
            &ResolveAgentOptions {
                omo_config: Some(config.clone()),
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
                routed(
                    ModelRoutingKind::Agent,
                    resolution.agent.clone(),
                    effective_model(resolution.model.clone()),
                    resolution.model_source,
                    variant,
                )
            }
            AgentResolutionResult::ModelUnavailable { agent, .. }
            | AgentResolutionResult::NotFound { agent, .. } => {
                unresolved(ModelRoutingKind::Agent, agent)
            }
        };
        if seen_agents.insert(row.name.clone()) {
            rows.push(row);
        }
    }
    rows
}
