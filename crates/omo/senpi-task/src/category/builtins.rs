//! omo v5.1.79 `category/builtins.ts` with the category tables of `google-categories.ts`,
//! `openai-categories.ts`, `anthropic-categories.ts` and `kimi-categories.ts`.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::fallback_chains::category_fallback_chain;
use super::prompts;
use crate::delegate_adapter::{DelegateFallbackEntry, transform_model_for_provider};

/// The routing half of a builtin category's `omo.json` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinCategoryConfig {
    pub model: &'static str,
    pub variant: Option<&'static str>,
}

impl BuiltinCategoryConfig {
    /// The config as the `categories.<name>` JSON object it stands in for.
    pub fn to_value(self) -> Value {
        let mut object = Map::new();
        object.insert("model".into(), Value::from(self.model));
        if let Some(variant) = self.variant {
            object.insert("variant".into(), Value::from(variant));
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinCategoryDefinition {
    pub name: &'static str,
    pub config: BuiltinCategoryConfig,
    pub description: &'static str,
    /// Selection guidance for the caller, rendered under the category in the task tool description.
    pub caller_guidance: Option<&'static str>,
    pub prompt_append: &'static str,
    pub resolve_prompt_append: Option<fn(Option<&str>) -> &'static str>,
    /// Every model id that opens the category; one present id is enough. Empty means ungated.
    pub requires_model: &'static [&'static str],
    /// Builtin default and chain resolve on exact registry ids only (#14155).
    pub exact_models: bool,
}

const fn config(model: &'static str, variant: Option<&'static str>) -> BuiltinCategoryConfig {
    BuiltinCategoryConfig { model, variant }
}

fn model_name_of(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_lowercase()
}

fn is_gpt_5_5_or_5_6_model(model: &str) -> bool {
    let model_name = model_name_of(model);
    ["gpt-5.5", "gpt-5-5", "gpt-5.6", "gpt-5-6"]
        .iter()
        .any(|marker| model_name.contains(marker))
}

/// The GPT-6 family (gpt-6-astra, gpt-6.2-sol, gateway-prefixed ids) by the last path segment.
pub fn is_gpt6_model(model: &str) -> bool {
    model_name_of(model).contains("gpt-6")
}

/// Both deep lanes ship GPT rungs only, so the GPT append covers every model their chains can
/// select; the generic append is reached through a user model override.
fn is_gpt_deep_lane_model(model: Option<&str>) -> bool {
    model.is_some_and(|model| is_gpt6_model(model) || is_gpt_5_5_or_5_6_model(model))
}

pub fn resolve_ultrabrain_category_prompt_append(model: Option<&str>) -> &'static str {
    match model {
        Some(model) if is_gpt6_model(model) => prompts::ULTRABRAIN_PROMPT_APPEND_GPT_6_ASTRA,
        _ => prompts::ULTRABRAIN_PROMPT_APPEND,
    }
}

pub fn resolve_deep_low_category_prompt_append(model: Option<&str>) -> &'static str {
    if is_gpt_deep_lane_model(model) {
        prompts::DEEP_LOW_PROMPT_APPEND_GPT
    } else {
        prompts::DEEP_LOW_PROMPT_APPEND
    }
}

pub fn resolve_deep_high_category_prompt_append(model: Option<&str>) -> &'static str {
    if is_gpt_deep_lane_model(model) {
        prompts::DEEP_HIGH_PROMPT_APPEND_GPT
    } else {
        prompts::DEEP_HIGH_PROMPT_APPEND
    }
}

pub fn resolve_unspecified_high_category_prompt_append(model: Option<&str>) -> &'static str {
    match model {
        Some(model) if is_gpt6_model(model) => prompts::UNSPECIFIED_HIGH_PROMPT_APPEND_GPT_6_ASTRA,
        _ => prompts::UNSPECIFIED_HIGH_PROMPT_APPEND,
    }
}

// ultrabrain and both deep lanes are a single exact-model rung (#14155), so each gates on that one
// model and disappears without it; the gate also pins resolution to that exact id.
const GPT_ASTRA_GATE_MODELS: &[&str] = &["gpt-6.2-astra"];
const DEEP_LOW_GATE_MODELS: &[&str] = &["gpt-6.2-sol"];
const ARCHITECT_GATE_MODELS: &[&str] = &["claude-fable-5-2", "claude-fable-5-1"];

