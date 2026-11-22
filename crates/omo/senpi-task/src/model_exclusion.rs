//! `exclude_models` registry view (`model-exclusion.ts`).
//!
//! Also the crate's home for the `excluded-models.ts` matcher: `withoutExcludedModels` is the only
//! thing that needs it, and both the category and agent resolvers walk registries through it.

use maho_ai::model_execution_gate::{
    blocked_model_message, fable_blocked, fable_blocked_in, is_execution_blocked,
};
use serde_json::Value;

use crate::host::{HostError, SenpiModelRegistry};

/// `provider/model-id` of one registry entry (`model-exclusion.ts` `modelKey`); `None` when the entry
/// is not an object carrying both string fields.
fn registry_model_key(model: &Value) -> Option<String> {
    let object = model.as_object()?;
    let provider = object.get("provider")?.as_str()?;
    let id = object.get("id")?.as_str()?;
    Some(format!("{provider}/{id}"))
}

/// `excluded-models.ts` `patternExpression`: `*` matches any run of characters (including `/` and
/// none at all) and every other character is literal; matching ignores case.
fn wildcard_pattern_matches(pattern: &str, subject: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let subject: Vec<char> = subject.to_lowercase().chars().collect();
    let (mut p, mut s) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while s < subject.len() {
        if p < pattern.len() && pattern[p] != '*' && pattern[p] == subject[s] {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            mark = s;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            mark += 1;
            s = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// `excluded-models.ts` `selectorTails`: every tail starting after a `/` is a model id some provider
/// serves, so an entry is tested against each of them.
fn selector_tails(model: &str) -> Vec<&str> {
    let tails: Vec<&str> = model
        .match_indices('/')
        .map(|(index, _)| &model[index + 1..])
        .collect();
    if tails.is_empty() { vec![model] } else { tails }
}

/// The entry that keeps `model` out, or `None` (`excluded-models.ts` `excludedModelPattern`).
pub fn excluded_model_pattern<'a>(model: &str, patterns: &'a [String]) -> Option<&'a str> {
    let tails = selector_tails(model);
    for raw in patterns {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        let matched = if pattern.contains('/') {
            wildcard_pattern_matches(pattern, model)
                || tails
                    .iter()
                    .any(|tail| wildcard_pattern_matches(pattern, tail))
        } else {
            tails
                .iter()
                .any(|tail| wildcard_pattern_matches(pattern, tail))
        };
        if matched {
            return Some(raw.as_str());
        }
    }
    None
}

/// `excluded-models.ts` `isExcludedModel`: whether any pattern keeps `model` out of a registry view.
pub fn is_excluded_model(model: &str, patterns: &[String]) -> bool {
    excluded_model_pattern(model, patterns).is_some()
}

/// The registry as `exclude_models` leaves it (`model-exclusion.ts` `withoutExcludedModels`): an
/// excluded model is neither available nor found, so every chain walk skips it exactly as it skips a
/// model whose provider has no login. A Claude Fable model is kept out the same way while the
/// execution gate (`maho_ai::model_execution_gate`) is on.
pub struct ExcludedModelsRegistry<'a> {
    inner: &'a dyn SenpiModelRegistry,
    patterns: Vec<String>,
    block_fable: bool,
}

impl ExcludedModelsRegistry<'_> {
    fn keeps_out(&self, model: &str) -> bool {
        is_excluded_model(model, &self.patterns) || is_execution_blocked(model, self.block_fable)
    }
}

impl SenpiModelRegistry for ExcludedModelsRegistry<'_> {
    fn get_available(&self) -> Result<Value, HostError> {
        let available = self.inner.get_available()?;
        let Value::Array(models) = available else {
            return Ok(available);
        };
        Ok(Value::Array(
            models
                .into_iter()
                .filter(|model| match registry_model_key(model) {
                    None => true,
                    Some(key) => !self.keeps_out(&key),
                })
                .collect(),
        ))
    }

    fn find(&self, provider: &str, model_id: &str) -> Option<Value> {
        if self.keeps_out(&format!("{provider}/{model_id}")) {
            return None;
        }
        self.inner.find(provider, model_id)
    }
}

/// `withoutExcludedModels`: the registry with `patterns` applied, gated by the process-wide setting.
pub fn without_excluded_models<'a>(
    registry: &'a dyn SenpiModelRegistry,
    patterns: Option<&[String]>,
) -> ExcludedModelsRegistry<'a> {
    without_kept_out_models(
        registry,
        patterns.map(<[String]>::to_vec).unwrap_or_default(),
        fable_blocked(),
    )
}

/// The registry an omo config view leaves: its `exclude_models` and its execution gate setting.
pub fn without_config_excluded_models<'a>(
    registry: &'a dyn SenpiModelRegistry,
    config: &Value,
) -> ExcludedModelsRegistry<'a> {
    without_kept_out_models(
        registry,
        crate::category::exclude_model_patterns(config).unwrap_or_default(),
        fable_blocked_in(config),
    )
}

/// The refusal for running `model` (`provider/id`) under an omo config view's execution gate.
pub fn blocked_model_refusal(model: &str, config: &Value) -> Option<String> {
    is_execution_blocked(model, fable_blocked_in(config)).then(|| blocked_model_message(model))
}

pub fn without_kept_out_models(
    registry: &dyn SenpiModelRegistry,
    patterns: Vec<String>,
    block_fable: bool,
) -> ExcludedModelsRegistry<'_> {
    ExcludedModelsRegistry {
        inner: registry,
        patterns,
        block_fable,
    }
}
