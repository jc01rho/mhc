//! omo v5.1.79 `category/available-names.ts`: the category names a config and registry offer.

use std::collections::BTreeSet;

use serde_json::Value;

use super::builtins::{
    BUILTIN_CATEGORY_DEFAULTS, is_category_chain_viable, is_category_gate_satisfied,
};
use crate::host::{SenpiModelRegistry, parse_available_models};
use crate::model_exclusion::without_config_excluded_models;

pub(crate) fn available_category_names(
    config: &Value,
    available_models: Option<&BTreeSet<String>>,
) -> Vec<String> {
    let user = config.get("categories").and_then(Value::as_object);
    let names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_string())
        .chain(user.into_iter().flat_map(|user| user.keys().cloned()))
        .collect();
    let Some(available_models) = available_models else {
        return names.into_iter().collect();
    };
    names
        .into_iter()
        .filter(|name| {
            let explicit = user.is_some_and(|user| user.contains_key(name));
            is_category_gate_satisfied(name, explicit, available_models)
                && is_category_chain_viable(name, explicit, available_models)
        })
        .collect()
}

/// Gated listing for the disabled/not_found early returns. The registry is only consulted best
/// effort here: a failing or malformed registry degrades to the ungated list, since those results
/// never resolve a model anyway.
pub(crate) fn gated_available_categories(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Vec<String> {
    let registry = without_config_excluded_models(registry, config);
    let parsed = registry
        .get_available()
        .ok()
        .and_then(|available| parse_available_models(&available));
    match parsed {
        Some(models) => {
            available_category_names(config, Some(&models.into_iter().collect::<BTreeSet<_>>()))
        }
        None => available_category_names(config, None),
    }
}

pub fn resolve_available_category_names(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Vec<String> {
    gated_available_categories(config, registry)
}
