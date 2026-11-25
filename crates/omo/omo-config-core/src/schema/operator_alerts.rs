use serde_json::{Map, Value};

use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, array, env_var_name_string, non_empty_string, number, optional,
    positive_integer, strict_object,
};

fn trim_threads(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };
    let mut trimmed: Map<String, Value> = Map::new();
    for (key, entry) in map {
        let next = match (key.as_str(), entry) {
            ("threads", Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(text) => Value::String(text.trim().to_string()),
                        other => other.clone(),
                    })
                    .collect(),
            ),
            _ => entry.clone(),
        };
        trimmed.insert(key.clone(), next);
    }
    Value::Object(trimmed)
}

fn omo_error_streak_settings_schema() -> Node {
    // webhook_url_env names an env var so webhook URLs never sit directly in config files.
    Node::Object(Box::new(ObjectSpec {
        fields: vec![
            optional("threshold", positive_integer()),
            optional("realert_minutes", number().with_min(0.0)),
            optional("threads", array(non_empty_string())),
            optional("webhook_url_env", env_var_name_string()),
        ],
        strict: true,
        preprocess: Some(trim_threads as PreprocessFn),
        refine: None,
    }))
}

pub fn omo_operator_alerts_settings_schema() -> Node {
    strict_object(vec![optional(
        "error_streak",
        omo_error_streak_settings_schema(),
    )])
}

const ERROR_STREAK_DEFAULT_THRESHOLD: u64 = 10;
const ERROR_STREAK_DEFAULT_REALERT_MINUTES: f64 = 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedErrorStreakSettings {
    pub threshold: u64,
    pub realert_ms: f64,
    pub threads: Vec<String>,
    pub webhook_url_env: Option<String>,
}

pub fn resolve_error_streak_settings(config: &Value) -> ResolvedErrorStreakSettings {
    let streak = config
        .get("operator_alerts")
        .and_then(|alerts| alerts.get("error_streak"));
    let threshold = streak
        .and_then(|streak| streak.get("threshold"))
        .and_then(Value::as_u64)
        .unwrap_or(ERROR_STREAK_DEFAULT_THRESHOLD);
    let realert_minutes = streak
        .and_then(|streak| streak.get("realert_minutes"))
        .and_then(Value::as_f64)
        .unwrap_or(ERROR_STREAK_DEFAULT_REALERT_MINUTES);
    let mut threads: Vec<String> = Vec::new();
    if let Some(items) = streak
        .and_then(|streak| streak.get("threads"))
        .and_then(Value::as_array)
    {
        for item in items.iter().filter_map(Value::as_str) {
            if !threads.iter().any(|seen| seen == item) {
                threads.push(item.to_string());
            }
        }
    }
    ResolvedErrorStreakSettings {
        threshold,
        realert_ms: realert_minutes * 60_000.0,
        threads,
        webhook_url_env: streak
            .and_then(|streak| streak.get("webhook_url_env"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::validate::safe_parse;
    use serde_json::json;

    #[test]
    fn no_table_resolves_to_ten_errors_hourly_and_no_target() {
        let resolved = resolve_error_streak_settings(&json!({}));
        assert_eq!(
            resolved,
            ResolvedErrorStreakSettings {
                threshold: 10,
                realert_ms: 60.0 * 60_000.0,
                threads: Vec::new(),
                webhook_url_env: None,
            }
        );
    }

    #[test]
    fn explicit_values_survive_and_a_repeated_thread_is_kept_once() {
        let config = safe_parse(
            &omo_operator_alerts_settings_schema(),
            &json!({
                "error_streak": {
                    "threshold": 3,
                    "realert_minutes": 0,
                    "threads": ["duty", "duty", "ops"],
                    "webhook_url_env": "ALERT_HOOK",
                }
            }),
        )
        .expect("valid operator_alerts block");
        let resolved = resolve_error_streak_settings(&json!({ "operator_alerts": config }));
        assert_eq!(resolved.threshold, 3);
        assert_eq!(resolved.realert_ms, 0.0);
        assert_eq!(
            resolved.threads,
            vec!["duty".to_string(), "ops".to_string()]
        );
        assert_eq!(resolved.webhook_url_env.as_deref(), Some("ALERT_HOOK"));
    }

    #[test]
    fn a_webhook_url_or_a_zero_threshold_is_refused() {
        assert!(
            safe_parse(
                &omo_operator_alerts_settings_schema(),
                &json!({ "error_streak": { "webhook_url_env": "https://hooks.example/x" } })
            )
            .is_err()
        );
        assert!(
            safe_parse(
                &omo_operator_alerts_settings_schema(),
                &json!({ "error_streak": { "threshold": 0 } })
            )
            .is_err()
        );
    }

    #[test]
    fn a_padded_thread_address_is_stored_trimmed_and_a_blank_one_is_refused() {
        let config = safe_parse(
            &omo_operator_alerts_settings_schema(),
            &json!({ "error_streak": { "threads": ["  duty  "] } }),
        )
        .expect("trimmed");
        assert_eq!(config["error_streak"]["threads"], json!(["duty"]));
        assert!(
            safe_parse(
                &omo_operator_alerts_settings_schema(),
                &json!({ "error_streak": { "threads": ["   "] } })
            )
            .is_err()
        );
    }

    #[test]
    fn an_unknown_key_inside_the_block_is_refused() {
        assert!(
            safe_parse(
                &omo_operator_alerts_settings_schema(),
                &json!({ "error_streak": { "webhook": "https://hooks.example/x" } })
            )
            .is_err()
        );
        assert!(
            safe_parse(
                &omo_operator_alerts_settings_schema(),
                &json!({ "error_streaks": { "threshold": 3 } })
            )
            .is_err()
        );
    }
}