/// Declared in the TypeScript order: google, openai, anthropic, then kimi categories.
pub const BUILTIN_CATEGORY_DEFAULTS: [BuiltinCategoryDefinition; 10] = [
    BuiltinCategoryDefinition {
        name: "visual-engineering",
        config: config("google/gemini-4-argon", Some("low")),
        description: prompts::VISUAL_ENGINEERING_DESCRIPTION,
        caller_guidance: None,
        prompt_append: prompts::VISUAL_ENGINEERING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "artistry",
        config: config("anthropic/claude-fable-5-2", Some("max")),
        description: prompts::ARTISTRY_DESCRIPTION,
        caller_guidance: None,
        prompt_append: prompts::ARTISTRY_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "ultrabrain",
        config: config("chatgpt-subscription/gpt-6.2-astra", Some("max")),
        description: prompts::ULTRABRAIN_DESCRIPTION,
        caller_guidance: None,
        prompt_append: prompts::ULTRABRAIN_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_ultrabrain_category_prompt_append),
        requires_model: GPT_ASTRA_GATE_MODELS,
        exact_models: true,
    },
    BuiltinCategoryDefinition {
        name: "deep-low",
        config: config("chatgpt-subscription/gpt-6.2-sol", Some("medium")),
        description: prompts::DEEP_LOW_DESCRIPTION,
        caller_guidance: Some(prompts::DEEP_LOW_CALLER_GUIDANCE),
        prompt_append: prompts::DEEP_LOW_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_deep_low_category_prompt_append),
        requires_model: DEEP_LOW_GATE_MODELS,
        exact_models: true,
    },
    BuiltinCategoryDefinition {
        name: "deep-high",
        config: config("chatgpt-subscription/gpt-6.2-astra", Some("high")),
        description: prompts::DEEP_HIGH_DESCRIPTION,
        caller_guidance: Some(prompts::DEEP_HIGH_CALLER_GUIDANCE),
        prompt_append: prompts::DEEP_HIGH_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_deep_high_category_prompt_append),
        requires_model: GPT_ASTRA_GATE_MODELS,
        exact_models: true,
    },
    BuiltinCategoryDefinition {
        name: "quick",
        config: config("chatgpt-subscription/gpt-6-luna-fast", Some("low")),
        description: prompts::QUICK_DESCRIPTION,
        caller_guidance: Some(prompts::QUICK_CALLER_GUIDANCE),
        prompt_append: prompts::QUICK_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "unspecified-low",
        config: config("anthropic/claude-sonnet-5-5", Some("medium")),
        description: prompts::UNSPECIFIED_LOW_DESCRIPTION,
        caller_guidance: Some(prompts::UNSPECIFIED_LOW_CALLER_GUIDANCE),
        prompt_append: prompts::UNSPECIFIED_LOW_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "unspecified-high",
        config: config("anthropic/claude-opus-5-5", Some("medium")),
        description: prompts::UNSPECIFIED_HIGH_DESCRIPTION,
        caller_guidance: Some(prompts::UNSPECIFIED_HIGH_CALLER_GUIDANCE),
        prompt_append: prompts::UNSPECIFIED_HIGH_PROMPT_APPEND,
        resolve_prompt_append: Some(resolve_unspecified_high_category_prompt_append),
        requires_model: &[],
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "architect",
        config: config("anthropic/claude-fable-5-2", Some("xhigh")),
        description: prompts::ARCHITECT_DESCRIPTION,
        caller_guidance: None,
        prompt_append: prompts::ARCHITECT_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: ARCHITECT_GATE_MODELS,
        exact_models: false,
    },
    BuiltinCategoryDefinition {
        name: "writing",
        config: config("google/gemini-4-argon", Some("low")),
        description: prompts::WRITING_DESCRIPTION,
        caller_guidance: None,
        prompt_append: prompts::WRITING_PROMPT_APPEND,
        resolve_prompt_append: None,
        requires_model: &[],
        exact_models: false,
    },
];

pub fn builtin_category(name: &str) -> Option<&'static BuiltinCategoryDefinition> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .find(|definition| definition.name == name)
}

