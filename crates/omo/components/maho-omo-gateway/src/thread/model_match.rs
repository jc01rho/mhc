//! `thread_set_model`'s one matching rule.
//!
//! Upstream counterpart: `thread/tools/model-match.ts`.

use serde_json::json;

use super::errors::{ThreadToolFailure, thread_tool_failure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub id: String,
    pub name: Option<String>,
}

impl ModelChoice {
    fn qualified(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// What a catalog answers for a pattern: the one model, or the failure the tool answers.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelMatch {
    Match(ModelChoice),
    Failure(ThreadToolFailure),
}

/// An empty model pattern is refused before any catalog is read, on this device or another.
pub fn model_pattern_failure(model: &str) -> Option<ThreadToolFailure> {
    model.trim().is_empty().then(|| {
        thread_tool_failure(
            "invalid_arguments",
            "The model pattern is empty.",
            "Pass a model id or display-name fragment.",
            None,
        )
    })
}

fn same_provider(choice: &ModelChoice, provider: Option<&str>) -> bool {
    provider.is_none_or(|provider| choice.provider.to_lowercase() == provider.trim().to_lowercase())
}

/// The one model a catalog's exact `provider/id` names (the first rung, within `provider` when given): a match no later
/// catalog change can make ambiguous, so a held catalog may decide it; the engine's set_model still confirms it.
pub fn exact_model(
    catalog: &[ModelChoice],
    model: &str,
    provider: Option<&str>,
) -> Option<ModelChoice> {
    let pattern = model.trim().to_lowercase();
    let named: Vec<&ModelChoice> = catalog
        .iter()
        .filter(|choice| {
            choice.qualified().to_lowercase() == pattern && same_provider(choice, provider)
        })
        .collect();
    match named.as_slice() {
        [only] => Some((*only).clone()),
        _ => None,
    }
}

/// Exact provider/id, then exact id, then a case-insensitive fragment of the id or display name, within `provider` when
/// given; none or several is a failure.
pub fn match_model(catalog: &[ModelChoice], model: &str, provider: Option<&str>) -> ModelMatch {
    let pattern = model.trim().to_lowercase();
    let available: Vec<&ModelChoice> = catalog
        .iter()
        .filter(|choice| same_provider(choice, provider))
        .collect();
    let mut matches: Vec<&ModelChoice> = available
        .iter()
        .copied()
        .filter(|choice| choice.qualified().to_lowercase() == pattern)
        .collect();
    if matches.is_empty() {
        matches = available
            .iter()
            .copied()
            .filter(|choice| choice.id.to_lowercase() == pattern)
            .collect();
    }
    if matches.is_empty() {
        matches = available
            .iter()
            .copied()
            .filter(|choice| {
                choice.id.to_lowercase().contains(&pattern)
                    || choice
                        .name
                        .as_ref()
                        .is_some_and(|name| name.to_lowercase().contains(&pattern))
            })
            .collect();
    }
    match matches.as_slice() {
        [] => ModelMatch::Failure(thread_tool_failure(
            "model_not_found",
            format!("No available model matches \"{model}\"."),
            "Choose a provider/id from the available list and retry.",
            Some(json!({
                "available": catalog.iter().take(20).map(ModelChoice::qualified).collect::<Vec<_>>(),
            })),
        )),
        [only] => ModelMatch::Match((*only).clone()),
        several => ModelMatch::Failure(thread_tool_failure(
            "model_ambiguous",
            format!("Several available models match \"{model}\"."),
            "Pass an exact provider/id or narrow the pattern with provider.",
            Some(json!({
                "candidates": several.iter().take(10).map(|choice| choice.qualified()).collect::<Vec<_>>(),
            })),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(provider: &str, id: &str, name: Option<&str>) -> ModelChoice {
        ModelChoice {
            provider: provider.into(),
            id: id.into(),
            name: name.map(str::to_owned),
        }
    }

    fn catalog() -> Vec<ModelChoice> {
        vec![
            choice("anthropic", "claude-opus", Some("Claude Opus")),
            choice("anthropic", "claude-haiku", Some("Claude Haiku")),
            choice("openai", "gpt-6", Some("GPT 6")),
            choice("azure", "gpt-6", Some("GPT 6 (Azure)")),
        ]
    }

    #[test]
    fn the_ladder_takes_provider_id_then_id_then_a_fragment() {
        assert_eq!(
            match_model(&catalog(), "OpenAI/GPT-6", None),
            ModelMatch::Match(choice("openai", "gpt-6", Some("GPT 6")))
        );
        assert_eq!(
            match_model(&catalog(), "claude-haiku", None),
            ModelMatch::Match(choice("anthropic", "claude-haiku", Some("Claude Haiku")))
        );
        assert_eq!(
            match_model(&catalog(), "opus", None),
            ModelMatch::Match(choice("anthropic", "claude-opus", Some("Claude Opus")))
        );
        assert_eq!(
            match_model(&catalog(), "gpt-6", Some("azure")),
            ModelMatch::Match(choice("azure", "gpt-6", Some("GPT 6 (Azure)")))
        );
    }

    #[test]
    fn none_or_several_matches_fail_with_the_lists_the_caller_needs() {
        let ModelMatch::Failure(missing) = match_model(&catalog(), "gemini", None) else {
            panic!("expected model_not_found");
        };
        assert_eq!(missing.code.as_str(), "model_not_found");
        assert_eq!(
            missing
                .details
                .as_ref()
                .map(|details| details["available"].clone()),
            Some(json!([
                "anthropic/claude-opus",
                "anthropic/claude-haiku",
                "openai/gpt-6",
                "azure/gpt-6"
            ]))
        );
        let ModelMatch::Failure(several) = match_model(&catalog(), "gpt-6", None) else {
            panic!("expected model_ambiguous");
        };
        assert_eq!(several.code.as_str(), "model_ambiguous");
        assert_eq!(
            several
                .details
                .as_ref()
                .map(|details| details["candidates"].clone()),
            Some(json!(["openai/gpt-6", "azure/gpt-6"]))
        );
    }

    #[test]
    fn only_an_exact_provider_id_is_decided_from_a_held_catalog() {
        assert_eq!(
            exact_model(&catalog(), "openai/gpt-6", None),
            Some(choice("openai", "gpt-6", Some("GPT 6")))
        );
        assert_eq!(exact_model(&catalog(), "gpt-6", None), None);
        assert!(model_pattern_failure("  ").is_some());
        assert!(model_pattern_failure("opus").is_none());
    }
}
