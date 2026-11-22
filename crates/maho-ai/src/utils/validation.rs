//! Port of senpi packages/ai/src/utils/validation.ts.
//!
//! senpi validates with TypeBox 1.3.34 (`Compile` + `Value.Convert`); `validation/engine.rs`
//! and `validation/formats.rs` port the parts of TypeBox that decide the verdict and the
//! `en_US` error lines. Rust tool parameters are always plain JSON Schema (`Value`), so every
//! schema takes senpi's plain-JSON-schema branch (`coerceWithJsonSchema`); `Value.Convert`
//! is a no-op for plain schemas, which the generated fixture `tests/golden/ai-validation.json`
//! confirms.

mod engine;
mod formats;

/// Returns existing engine error paths and messages without coercion or null normalization.
pub fn strict_errors(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Vec<(String, String)> {
    engine::errors(schema, value)
        .into_iter()
        .map(|issue| (issue.instance_path, issue.message))
        .collect()
}

/// `strict_errors` with each issue's keyword and missing required properties, for protocol-owned
/// validators that report Ajv-shaped errors (`instance_path`, `keyword`, `message`, `requiredProperties`).
pub fn strict_issues(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Vec<super::tool_argument_validator::ToolArgumentValidationError> {
    engine::errors(schema, value)
        .into_iter()
        .map(
            |issue| super::tool_argument_validator::ToolArgumentValidationError {
                instance_path: issue.instance_path,
                keyword: issue.keyword.to_owned(),
                message: Some(issue.message),
                params: if issue.required_properties.is_empty() {
                    serde_json::Map::new()
                } else {
                    [(
                        "requiredProperties".to_owned(),
                        serde_json::Value::from(issue.required_properties),
                    )]
                    .into_iter()
                    .collect()
                },
            },
        )
        .collect()
}

/// Check recovered arguments without applying the normal tool-argument coercions.
pub(crate) fn check_recovered_arguments(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> bool {
    engine::check(schema, value)
}

use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};
use crate::utils::js::{
    json_number, json_stringify_pretty, number_to_string, string_to_number, trim,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("Tool \"{0}\" not found")]
    ToolNotFound(String),
    #[error("{0}")]
    Invalid(String),
}

fn schema_types(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn matches_json_type(value: &Value, type_name: &str) -> bool {
    match type_name {
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// `coercePrimitiveByType`; `None` when the value is returned unchanged.
fn coerce_primitive_by_type(value: &Value, type_name: &str) -> Option<Value> {
    match (type_name, value) {
        ("number" | "integer", Value::Null) => Some(json_number(0.0)),
        ("number" | "integer", Value::Bool(flag)) => {
            Some(json_number(if *flag { 1.0 } else { 0.0 }))
        }
        ("number", Value::String(text)) if !trim(text).is_empty() => {
            let parsed = string_to_number(text);
            parsed.is_finite().then(|| json_number(parsed))
        }
        ("integer", Value::String(text)) if !trim(text).is_empty() => {
            let parsed = string_to_number(text);
            (parsed.is_finite() && parsed.fract() == 0.0).then(|| json_number(parsed))
        }
        ("boolean", Value::Null) => Some(Value::Bool(false)),
        ("boolean", Value::String(text)) if text == "true" || text == "false" => {
            Some(Value::Bool(text == "true"))
        }
        ("boolean", Value::Number(n)) => match n.as_f64() {
            Some(1.0) => Some(Value::Bool(true)),
            Some(0.0) => Some(Value::Bool(false)),
            _ => None,
        },
        ("string", Value::Null) => Some(Value::String(String::new())),
        ("string", Value::Number(n)) => n.as_f64().map(|n| Value::String(number_to_string(n))),
        ("string", Value::Bool(flag)) => Some(Value::String(flag.to_string())),
        ("null", Value::String(text)) if text.is_empty() => Some(Value::Null),
        ("null", Value::Bool(false)) => Some(Value::Null),
        ("null", Value::Number(n)) if n.as_f64() == Some(0.0) => Some(Value::Null),
        _ => None,
    }
}

fn apply_schema_object_coercion(object: &mut Map<String, Value>, schema: &Map<String, Value>) {
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if let Some(slot) = object.get_mut(key) {
                *slot = coerce_with_json_schema(slot.take(), property_schema);
            }
        }
    }
    if let Some(additional) = schema
        .get("additionalProperties")
        .filter(|v| v.is_object() || v.is_array())
    {
        for (key, slot) in object.iter_mut() {
            if !properties.is_some_and(|p| p.contains_key(key)) {
                *slot = coerce_with_json_schema(slot.take(), additional);
            }
        }
    }
}

fn apply_schema_array_coercion(items: &mut [Value], schema: &Map<String, Value>) {
    match schema.get("items") {
        Some(Value::Array(tuple)) => {
            for (slot, item_schema) in items.iter_mut().zip(tuple) {
                *slot = coerce_with_json_schema(slot.take(), item_schema);
            }
        }
        Some(item_schema @ Value::Object(_)) => {
            for slot in items.iter_mut() {
                *slot = coerce_with_json_schema(slot.take(), item_schema);
            }
        }
        _ => {}
    }
}

fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    if schemas.iter().any(|schema| engine::check(schema, &value)) {
        return value;
    }
    for schema in schemas {
        let coerced = coerce_with_json_schema(value.clone(), schema);
        if engine::check(schema, &coerced) {
            return coerced;
        }
    }
    value
}

fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let Some(schema) = schema.as_object() else {
        return value;
    };
    let mut next = value;
    if let Some(Value::Array(all)) = schema.get("allOf") {
        for nested in all {
            next = coerce_with_json_schema(next, nested);
        }
    }
    if let Some(Value::Array(any)) = schema.get("anyOf") {
        next = coerce_with_union_schema(next, any);
    }
    if let Some(Value::Array(one)) = schema.get("oneOf") {
        next = coerce_with_union_schema(next, one);
    }
    let types = schema_types(schema);
    let matches_union_member = types.len() > 1 && types.iter().any(|t| matches_json_type(&next, t));
    if !types.is_empty()
        && !matches_union_member
        && let Some(candidate) = types
            .iter()
            .find_map(|t| coerce_primitive_by_type(&next, t))
    {
        next = candidate;
    }
    if types.contains(&"object")
        && let Value::Object(object) = &mut next
    {
        apply_schema_object_coercion(object, schema);
    }
    if types.contains(&"array")
        && let Value::Array(items) = &mut next
    {
        apply_schema_array_coercion(items, schema);
    }
    next
}

fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Value::Array(items) = value {
        match schema.get("items") {
            Some(Value::Array(tuple)) => items
                .iter_mut()
                .zip(tuple)
                .for_each(|(item, s)| normalize_optional_nulls(item, s)),
            Some(item_schema) if item_schema.is_object() => items
                .iter_mut()
                .for_each(|item| normalize_optional_nulls(item, item_schema)),
            _ => {}
        }
        return;
    }
    let (Value::Object(object), Some(Value::Object(properties))) =
        (value, schema.get("properties"))
    else {
        return;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for (key, property_schema) in properties {
        let Some(slot) = object.get_mut(key) else {
            continue;
        };
        let is_ref = property_schema.get("$ref").is_some_and(Value::is_string);
        if slot.is_null()
            && !required.contains(&key.as_str())
            && !is_ref
            && !engine::check(property_schema, &Value::Null)
        {
            object.shift_remove(key);
        } else {
            normalize_optional_nulls(slot, property_schema);
        }
    }
}

fn format_validation_path(issue: &engine::Issue) -> String {
    let base = issue
        .instance_path
        .strip_prefix('/')
        .unwrap_or(&issue.instance_path)
        .replace('/', ".");
    if issue.keyword == "required"
        && let Some(property) = issue.required_properties.first().filter(|p| !p.is_empty())
    {
        return if base.is_empty() {
            property.clone()
        } else {
            format!("{base}.{property}")
        };
    }
    if base.is_empty() { "root".into() } else { base }
}

fn accepted_json_types(schema: &Value) -> Option<std::collections::BTreeSet<String>> {
    let schema = schema.as_object()?;
    let types = schema_types(schema);
    if !types.is_empty() {
        return Some(types.into_iter().map(str::to_owned).collect());
    }
    let members = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)?;
    if members.is_empty() {
        return None;
    }
    let mut accepted = std::collections::BTreeSet::new();
    for member in members {
        let member_types = accepted_json_types(member)?;
        accepted.extend(member_types);
    }
    Some(accepted)
}

