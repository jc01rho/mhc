//! omo v5.1.79 `category/user-candidates.ts`: a category config's declared model chain.

use serde_json::Value;

use crate::model_chain::ModelChainCandidate;

fn string_field(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(Value::as_str).map(str::to_string)
}

fn first_string(values: &[Option<&Value>]) -> Option<String> {
    values
        .iter()
        .find_map(|value| value.and_then(Value::as_str))
        .map(str::to_string)
}

pub(crate) fn canonical_models(config: &Value) -> Option<&Vec<Value>> {
    config
        .get("models")
        .and_then(Value::as_array)
        .filter(|models| !models.is_empty())
}

/// `fallback_models` as `model[ variant]` strings, the shape delegate-core reads.
pub(crate) fn flatten_fallback_models(fallback_models: Option<&Value>) -> Option<Vec<String>> {
    match fallback_models? {
        Value::String(model) => Some(vec![model.clone()]),
        Value::Array(entries) => Some(
            entries
                .iter()
                .filter_map(|entry| match entry {
                    Value::String(model) => Some(model.clone()),
                    Value::Object(object) => {
                        let model = object.get("model")?.as_str()?;
                        Some(
                            match object
                                .get("variant")
                                .and_then(Value::as_str)
                                .filter(|variant| !variant.is_empty())
                            {
                                Some(variant) => format!("{model} {variant}"),
                                None => model.to_string(),
                            },
                        )
                    }
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Canonical `models[]` wins over the legacy `model` + `fallback_models` branch: entry zero is the
/// primary model and the rest become the ordered runtime fallback chain.
pub(crate) fn category_model_candidates(config: &Value) -> Vec<ModelChainCandidate> {
    if let Some(models) = canonical_models(config) {
        return models
            .iter()
            .filter_map(|entry| match entry {
                Value::String(model) => Some(ModelChainCandidate::of(model)),
                Value::Object(object) => Some(ModelChainCandidate {
                    model: object.get("model")?.as_str()?.to_string(),
                    variant: string_field(entry, "variant"),
                    reasoning_effort: string_field(entry, "reasoning"),
                }),
                _ => None,
            })
            .collect();
    }
    let config_variant = config.get("variant");
    let config_reasoning = config.get("reasoning");
    let config_effort = config.get("reasoningEffort");
    let mut candidates: Vec<ModelChainCandidate> = string_field(config, "model")
        .map(|model| ModelChainCandidate {
            model,
            variant: first_string(&[config_variant]),
            reasoning_effort: first_string(&[config_reasoning, config_effort]),
        })
        .into_iter()
        .collect();
    let entries = match config.get("fallback_models") {
        Some(Value::String(model)) => vec![Value::String(model.clone())],
        Some(Value::Array(entries)) => entries.clone(),
        _ => return candidates,
    };
    candidates.extend(entries.iter().filter_map(|entry| match entry {
        Value::String(model) => Some(ModelChainCandidate {
            model: model.clone(),
            variant: first_string(&[config_variant]),
            reasoning_effort: first_string(&[config_effort]),
        }),
        Value::Object(_) => Some(ModelChainCandidate {
            model: string_field(entry, "model")?,
            variant: first_string(&[entry.get("variant"), config_variant]),
            reasoning_effort: first_string(&[
                entry.get("reasoning"),
                config_reasoning,
                entry.get("reasoningEffort"),
                config_effort,
            ]),
        }),
        _ => None,
    }));
    candidates
}
