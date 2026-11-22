//! omo v5.1.79 `category/coverage.ts`: which task categories a registry can serve.

use std::collections::BTreeSet;

use serde_json::Value;

use super::available_names::resolve_available_category_names;
use super::builtins::BUILTIN_CATEGORY_DEFAULTS;
use super::chain_diagnostics::{missing_chain_providers, unlisted_provider_model};
use super::fallback_chains::category_fallback_chain;
use crate::host::{HostError, SenpiModelRegistry, parse_available_models};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusableCategory {
    pub name: String,
    /// The chain providers with no model in the registry, in chain order: the ones a login fixes.
    pub providers: Vec<String>,
    /// The `provider/id` an unlisted gateway or proxy serves for this category: the opt-in pin.
    pub unlisted_provider_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryCoverage {
    pub usable: Vec<String>,
    pub unusable: Vec<UnusableCategory>,
}

/// Which task categories this registry can serve, by the same gate the spawn path applies, and for
/// each one it cannot, the chain providers the category-unavailable notice would name. A disabled
/// category is neither usable nor a gap. A registry whose model list is not an array is an error:
/// the gated resolver would silently list every category there, which is not a coverage answer.
pub fn resolve_category_coverage(
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Result<CategoryCoverage, HostError> {
    let Some(available) = parse_available_models(&registry.get_available()?) else {
        return Err(HostError {
            message: "the model registry did not return a model list".to_string(),
        });
    };
    let user = config.get("categories").and_then(Value::as_object);
    let enabled = |name: &str| {
        user.and_then(|user| user.get(name))
            .and_then(|category| category.get("disable"))
            != Some(&Value::Bool(true))
    };
    let usable: BTreeSet<String> = resolve_available_category_names(config, registry)
        .into_iter()
        .collect();
    let names: BTreeSet<String> = BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_string())
        .chain(user.into_iter().flat_map(|user| user.keys().cloned()))
        .collect();
    let mut coverage = CategoryCoverage::default();
    for name in names {
        if !enabled(name.as_str()) {
            continue;
        }
        if usable.contains(&name) {
            coverage.usable.push(name);
            continue;
        }
        let chain = category_fallback_chain(&name).unwrap_or_default();
        coverage.unusable.push(UnusableCategory {
            providers: missing_chain_providers(chain, &available),
            unlisted_provider_model: unlisted_provider_model(chain, &available),
            name,
        });
    }
    Ok(coverage)
}