/// `BUILTIN_CATEGORY_REQUIRES_MODEL` as ordered `(category, gate models)` pairs.
pub fn builtin_category_requires_model() -> Vec<(&'static str, &'static [&'static str])> {
    BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .filter(|definition| !definition.requires_model.is_empty())
        .map(|definition| (definition.name, definition.requires_model))
        .collect()
}

/// `categoryGateModels`: the model ids that open a gated builtin category.
pub fn category_gate_models(category_name: &str) -> Option<&'static [&'static str]> {
    builtin_category(category_name)
        .map(|definition| definition.requires_model)
        .filter(|models| !models.is_empty())
}

/// `CATEGORY_CALLER_GUIDANCE[name]`.
pub fn category_caller_guidance(category_name: &str) -> Option<&'static str> {
    builtin_category(category_name).and_then(|definition| definition.caller_guidance)
}

fn category_chain_providers(category_name: &str) -> Vec<&'static str> {
    category_fallback_chain(category_name)
        .unwrap_or_default()
        .iter()
        .flat_map(|rung| rung.providers.iter().map(String::as_str))
        .collect()
}

/// `provider` serves `model` when the registry lists it under that provider, spelled as omo's
/// routing tables spell it or through the provider-specific id transform delegate-core applies at
/// resolution time (github-copilot: claude-fable-5.1, kimi-coding: k3). A gateway re-publishing the
/// model serves no builtin rung: a builtin chain resolves only on the providers it lists (#9146).
fn provider_serves_model(provider: &str, model: &str, available_models: &BTreeSet<String>) -> bool {
    available_models.contains(&format!("{provider}/{model}"))
        || available_models.contains(&format!(
            "{provider}/{}",
            transform_model_for_provider(provider, model)
        ))
}

/// `available_models` holds live registry models as `provider/id`. A gate opens only when a
/// provider this category's chain lists serves a gate model; any explicit `categories.<name>`
/// entry opts out.
pub fn is_category_gate_satisfied(
    category_name: &str,
    has_explicit_user_config: bool,
    available_models: &BTreeSet<String>,
) -> bool {
    let Some(gate_models) = category_gate_models(category_name) else {
        return true;
    };
    if has_explicit_user_config {
        return true;
    }
    let chain_providers = category_chain_providers(category_name);
    gate_models.iter().any(|gate_model| {
        chain_providers
            .iter()
            .any(|provider| provider_serves_model(provider, gate_model, available_models))
    })
}

pub(crate) type ExactServedRung = (DelegateFallbackEntry, DelegateFallbackEntry);

fn is_exact_model_category(category_name: &str) -> bool {
    builtin_category(category_name).is_some_and(|definition| definition.exact_models)
}

pub(crate) fn exact_served_rungs(
    category_name: &str,
    chain: &[DelegateFallbackEntry],
    available_models: &BTreeSet<String>,
) -> Option<Vec<ExactServedRung>> {
    if !is_exact_model_category(category_name) {
        return None;
    }
    Some(
        chain
            .iter()
            .filter_map(|rung| {
                let providers: Vec<String> = rung
                    .providers
                    .iter()
                    .filter(|provider| {
                        provider_serves_model(provider, &rung.model, available_models)
                    })
                    .cloned()
                    .collect();
                (!providers.is_empty()).then(|| {
                    (
                        DelegateFallbackEntry {
                            providers,
                            model: rung.model.clone(),
                            variant: rung.variant.clone(),
                        },
                        rung.clone(),
                    )
                })
            })
            .collect(),
    )
}

/// A chain rung resolves when one of its own providers serves its model in the live registry.
pub fn is_category_chain_rung_resolvable(
    entry: &DelegateFallbackEntry,
    available_models: &BTreeSet<String>,
) -> bool {
    entry
        .providers
        .iter()
        .any(|provider| provider_serves_model(provider, &entry.model, available_models))
}

/// A builtin-only category is usable only when at least one fallback rung resolves; any explicit
/// `categories.<name>` entry opts out of the check.
pub fn is_category_chain_viable(
    category_name: &str,
    has_explicit_user_config: bool,
    available_models: &BTreeSet<String>,
) -> bool {
    if has_explicit_user_config {
        return true;
    }
    match category_fallback_chain(category_name) {
        Some(chain) if !chain.is_empty() => chain
            .iter()
            .any(|rung| is_category_chain_rung_resolvable(rung, available_models)),
        _ => true,
    }
}