fn parse_json_structure(
    text: &str,
    accepted: &std::collections::BTreeSet<String>,
) -> Option<Value> {
    let trimmed = trim(text);
    let wants_array = accepted.contains("array") && trimmed.starts_with('[');
    let wants_object = accepted.contains("object") && trimmed.starts_with('{');
    if !wants_array && !wants_object {
        return None;
    }
    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    if wants_array && parsed.is_array() {
        return Some(parsed);
    }
    if wants_object && parsed.is_object() {
        return Some(parsed);
    }
    None
}

fn decode_json_encoded_structures(value: Value, schema: &Value) -> Value {
    if let Value::String(ref text) = value {
        let Some(accepted) = accepted_json_types(schema) else {
            return value;
        };
        if accepted.contains("string") {
            return value;
        }
        return match parse_json_structure(text, &accepted) {
            Some(decoded) => decode_json_encoded_structures(decoded, schema),
            None => value,
        };
    }
    let Some(schema_obj) = schema.as_object() else {
        return value;
    };
    let types = schema_types(schema_obj);
    match value {
        Value::Array(mut items) if types.contains(&"array") => {
            match schema_obj.get("items") {
                Some(Value::Array(tuple)) => {
                    for (slot, item_schema) in items.iter_mut().zip(tuple) {
                        *slot = decode_json_encoded_structures(slot.take(), item_schema);
                    }
                }
                Some(item_schema @ Value::Object(_)) => {
                    for slot in items.iter_mut() {
                        *slot = decode_json_encoded_structures(slot.take(), item_schema);
                    }
                }
                _ => {}
            }
            Value::Array(items)
        }
        Value::Object(mut record) if types.contains(&"object") => {
            let properties = schema_obj.get("properties").and_then(Value::as_object);
            let additional = schema_obj
                .get("additionalProperties")
                .filter(|v| v.is_object());
            let keys: Vec<String> = record.keys().cloned().collect();
            for key in keys {
                let property_schema = properties.and_then(|props| props.get(&key)).or(additional);
                if let (Some(prop_schema), Some(slot)) = (property_schema, record.get_mut(&key)) {
                    *slot = decode_json_encoded_structures(slot.take(), prop_schema);
                }
            }
            Value::Object(record)
        }
        other => other,
    }
}

struct DiscriminatedBranch<'a> {
    schema: &'a Value,
    values: Vec<Value>,
    optional: bool,
    default_value: Option<Value>,
}

struct DiscriminatedUnion<'a> {
    property: String,
    branches: Vec<DiscriminatedBranch<'a>>,
    default_value: Option<Value>,
}

enum SelectedBranch<'a> {
    Branch {
        branch: &'a DiscriminatedBranch<'a>,
        value: Value,
        defaulted: bool,
    },
    Unknown,
    None,
}

fn const_values(schema: Option<&Value>) -> Option<Vec<Value>> {
    let schema = schema?.as_object()?;
    if let Some(val) = schema.get("const") {
        return Some(vec![val.clone()]);
    }
    if let Some(Value::Array(items)) = schema.get("enum")
        && !items.is_empty()
    {
        return Some(items.clone());
    }
    let members = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)?;
    if members.is_empty() {
        return None;
    }
    let mut values = Vec::new();
    for member in members {
        let member_values = const_values(Some(member))?;
        values.extend(member_values);
    }
    Some(values)
}

fn discriminated_branches<'a>(
    members: &'a [Value],
    property: &str,
) -> Option<Vec<DiscriminatedBranch<'a>>> {
    let mut seen: Vec<Value> = Vec::new();
    let mut branches = Vec::new();
    for member in members {
        let member_obj = member.as_object()?;
        let property_schema = member_obj
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(property));
        let values = const_values(property_schema)?;
        if values.is_empty() || values.iter().any(|v| seen.contains(v)) {
            return None;
        }
        seen.extend(values.iter().cloned());
        let optional = !member_obj
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|req| req.iter().any(|r| r.as_str() == Some(property)));
        let default_value = property_schema
            .and_then(Value::as_object)
            .and_then(|s| s.get("default"))
            .cloned();
        branches.push(DiscriminatedBranch {
            schema: member,
            values,
            optional,
            default_value,
        });
    }
    Some(branches)
}

