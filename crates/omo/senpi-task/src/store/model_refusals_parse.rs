//! `store/model-refusals-parse.ts`: `model_refusals` is diagnostic provenance. A malformed entry, or a
//! reason a newer build added, is dropped and never a reason to lose the whole record.

use serde_json::Value;

use crate::state::ModelRefusal;

pub fn parse_optional_model_refusals(record: &Value) -> Option<Vec<ModelRefusal>> {
    let entries = record.get("model_refusals")?.as_array()?;
    let refusals: Vec<ModelRefusal> = entries.iter().filter_map(parse_refusal).collect();
    (!refusals.is_empty()).then_some(refusals)
}

fn parse_refusal(entry: &Value) -> Option<ModelRefusal> {
    let object = entry.as_object()?;
    for key in ["model", "at"] {
        if !object.get(key).is_some_and(Value::is_string) {
            return None;
        }
    }
    for key in ["detail", "resets"] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return None;
        }
    }
    serde_json::from_value(entry.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ModelRefusalReason;
    use serde_json::json;

    #[test]
    fn malformed_entries_and_unknown_reasons_are_dropped_not_fatal() {
        let record = json!({
            "model_refusals": [
                {"model": "a/one", "reason": "usage_limit", "at": "t1", "resets": "in 2 hours"},
                {"model": "a/two", "reason": "a_reason_from_a_newer_build", "at": "t2"},
                {"model": 7, "reason": "error", "at": "t3"},
                {"model": "a/three", "reason": "refused", "at": "t4", "detail": 5},
                "not an object",
                {"model": "a/four", "reason": "unavailable", "at": "t5", "detail": "overloaded"},
            ]
        });
        let refusals = parse_optional_model_refusals(&record).expect("two entries survive");
        assert_eq!(refusals.len(), 2);
        assert_eq!(refusals[0].model, "a/one");
        assert_eq!(refusals[0].reason, ModelRefusalReason::UsageLimit);
        assert_eq!(refusals[0].resets.as_deref(), Some("in 2 hours"));
        assert_eq!(refusals[1].model, "a/four");
        assert_eq!(refusals[1].detail.as_deref(), Some("overloaded"));
    }

    #[test]
    fn absent_or_fully_invalid_lists_read_as_none() {
        assert_eq!(parse_optional_model_refusals(&json!({})), None);
        assert_eq!(
            parse_optional_model_refusals(&json!({"model_refusals": "x"})),
            None
        );
        assert_eq!(
            parse_optional_model_refusals(&json!({"model_refusals": [{"model": "a/b"}]})),
            None
        );
    }
}
