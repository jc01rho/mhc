use omo_config_core::internal::validate::{Node, safe_parse};
use omo_config_core::{
    ResolvedErrorStreakSettings, omo_config_layer_schema, omo_config_profile_schema,
    omo_config_schema, omo_typed_harness_config_schema, resolve_error_streak_settings,
};
use serde_json::{Value, json};

fn parsed(node: &Node, value: Value) -> Value {
    match safe_parse(node, &value) {
        Ok(parsed) => parsed,
        Err(issues) => panic!("expected a valid document, got {issues:?}"),
    }
}

fn accepted(node: &Node, value: Value) -> bool {
    safe_parse(node, &value).is_ok()
}

#[test]
fn operator_alerts_defaults_to_ten_errors_hourly_and_no_target() {
    let config = parsed(&omo_config_schema(), json!({}));
    assert_eq!(
        resolve_error_streak_settings(&config),
        ResolvedErrorStreakSettings {
            threshold: 10,
            realert_ms: 60.0 * 60_000.0,
            threads: Vec::new(),
            webhook_url_env: None,
        }
    );
}

#[test]
fn operator_alerts_error_streak_survives_every_config_layer() {
    let operator_alerts = json!({
        "error_streak": {
            "threshold": 3,
            "realert_minutes": 0,
            "threads": ["duty", "duty", "ops"],
            "webhook_url_env": "ALERT_HOOK",
        }
    });
    for schema in [
        omo_config_schema(),
        omo_config_layer_schema(),
        omo_config_profile_schema(),
        omo_typed_harness_config_schema(),
    ] {
        let value = parsed(
            &schema,
            json!({ "operator_alerts": operator_alerts.clone() }),
        );
        assert_eq!(value["operator_alerts"], operator_alerts);
    }
    let config = parsed(
        &omo_config_schema(),
        json!({ "operator_alerts": operator_alerts }),
    );
    assert_eq!(
        resolve_error_streak_settings(&config),
        ResolvedErrorStreakSettings {
            threshold: 3,
            realert_ms: 0.0,
            threads: vec!["duty".to_string(), "ops".to_string()],
            webhook_url_env: Some("ALERT_HOOK".to_string()),
        }
    );
}

#[test]
fn operator_alerts_refuses_a_webhook_url_or_a_zero_threshold() {
    assert!(!accepted(
        &omo_config_schema(),
        json!({ "operator_alerts": { "error_streak": { "webhook_url_env": "https://hooks.example/x" } } }),
    ));
    assert!(!accepted(
        &omo_config_schema(),
        json!({ "operator_alerts": { "error_streak": { "threshold": 0 } } }),
    ));
}

#[test]
fn keyword_detector_survives_every_config_layer_and_rejects_unknown_keywords() {
    let keyword_detector = json!({
        "enabled_expansions": ["ultrawork", "hyperplan-ultrawork"],
        "disabled_keywords": ["team", "hyperplan"],
    });
    for schema in [
        omo_config_schema(),
        omo_config_layer_schema(),
        omo_config_profile_schema(),
        omo_typed_harness_config_schema(),
    ] {
        let value = parsed(
            &schema,
            json!({ "keyword_detector": keyword_detector.clone() }),
        );
        assert_eq!(value["keyword_detector"], keyword_detector);
    }
    assert!(!accepted(
        &omo_config_schema(),
        json!({ "keyword_detector": { "disabled_keywords": ["unknown"] } }),
    ));
}
