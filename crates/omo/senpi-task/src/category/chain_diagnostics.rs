//! omo v5.1.79 `category/chain-diagnostics.ts`: what a dead builtin chain tells the user.

use std::collections::{BTreeSet, HashSet};

use super::fallback_chains::category_fallback_chain;
use crate::delegate_adapter::{
    DelegateFallbackEntry, DelegateModelResolutionInput, DelegateModelResolutionResult,
    resolve_model_for_delegate_task, warm_cache_deps,
};
use crate::host::{HostError, SenpiModelRegistry, parse_available_models};
use crate::model_chain::{ModelChainCandidate, available_chain_candidates};
use crate::model_exclusion::without_excluded_models;

fn provider_of(model: &str) -> &str {
    model
        .split_once('/')
        .map_or(model, |(provider, _)| provider)
}

/// Chain providers with no model in the live registry, in chain order, deduplicated.
pub(crate) fn missing_chain_providers(
    chain: &[DelegateFallbackEntry],
    available_models: &[String],
) -> Vec<String> {
    let connected: HashSet<&str> = available_models
        .iter()
        .map(|model| provider_of(model))
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for provider in chain.iter().flat_map(|rung| &rung.providers) {
        if !connected.contains(provider.as_str()) && !missing.contains(provider) {
            missing.push(provider.clone());
        }
    }
    missing
}

/// The first rung model, in chain order, that only an unlisted provider (a gateway or custom proxy)
/// serves. Builtin resolution never selects it; it is the exact pin a user may opt into (#9146).
pub(crate) fn unlisted_provider_model(
    chain: &[DelegateFallbackEntry],
    available_models: &[String],
) -> Option<String> {
    chain.iter().find_map(|rung| {
        match resolve_model_for_delegate_task(
            &DelegateModelResolutionInput {
                fallback_chain: Some(vec![rung.clone()]),
                allow_unlisted_providers: true,
                available_models: available_models.iter().cloned().collect(),
                ..Default::default()
            },
            &warm_cache_deps(),
        ) {
            Some(DelegateModelResolutionResult::Resolved(resolution)) => Some(resolution.model),
            _ => None,
        }
    })
}

/// The builtin fallback chain of a category, resolved against the models the registry reports as
/// available, in chain order. `resolve_category` leaves a user-forced model's chain untouched; a
/// background child that must not die with its one refused pin appends these rungs itself.
pub fn builtin_category_chain_candidates(
    category_name: &str,
    registry: &dyn SenpiModelRegistry,
) -> Result<Vec<ModelChainCandidate>, HostError> {
    let Some(chain) =
        category_fallback_chain(&omo_config_core::canonical_category_name(category_name))
    else {
        return Ok(Vec::new());
    };
    let available: BTreeSet<String> =
        parse_available_models(&without_excluded_models(registry, None).get_available()?)
            .unwrap_or_default()
            .into_iter()
            .collect();
    Ok(available_chain_candidates(chain, &available))
}
