use serde_json::Value;

use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, any, array, boolean, enumeration, number, optional,
    positive_integer, record, string, union,
};
use crate::models::models_distribution::MODELS_DISTRIBUTION_VALUES;
use crate::schema::fallback_models::{
    normalize_legacy_model_fields, omo_fallback_model_object_schema, omo_fallback_models_schema,
    omo_reasoning_effort_schema, omo_thinking_config_schema,
};
use crate::schema::model_ref::omo_reasoning_schema;

pub fn omo_category_config_object_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            optional("description", string()),
            optional("model", string()),
            optional(
                "models",
                array(union(vec![string(), omo_fallback_model_object_schema()])),
            ),
            optional(
                "models_distribution",
                enumeration(&MODELS_DISTRIBUTION_VALUES),
            ),
            optional("reasoning", omo_reasoning_schema()),
            optional(
                "temperature",
                crate::internal::validate::number_between(0.0, 2.0),
            ),
            optional("top_p", crate::internal::validate::number_between(0.0, 1.0)),
            optional("max_tokens", positive_integer()),
            optional("provider_options", record(any())),
            optional("fallback_models", omo_fallback_models_schema()),
            optional("variant", string()),
            optional("maxTokens", number()),
            optional("thinking", omo_thinking_config_schema()),
            optional("reasoningEffort", omo_reasoning_effort_schema()),
            optional("textVerbosity", enumeration(&["low", "medium", "high"])),
            optional("tools", record(boolean())),
            optional("prompt_append", string()),
            optional("max_prompt_tokens", positive_integer()),
            optional("is_unstable_agent", boolean()),
            optional("disable", boolean()),
            optional("warn_unavailable", boolean()),
            // mhc policy gate for omo#12239: only the builtin architect reads it.
            optional("allow_edits", boolean()),
        ],
        strict: true,
        preprocess: Some(without_retired_category_keys as PreprocessFn),
        refine: None,
    }
}

// `provider_lock` only withheld a parent-session model fallback that upstream retired (omo #12010):
// an omo.json that still carries the key loads unchanged and the key is dropped.
fn without_retired_category_keys(value: &Value) -> Value {
    let mut normalized = normalize_legacy_model_fields(value);
    if let Value::Object(entry) = &mut normalized {
        entry.shift_remove("provider_lock");
    }
    normalized
}

pub fn omo_category_config_object_schema() -> Node {
    Node::Object(Box::new(omo_category_config_object_spec()))
}

pub fn omo_category_config_schema() -> Node {
    omo_category_config_object_schema()
}

pub fn omo_category_config_layer_schema() -> Node {
    Node::Object(Box::new(omo_category_config_object_spec().partial()))
}

pub fn omo_categories_config_schema() -> Node {
    record(omo_category_config_schema())
}

pub fn omo_categories_config_layer_schema() -> Node {
    record(omo_category_config_layer_schema())
}
