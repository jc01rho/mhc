//! Upstream `components/task/model-error-text.ts`: the spawn-time refusal text for a target whose
//! model chain or pin this machine cannot serve.
use senpi_task::{
    DelegateFallbackEntry, agents::AgentDefinition, manager::types::PlanResolutionError,
};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainFailureTarget<'a> {
    Category(&'a str),
    Agent(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinKey {
    Model,
    Models,
}

fn target_kind(target: ChainFailureTarget) -> &'static str {
    match target {
        ChainFailureTarget::Category(_) => "Category",
        ChainFailureTarget::Agent(_) => "Agent",
    }
}

fn target_name(target: ChainFailureTarget<'_>) -> &str {
    match target {
        ChainFailureTarget::Category(name) | ChainFailureTarget::Agent(name) => name,
    }
}

// The Settings editor writes exactly these keys (categories.<name>.model / agents.<name>.model), so
// the refusal names the key the reader can change instead of a generic "configure omo.json".
fn chain_fix_key(target: ChainFailureTarget) -> String {
    match target {
        ChainFailureTarget::Category(name) => format!("categories.{name}.model"),
        ChainFailureTarget::Agent(name) => format!("agents.{name}.model"),
    }
}

// A pin can also live in the models list the schema accepts, and that list is a different key: naming
// `.model` would send the reader to a key they never wrote.
fn pin_fix_key(target: ChainFailureTarget, pin_key: PinKey) -> String {
    match (pin_key, target) {
        (PinKey::Model, _) => chain_fix_key(target),
        (PinKey::Models, ChainFailureTarget::Category(name)) => format!("categories.{name}.models"),
        (PinKey::Models, ChainFailureTarget::Agent(name)) => format!("agents.{name}.models"),
    }
}

// One entry per chain rung: the model and the providers that can serve it. The reader acts on the
// first entry they recognize, which a single "attempted <id>" never allowed.
fn chain_entries_text(chain: &[DelegateFallbackEntry]) -> String {
    chain
        .iter()
        .map(|rung| format!("{} ({})", rung.model, rung.providers.join(" or ")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unlisted_opt_in_text(unlisted_model: Option<&str>, target: ChainFailureTarget) -> String {
    let Some(unlisted_model) = unlisted_model else {
        return String::new();
    };
    let Some((gateway, model)) = unlisted_model
        .split_once('/')
        .filter(|(g, _)| !g.is_empty())
    else {
        return String::new();
    };
    format!(
        " Only {gateway} serves {model}, and omo never bills a builtin {} to an unlisted provider; set {} = \"{unlisted_model}\" to opt in.",
        target_kind(target).to_lowercase(),
        chain_fix_key(target)
    )
}

/// The spawn-time refusal for a target whose whole chain needs providers this machine has not
/// connected. It names EVERY model in the chain, the providers each one needs, and both fixes:
/// connect a provider, or point the target at a model the machine already runs. Nothing is
/// substituted and no child is spawned.
pub fn chain_unavailable_text(
    target: ChainFailureTarget,
    chain: &[DelegateFallbackEntry],
    missing_providers: &[String],
    unlisted_model: Option<&str>,
) -> String {
    let models = if chain.is_empty() {
        "none".to_owned()
    } else {
        chain_entries_text(chain)
    };
    let reason = if missing_providers.is_empty() {
        "its chain's providers are connected but none of them lists these model ids".to_owned()
    } else {
        format!(
            "its chain needs providers this machine has not connected ({})",
            missing_providers.join(", ")
        )
    };
    let fix = if missing_providers.is_empty() {
        format!(
            "set {} in Settings to a model you already run",
            chain_fix_key(target)
        )
    } else {
        format!(
            "connect one of those providers, or set {} in Settings to a model you already run",
            chain_fix_key(target)
        )
    };
    format!(
        "{} \"{}\" has no usable model: {reason}. Chain, in order: {models}.{} Fix: {fix}. No child was spawned.",
        target_kind(target),
        target_name(target),
        unlisted_opt_in_text(unlisted_model, target)
    )
}

/// The refusal for a target the user pinned to a model the live registry does not serve. The pin is
/// a choice, so it is never replaced; the message names it and the two ways out, so the parent stops
/// retrying the same broken binding.
pub fn pinned_model_unavailable_text(
    target: ChainFailureTarget,
    pinned_model: &str,
    pin_key: PinKey,
) -> String {
    format!(
        "{} \"{}\" is pinned to \"{pinned_model}\", which this machine's model registry does not serve: that provider is not connected, or it does not list that model id. Fix: connect the provider, or re-pick {} in Settings. No child was spawned.",
        target_kind(target),
        target_name(target),
        pin_fix_key(target, pin_key)
    )
}

/// Upstream `AgentUnavailable`: the agent resolver's `model_unavailable` fields the refusal reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentUnavailable<'a> {
    pub agent: Option<&'a str>,
    pub attempted_model: Option<&'a str>,
    pub attempted_chain: Option<&'a [DelegateFallbackEntry]>,
    pub missing_providers: Option<&'a [String]>,
}

// Which field held the pin matters to the reader: the fix names that field, not one they never wrote.
fn first_configured_pin(config: Option<&Value>) -> Option<(String, PinKey)> {
    let config = config?;
    if let Some(model) = config.get("model").and_then(Value::as_str) {
        return Some((model.to_owned(), PinKey::Model));
    }
    let entry = config.get("models")?.as_array()?.first()?;
    let model = entry
        .as_str()
        .or_else(|| entry.get("model").and_then(Value::as_str))?;
    Some((model.to_owned(), PinKey::Models))
}

fn first_agent_pin(definition: Option<&AgentDefinition>) -> Option<(String, PinKey)> {
    let definition = definition?;
    if let Some(model) = &definition.model {
        return Some((model.clone(), PinKey::Model));
    }
    let entry = definition.models.as_ref()?.first()?;
    Some((entry.model().to_owned(), PinKey::Models))
}

/// A category refusal: a dead chain names every model and the fix, a pin the registry cannot serve
/// names the pin. Any other `model_unavailable` keeps the resolver's own text (`None`).
pub fn category_model_failure_text(
    error: &PlanResolutionError,
    omo_config: &Value,
    canonical_category: Option<&str>,
) -> Option<String> {
    // The resolver canonicalizes a retired alias; the Settings key and the pin live under the
    // canonical name, so the refusal must name that one.
    let category = canonical_category.or(error.category.as_deref())?;
    if let Some(chain) = &error.attempted_chain {
        return Some(chain_unavailable_text(
            ChainFailureTarget::Category(category),
            chain,
            error.missing_providers.as_deref().unwrap_or_default(),
            error.unlisted_provider_model.as_deref(),
        ));
    }
    // A pin can be written either way the schema allows: a scalar model or a models list.
    let (model, key) = first_configured_pin(
        omo_config
            .get("categories")
            .and_then(|categories| categories.get(category)),
    )?;
    Some(pinned_model_unavailable_text(
        ChainFailureTarget::Category(category),
        &model,
        key,
    ))
}

pub fn agent_model_failure_text(
    agent_name: &str,
    resolution: AgentUnavailable,
    agents: &[(String, AgentDefinition)],
) -> String {
    // The resolver canonicalizes a retired alias, so the pin and the Settings key live under that
    // name; keying the lookup on the requested alias missed the pin entirely.
    let agent = resolution.agent.unwrap_or(agent_name);
    let definition = agents
        .iter()
        .find(|(name, _)| name == agent)
        .map(|(_, definition)| definition);
    // A pin is the cause of the failure, so it wins over the chain that the pin replaced: refusing
    // with the chain text told a pinned agent's caller to set the key it had already set.
    if let Some((model, key)) = first_agent_pin(definition) {
        return pinned_model_unavailable_text(ChainFailureTarget::Agent(agent), &model, key);
    }
    if let Some(chain) = resolution.attempted_chain {
        return chain_unavailable_text(
            ChainFailureTarget::Agent(agent),
            chain,
            resolution.missing_providers.unwrap_or_default(),
            None,
        );
    }
    format!(
        "No available model for agent \"{agent}\" (attempted {}).",
        resolution.attempted_model.unwrap_or("none")
    )
}