fn find_discriminated_union(schema: &Value) -> Option<DiscriminatedUnion<'_>> {
    let schema_obj = schema.as_object()?;
    let members = schema_obj
        .get("anyOf")
        .or_else(|| schema_obj.get("oneOf"))
        .and_then(Value::as_array)?;
    if members.len() < 2 {
        return None;
    }
    let first_props = members
        .first()?
        .as_object()?
        .get("properties")
        .and_then(Value::as_object)?;
    if !members.iter().all(|m| {
        m.as_object()
            .and_then(|o| o.get("properties"))
            .and_then(Value::as_object)
            .is_some()
    }) {
        return None;
    }
    for property in first_props.keys() {
        let Some(branches) = discriminated_branches(members, property) else {
            continue;
        };
        let default_value = schema_obj
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(property))
            .and_then(Value::as_object)
            .and_then(|s| s.get("default"))
            .cloned()
            .or_else(|| branches.iter().find_map(|b| b.default_value.clone()));
        return Some(DiscriminatedUnion {
            property: property.clone(),
            branches,
            default_value,
        });
    }
    None
}

fn select_branch<'a>(
    union: &'a DiscriminatedUnion<'a>,
    value: Option<&Value>,
) -> SelectedBranch<'a> {
    if let Some(val) = value {
        return match union.branches.iter().find(|c| c.values.contains(val)) {
            Some(branch) => SelectedBranch::Branch {
                branch,
                value: val.clone(),
                defaulted: false,
            },
            None => SelectedBranch::Unknown,
        };
    }
    if let Some(def) = &union.default_value
        && let Some(branch) = union.branches.iter().find(|c| c.values.contains(def))
    {
        return SelectedBranch::Branch {
            branch,
            value: def.clone(),
            defaulted: true,
        };
    }
    let optional: Vec<_> = union.branches.iter().filter(|c| c.optional).collect();
    if optional.len() == 1 && optional[0].values.len() == 1 {
        return SelectedBranch::Branch {
            branch: optional[0],
            value: optional[0].values[0].clone(),
            defaulted: true,
        };
    }
    SelectedBranch::None
}

fn format_discriminated_union_error(schema: &Value, args: &Value) -> Option<String> {
    let args_obj = args.as_object()?;
    let union = find_discriminated_union(schema)?;
    let received = args_obj.get(&union.property);
    match select_branch(&union, received) {
        SelectedBranch::None => None,
        SelectedBranch::Unknown => {
            let received_json = received
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .unwrap_or_default();
            let allowed: Vec<String> = union
                .branches
                .iter()
                .flat_map(|b| b.values.iter())
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .collect();
            Some(format!(
                "  - {}: must be one of {} (received {received_json})",
                union.property,
                allowed.join(", ")
            ))
        }
        SelectedBranch::Branch {
            branch,
            value,
            defaulted,
        } => {
            let mut root_schema = schema.as_object()?.clone();
            root_schema.shift_remove("anyOf");
            root_schema.shift_remove("oneOf");
            let root_errors = engine::errors(&Value::Object(root_schema), args);
            let branch_errors = engine::errors(branch.schema, args);
            let mut unique: Vec<String> = Vec::new();
            for issue in root_errors.iter().chain(branch_errors.iter()) {
                let msg = format!("{}: {}", format_validation_path(issue), issue.message);
                if !unique.contains(&msg) {
                    unique.push(msg);
                }
            }
            if unique.is_empty() {
                return None;
            }
            let val_json = serde_json::to_string(&value).unwrap_or_default();
            let default_note = if defaulted {
                format!(" (default when {} is omitted)", union.property)
            } else {
                String::new()
            };
            Some(format!(
                "  - for {} {val_json}{default_note}: {}",
                union.property,
                unique.join("; ")
            ))
        }
    }
}

pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, ValidationError> {
    let tool = tools
        .iter()
        .find(|t| t.name == tool_call.name)
        .ok_or_else(|| ValidationError::ToolNotFound(tool_call.name.clone()))?;
    validate_tool_arguments(tool, tool_call)
}

pub fn validate_tool_arguments(
    tool: &Tool,
    tool_call: &ToolCall,
) -> Result<Value, ValidationError> {
    validate_value(
        &tool.parameters,
        &tool_call.name,
        &Value::Object(tool_call.arguments.clone()),
    )
}

/// Validate prepared arguments without requiring the received value to be an object.
pub fn validate_tool_arguments_value(
    tool: &Tool,
    tool_name: &str,
    received: &Value,
) -> Result<Value, ValidationError> {
    validate_value(&tool.parameters, tool_name, received)
}

