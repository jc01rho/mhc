use super::*;

fn ledger() -> Value {
    serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/tmp/w","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/tmp/w/.git","gitFileSnapshot":"gitdir: /tmp/g","commonConfigPath":"/tmp/g/config","commonConfigSnapshot":null})
}
#[test]
fn preserves_unknown_fields_null_and_large_js_integer_pid() {
    let mut value = ledger();
    value["future"] = serde_json::json!({"x":1});
    value["pid"] = serde_json::json!(4_294_967_296u64);
    value["processStart"] = Value::Null;
    value["finalizeOutcome"] = "admin_tamper".into();
    value["queued"] = Value::Bool(true);
    let parsed = parse_reservation_run_ledger(value.clone()).unwrap();
    assert_eq!(parsed.value(), &value);
    assert_eq!(parsed.run_id(), "run");
}
#[test]
fn dream_requires_matching_trigger_origin_and_target() {
    let mut value = ledger();
    value["kind"] = "dream".into();
    assert_eq!(
        parse_reservation_run_ledger(value.clone()).unwrap_err().0,
        "Reservation run kind and trigger disagree"
    );
    value["trigger"] = "dream".into();
    assert_eq!(
        parse_reservation_run_ledger(value.clone()).unwrap_err().0,
        "Dream run origin is required"
    );
    value["origin"] = "idle".into();
    value["targetDoc"] = "reference/style.md".into();
    assert!(parse_reservation_run_ledger(value).is_ok());
}
#[test]
fn pressure_origin_and_dream_budget_metadata_are_validated() {
    let mut value = ledger();
    value["kind"] = "dream".into();
    value["trigger"] = "dream".into();
    value["origin"] = "pressure".into();
    value["systemTokenBudget"] = serde_json::json!(30000);
    value["systemTokenTarget"] = serde_json::json!(24000);
    assert!(parse_reservation_run_ledger(value.clone()).is_ok());
    value.as_object_mut().unwrap().remove("systemTokenTarget");
    assert_eq!(
        parse_reservation_run_ledger(value).unwrap_err().0,
        "Dream token budget metadata is incomplete"
    );
}
#[test]
fn invalid_fields_rejected() {
    for (key, value) in [
        ("version", Value::from(2)),
        ("deadlineAt", Value::from(106)),
        ("pid", Value::Null),
        ("attempt", Value::from(1.5)),
        ("conversationIds", serde_json::json!([1])),
        ("launching", Value::from("true")),
        ("queued", Value::from("yes")),
        ("commonConfigSnapshot", Value::Bool(false)),
        ("finalizeOutcome", Value::from("unknown")),
        ("cleanupIncomplete", Value::from("yes")),
        ("systemTokenBudget", Value::from(30000)),
    ] {
        let mut input = ledger();
        input[key] = value;
        assert!(parse_reservation_run_ledger(input).is_err(), "{key}");
    }
}
