use super::eval_request::{EvalRunField, eval_run_field_required_message, normalize_eval_summary};
use maho_ai::utils::tool_argument_validator::{
    ToolArgumentValidationError, register_tool_argument_validator,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const RUN_FIELDS: [EvalRunField; 3] = [
    EvalRunField::Language,
    EvalRunField::Code,
    EvalRunField::Summary,
];

pub fn register_eval_argument_validator(schema: &Value) {
    let mut generic = schema.clone();
    if let Some(object) = generic.as_object_mut() {
        object.insert("x-eval-generic".into(), Value::Bool(true));
    }
    register_tool_argument_validator(
        schema,
        Arc::new(move |value: &Value| {
            let missing = missing_run_field_errors(value);
            if !missing.is_empty() {
                return Err(missing);
            }
            let errors = maho_ai::utils::validation::strict_issues(&generic, value);
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors)
            }
        }),
    );
}

pub fn missing_run_field_errors(value: &Value) -> Vec<ToolArgumentValidationError> {
    let Some(record) = value.as_object() else {
        return Vec::new();
    };
    if let Some(action) = record.get("action")
        && action.as_str() != Some("run")
    {
        return Vec::new();
    }
    RUN_FIELDS
        .into_iter()
        .filter(|field| is_missing(*field, record.get(field.as_str())))
        .map(|field| {
            let mut params = Map::new();
            params.insert("missingProperty".into(), json!(field.as_str()));
            ToolArgumentValidationError {
                instance_path: String::new(),
                keyword: "required".into(),
                message: Some(eval_run_field_required_message(field)),
                params,
            }
        })
        .collect()
}

fn is_missing(field: EvalRunField, value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return true;
    };
    if value.is_null() {
        return true;
    }
    field == EvalRunField::Summary && value.is_string() && normalize_eval_summary(value).is_none()
}