pub use super::tool_argument_validator::register_tool_argument_validator;

/// `formatValidationPath` for a protocol validator's Ajv-shaped error: `required` names the
/// missing property from `requiredProperties` or `missingProperty`.
fn format_protocol_validation_path(
    error: &super::tool_argument_validator::ToolArgumentValidationError,
) -> String {
    let base = error
        .instance_path
        .strip_prefix('/')
        .unwrap_or(&error.instance_path)
        .replace('/', ".");
    if error.keyword == "required" {
        let property = error
            .required_properties()
            .and_then(|properties| properties.first().map(|p| (*p).to_owned()))
            .or_else(|| error.missing_property().map(str::to_owned))
            .filter(|property| !property.is_empty());
        if let Some(property) = property {
            return if base.is_empty() {
                property
            } else {
                format!("{base}.{property}")
            };
        }
    }
    if base.is_empty() { "root".into() } else { base }
}

fn validate_value(
    parameters: &Value,
    tool_name: &str,
    received: &Value,
) -> Result<Value, ValidationError> {
    // A protocol-owned validator bypasses local coercion and null normalization and returns the
    // original arguments.
    if let Some(protocol_validator) =
        super::tool_argument_validator::get_tool_argument_validator(parameters)
    {
        return match protocol_validator(received) {
            Ok(()) => Ok(received.clone()),
            Err(errors) => {
                let lines: Vec<String> = errors
                    .iter()
                    .map(|error| {
                        format!(
                            "  - {}: {} ({})",
                            format_protocol_validation_path(error),
                            error.message.as_deref().unwrap_or(&error.keyword),
                            error.keyword
                        )
                    })
                    .collect();
                let body = if lines.is_empty() {
                    "Unknown validation error".to_owned()
                } else {
                    lines.join("\n")
                };
                Err(ValidationError::Invalid(format!(
                    "Validation failed for tool \"{tool_name}\":\n{body}"
                )))
            }
        };
    }
    let mut args = received.clone();
    normalize_optional_nulls(&mut args, parameters);
    args = decode_json_encoded_structures(args, parameters);
    let coerced = coerce_with_json_schema(args.clone(), parameters);
    if args.is_object() && coerced.is_object() {
        args = coerced;
    } else if coerced != args {
        return Ok(if engine::check(parameters, &coerced) {
            coerced
        } else {
            args
        });
    }
    let issues = engine::errors(parameters, &args);
    if issues.is_empty() {
        return Ok(args);
    }
    let body = format_discriminated_union_error(parameters, &args).unwrap_or_else(|| {
        let lines: Vec<String> = issues
            .iter()
            .map(|issue| format!("  - {}: {}", format_validation_path(issue), issue.message))
            .collect();
        if lines.is_empty() {
            "Unknown validation error".to_owned()
        } else {
            lines.join("\n")
        }
    });
    Err(ValidationError::Invalid(format!(
        "Validation failed for tool \"{tool_name}\":\n{body}\n\nReceived arguments:\n{}",
        json_stringify_pretty(received)
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // tool-argument-validator.ts: protocol-owned validators keep the original arguments.
    #[test]
    fn protocol_owned_validator_bypasses_coercion_and_reports_missing_property() {
        let parameters = json!({
            "type": "object",
            "properties": { "count": { "type": "number" }, "note": { "type": "string" } },
            "required": ["count"],
            "x-protocol-validator-fixture": true
        });
        register_tool_argument_validator(
            &parameters,
            std::sync::Arc::new(|value: &Value| {
                if value.get("count").is_some() {
                    return Ok(());
                }
                Err(vec![
                    super::super::tool_argument_validator::ToolArgumentValidationError {
                        instance_path: String::new(),
                        keyword: "required".into(),
                        message: Some("must have required property 'count'".into()),
                        params: [("missingProperty".to_owned(), json!("count"))]
                            .into_iter()
                            .collect(),
                    },
                ])
            }),
        );
        let protocol_tool = tool(parameters);
        let original = json!({ "count": "5", "note": null });
        assert_eq!(
            validate_tool_arguments(&protocol_tool, &call(original.clone())).expect("valid"),
            original
        );
        let error = validate_tool_arguments(&protocol_tool, &call(json!({ "note": "x" })))
            .expect_err("invalid");
        assert_eq!(
            error.to_string(),
            "Validation failed for tool \"echo\":\n  - count: must have required property 'count' (required)"
        );
    }

    fn tool(parameters: Value) -> Tool {
        serde_json::from_value(
            json!({ "name": "echo", "description": "Echo tool", "parameters": parameters }),
        )
        .expect("tool")
    }

    fn call(arguments: Value) -> ToolCall {
        serde_json::from_value(json!({ "id": "tool-1", "name": "echo", "arguments": arguments }))
            .expect("tool call")
    }

    fn plain(schema: Value, input: Value) -> (Tool, ToolCall) {
        (
            tool(
                json!({ "type": "object", "properties": { "value": schema }, "required": ["value"] }),
            ),
            call(json!({ "value": input })),
        )
    }

    #[test]
    fn strict_errors_preserves_engine_errors_without_coercion_or_null_normalization() {
        // Given: optional non-nullable fields that ordinary validation can normalize or coerce.
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "number" },
                "label": { "type": "string" }
            }
        });
        let value = json!({ "count": "42", "label": null });
        let original = value.clone();
        let expected: Vec<_> = engine::errors(&schema, &value)
            .into_iter()
            .map(|issue| (issue.instance_path, issue.message))
            .collect();

        // When: the public strict wrapper evaluates the received value.
        let errors = strict_errors(&schema, &value);

        // Then: exact engine errors survive, and the input remains untouched.
        assert_eq!(errors, expected);
        assert_eq!(
            errors
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/count", "/label"]
        );
        assert_eq!(value, original);
    }

    #[test]
    fn strict_errors_is_empty_when_received_value_satisfies_schema() {
        // Given: a valid value, including an explicitly nullable field.
        let schema = json!({ "type": "array", "items": { "type": ["number", "null"] } });
        let value = json!([42, null]);

        // When: the public strict wrapper evaluates it.
        let errors = strict_errors(&schema, &value);

        // Then: no errors are returned.
        assert!(errors.is_empty());
    }

    #[test]
    fn validates_non_object_values_against_the_tool_schema() {
        let tool = tool(json!({ "type": "array", "items": { "type": "number" } }));

        let validated = validate_tool_arguments_value(&tool, "echo", &json!([1, 2]));

        assert_eq!(validated, Ok(json!([1, 2])));
    }

    #[test]
    fn still_validates_when_function_constructor_is_unavailable() {
        // TS disables `Function` to force TypeBox's interpreter; the Rust engine is always interpreted.
        let tool = tool(
            json!({ "type": "object", "required": ["count"], "properties": { "count": { "type": "number" } } }),
        );
        assert_eq!(
            validate_tool_arguments(&tool, &call(json!({ "count": "42" }))),
            Ok(json!({ "count": 42 }))
        );
    }

    #[test]
    fn coerces_serialized_plain_json_schemas_with_ajv_compatible_primitive_rules() {
        let cases = [
            (json!({ "type": "number" }), json!("42"), json!(42)),
            (json!({ "type": "number" }), json!(true), json!(1)),
            (json!({ "type": "number" }), json!(null), json!(0)),
            (json!({ "type": "integer" }), json!("42"), json!(42)),
            (json!({ "type": "boolean" }), json!("true"), json!(true)),
            (json!({ "type": "boolean" }), json!("false"), json!(false)),
            (json!({ "type": "boolean" }), json!(1), json!(true)),
            (json!({ "type": "boolean" }), json!(0), json!(false)),
            (json!({ "type": "string" }), json!(null), json!("")),
            (json!({ "type": "string" }), json!(true), json!("true")),
            (json!({ "type": "null" }), json!(""), json!(null)),
            (json!({ "type": "null" }), json!(0), json!(null)),
            (json!({ "type": "null" }), json!(false), json!(null)),
            (
                json!({ "type": ["number", "string"] }),
                json!("1"),
                json!("1"),
            ),
            (
                json!({ "type": ["boolean", "number"] }),
                json!("1"),
                json!(1),
            ),
        ];
        for (schema, input, expected) in cases {
            let (tool, call) = plain(schema.clone(), input.clone());
            assert_eq!(
                validate_tool_arguments(&tool, &call),
                Ok(json!({ "value": expected })),
                "{schema} {input}"
            );
        }
    }

    #[test]
    fn treats_null_as_omission_for_optional_non_nullable_properties() {
        let tool = tool(json!({
            "type": "object",
            "required": ["path", "metadata"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "number" },
                "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
                "metadata": { "type": "object", "required": [], "properties": { "enabled": { "type": "boolean" } } }
            }
        }));
        let call = call(
            json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } }),
        );
        assert_eq!(
            validate_tool_arguments(&tool, &call),
            Ok(json!({ "path": "file.txt", "nullable": null, "metadata": {} }))
        );
    }

    #[test]
    fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
        let tool = tool(json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/value" } },
            "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
        }));
        assert_eq!(
            validate_tool_arguments(&tool, &call(json!({ "value": null }))),
            Ok(json!({ "value": null }))
        );
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
        let tool = tool(
            json!({ "type": "object", "required": ["value"], "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } } }),
        );
        assert_eq!(
            validate_tool_arguments(&tool, &call(json!({ "value": null }))),
            Ok(json!({ "value": null }))
        );
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_one_of_nullable_union_arm() {
        let (tool, call) = plain(
            json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] }),
            json!(null),
        );
        assert_eq!(
            validate_tool_arguments(&tool, &call),
            Ok(json!({ "value": null }))
        );
    }

    #[test]
    fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
        let (tool, call) = plain(
            json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
            json!("42"),
        );
        assert_eq!(
            validate_tool_arguments(&tool, &call),
            Ok(json!({ "value": 42 }))
        );
    }

    #[test]
    fn accepts_null_for_nullable_array_schemas_with_items() {
        let (tool, call) = plain(
            json!({ "type": ["array", "null"], "items": { "type": "string" } }),
            json!(null),
        );
        assert!(engine::check(
            &tool.parameters,
            &Value::Object(call.arguments.clone())
        ));
        assert_eq!(
            validate_tool_arguments(&tool, &call),
            Ok(json!({ "value": null }))
        );
    }

    #[test]
    fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
        let cases = [
            (json!({ "type": "boolean" }), json!("1")),
            (json!({ "type": "boolean" }), json!("0")),
            (json!({ "type": "null" }), json!("null")),
            (json!({ "type": "integer" }), json!("42.1")),
        ];
        for (schema, input) in cases {
            let (tool, call) = plain(schema, input);
            let Err(ValidationError::Invalid(message)) = validate_tool_arguments(&tool, &call)
            else {
                panic!("expected failure")
            };
            assert!(message.contains("Validation failed"));
        }
    }

    #[test]
    fn validate_tool_call_reports_unknown_tools() {
        let err = validate_tool_call(&[], &call(json!({}))).expect_err("unknown tool");
        assert_eq!(err.to_string(), "Tool \"echo\" not found");
    }

    #[test]
    fn decodes_json_encoded_arrays_and_objects_for_serialized_plain_json_schemas() {
        let (array_tool, array_call) = plain(
            json!({ "type": "array", "items": { "type": "string" } }),
            json!("[\"/refs/a.png\",\"/refs/b.png\"]"),
        );
        let (object_tool, object_call) = plain(
            json!({
                "type": "object",
                "properties": { "paths": { "type": "array", "items": { "type": "string" } } }
            }),
            json!("{\"paths\":[\"/refs/a.png\"]}"),
        );
        assert_eq!(
            validate_tool_arguments(&array_tool, &array_call),
            Ok(json!({ "value": ["/refs/a.png", "/refs/b.png"] }))
        );
        assert_eq!(
            validate_tool_arguments(&object_tool, &object_call),
            Ok(json!({ "value": { "paths": ["/refs/a.png"] } }))
        );
    }

    #[test]
    fn decodes_json_encoded_structures_nested_inside_arrays_and_objects() {
        let batch_tool = tool(json!({
            "type": "object",
            "properties": {
                "jobs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "paths": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                }
            }
        }));
        let batch_call = call(json!({
            "jobs": [
                { "paths": "[\"/refs/a.png\"]" },
                "{\"paths\":[\"/refs/b.png\"]}"
            ]
        }));
        assert_eq!(
            validate_tool_arguments(&batch_tool, &batch_call),
            Ok(json!({
                "jobs": [
                    { "paths": ["/refs/a.png"] },
                    { "paths": ["/refs/b.png"] }
                ]
            }))
        );
    }

    #[test]
    fn leaves_strings_alone_where_schema_accepts_a_string() {
        let echo_tool = tool(json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "either": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                }
            }
        }));
        let echo_call = call(json!({
            "text": "[\"kept\"]",
            "either": "[\"kept\"]"
        }));
        assert_eq!(
            validate_tool_arguments(&echo_tool, &echo_call),
            Ok(json!({
                "text": "[\"kept\"]",
                "either": "[\"kept\"]"
            }))
        );
    }

    #[test]
    fn discriminated_unions_report_selected_or_default_branch_and_allowed_values() {
        let run_props = json!({
            "action": {
                "anyOf": [
                    { "const": "run" },
                    { "const": "list" },
                    { "const": "peek" },
                    { "const": "stop" }
                ]
            },
            "language": { "anyOf": [{ "const": "js" }, { "const": "py" }] },
            "code": { "type": "string" },
            "summary": { "type": "string" },
            "timeout": { "type": "number", "minimum": 1 },
            "cell_id": { "type": "string", "minLength": 1 }
        });
        let union_tool = tool(json!({
            "type": "object",
            "properties": run_props,
            "anyOf": [
                {
                    "type": "object",
                    "properties": {
                        "action": { "const": "run" },
                        "language": { "anyOf": [{ "const": "js" }, { "const": "py" }] },
                        "code": { "type": "string" },
                        "summary": { "type": "string" },
                        "timeout": { "type": "number", "minimum": 1 },
                        "cell_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["language", "code", "summary"]
                },
                {
                    "type": "object",
                    "properties": { "action": { "const": "list" } },
                    "required": ["action"]
                },
                {
                    "type": "object",
                    "properties": {
                        "action": { "anyOf": [{ "const": "peek" }, { "const": "stop" }] },
                        "cell_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["action", "cell_id"]
                }
            ]
        }));
        let error_lines = |t: &Tool, args: Value| -> Vec<String> {
            let Err(ValidationError::Invalid(msg)) = validate_tool_arguments(t, &call(args)) else {
                panic!("expected validation failure");
            };
            msg.split("\n\nReceived arguments:")
                .next()
                .unwrap_or("")
                .lines()
                .filter(|line| line.starts_with("  - "))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(
            error_lines(&union_tool, json!({ "code": "1", "summary": "s" })),
            vec![
                "  - for action \"run\" (default when action is omitted): language: must have required properties language"
            ]
        );
        assert_eq!(
            error_lines(&union_tool, json!({ "action": "peek" })),
            vec!["  - for action \"peek\": cell_id: must have required properties cell_id"]
        );
        assert_eq!(
            error_lines(&union_tool, json!({ "action": "list", "timeout": "soon" })),
            vec!["  - for action \"list\": timeout: must be number"]
        );
        assert_eq!(
            error_lines(&union_tool, json!({ "action": "peak", "cell_id": "x" })),
            vec![
                "  - action: must be one of \"run\", \"list\", \"peek\", \"stop\" (received \"peak\")"
            ]
        );
    }

    fn fixture() -> Value {
        // Generated by `bun tools/golden/run.mjs --case ai-validation` from pinned senpi.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/ai-validation.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture"))
            .expect("fixture json")
    }

    #[test]
    fn engine_matches_generated_typebox_checks_and_errors() {
        let fixture = fixture();
        let checks = fixture["checks"].as_array().expect("checks");
        assert!(!checks.is_empty());
        for case in checks {
            let (schema, value) = (&case["schema"], &case["value"]);
            assert_eq!(
                engine::check(schema, value),
                case["ok"].as_bool().expect("ok"),
                "check {schema} {value}"
            );
            let errors: Vec<Value> = engine::errors(schema, value)
                .into_iter()
                .map(|i| json!([i.keyword, i.instance_path, i.message]))
                .collect();
            assert_eq!(
                Value::Array(errors),
                case["errors"],
                "errors {schema} {value}"
            );
        }
    }

    #[test]
    fn validate_matches_generated_senpi_results() {
        let fixture = fixture();
        let calls = fixture["calls"].as_array().expect("calls");
        assert!(!calls.is_empty());
        for case in calls {
            let got = validate_value(&case["parameters"], "echo", &case["arguments"]);
            if case["ok"] == json!(true) {
                assert_eq!(got, Ok(case["value"].clone()), "{case}");
            } else {
                assert_eq!(
                    got,
                    Err(ValidationError::Invalid(
                        case["error"].as_str().expect("error").to_owned()
                    )),
                    "{case}"
                );
            }
        }
    }
}
