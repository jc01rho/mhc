use omo_config_core::internal::validate::{Node, safe_parse};
use omo_config_core::{
    ResolvedSessionWakeSettings, is_omo_device_service_enabled, omo_agent_def_schema,
    omo_category_config_schema, omo_computer_settings_schema, omo_config_layer_schema,
    omo_config_profile_schema, omo_config_schema, omo_memory_settings_layer_schema,
    omo_memory_settings_schema, omo_typed_harness_config_schema, resolve_session_wake_settings,
};
use serde_json::{Value, json};

const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;

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
fn session_wake_defaults_to_the_same_device_wake_with_no_limit() {
    let config = parsed(&omo_config_schema(), json!({}));
    assert_eq!(
        resolve_session_wake_settings(&config),
        ResolvedSessionWakeSettings {
            same_device: true,
            wake_limit: 0,
        }
    );
}

#[test]
fn session_wake_limit_alone_keeps_the_same_device_wake_on() {
    let config = parsed(
        &omo_config_schema(),
        json!({ "session_wake": { "wake_limit": 3 } }),
    );
    assert_eq!(
        resolve_session_wake_settings(&config),
        ResolvedSessionWakeSettings {
            same_device: true,
            wake_limit: 3,
        }
    );
}

#[test]
fn session_wake_explicit_same_device_false_turns_the_wake_off() {
    let config = parsed(
        &omo_config_schema(),
        json!({ "session_wake": { "same_device": false } }),
    );
    assert!(!resolve_session_wake_settings(&config).same_device);
}

#[test]
fn session_wake_survives_every_config_layer() {
    let session_wake = json!({ "same_device": true, "wake_limit": 2 });
    for schema in [
        omo_config_schema(),
        omo_config_layer_schema(),
        omo_config_profile_schema(),
        omo_typed_harness_config_schema(),
    ] {
        let value = parsed(&schema, json!({ "session_wake": session_wake.clone() }));
        assert_eq!(value["session_wake"], session_wake);
    }
    let config = parsed(
        &omo_config_schema(),
        json!({ "session_wake": session_wake.clone() }),
    );
    assert_eq!(
        resolve_session_wake_settings(&config),
        ResolvedSessionWakeSettings {
            same_device: true,
            wake_limit: 2,
        }
    );
}

#[test]
fn session_wake_rejects_a_negative_or_fractional_limit_and_unknown_keys() {
    for session_wake in [
        json!({ "wake_limit": -1 }),
        json!({ "wake_limit": 1.5 }),
        json!({ "same_device": true, "other": 1 }),
    ] {
        assert!(
            !accepted(
                &omo_config_schema(),
                json!({ "session_wake": session_wake.clone() })
            ),
            "{session_wake}"
        );
    }
}

#[test]
fn device_service_is_on_without_a_section() {
    let config = parsed(&omo_config_schema(), json!({}));
    assert!(is_omo_device_service_enabled(&config));
}

#[test]
fn device_service_enabled_false_turns_the_bootstrap_off() {
    let config = parsed(
        &omo_config_schema(),
        json!({ "device_service": { "enabled": false } }),
    );
    assert!(!is_omo_device_service_enabled(&config));
}

#[test]
fn device_service_empty_section_defaults_enabled_on_the_root_only() {
    let root = parsed(&omo_config_schema(), json!({ "device_service": {} }));
    assert_eq!(root["device_service"], json!({ "enabled": true }));
    let layer = parsed(&omo_config_layer_schema(), json!({ "device_service": {} }));
    assert_eq!(layer["device_service"], json!({}));
}

#[test]
fn device_service_is_accepted_in_harness_blocks_and_profiles_and_rejects_strays() {
    assert!(accepted(
        &omo_config_schema(),
        json!({ "[senpi]": { "device_service": { "enabled": false } } })
    ));
    assert!(accepted(
        &omo_config_schema(),
        json!({ "profiles": { "work": { "device_service": { "enabled": false } } } })
    ));
    assert!(!accepted(
        &omo_config_schema(),
        json!({ "device_service": { "enabled": "no" } })
    ));
    assert!(!accepted(
        &omo_config_schema(),
        json!({ "device_service": { "install": false } })
    ));
}

#[test]
fn category_drops_a_retired_provider_lock() {
    for value in [true, false] {
        let input = json!({ "model": "openai/gpt-6-astra", "provider_lock": value });
        assert_eq!(
            parsed(&omo_category_config_schema(), input),
            json!({ "model": "openai/gpt-6-astra" })
        );
    }
}

#[test]
fn categories_and_agents_accept_only_the_two_models_distribution_policies() {
    for schema in [omo_category_config_schema(), omo_agent_def_schema()] {
        for policy in ["sequential", "round_robin"] {
            let value = parsed(
                &schema,
                json!({ "models": ["openai/gpt-6-astra", "anthropic/claude-opus-5-5"], "models_distribution": policy }),
            );
            assert_eq!(value["models_distribution"], policy);
        }
        assert!(!accepted(
            &schema,
            json!({ "models_distribution": "random" })
        ));
        assert!(!accepted(&schema, json!({ "models_distribution": true })));
    }
}

#[test]
fn computer_reserved_keys_accept_only_their_own_values() {
    for block in [
        json!({ "run_sandbox": "wasm" }),
        json!({ "intervention": "stop" }),
        json!({ "macos_key_priming": true }),
        json!({ "trace": "yes" }),
    ] {
        assert!(
            !accepted(&omo_computer_settings_schema(), block.clone()),
            "{block}"
        );
    }
}

#[test]
fn computer_show_cursor_off_survives_the_root_config() {
    let value = parsed(
        &omo_config_schema(),
        json!({ "computer": { "showCursor": false } }),
    );
    assert_eq!(value["computer"], json!({ "showCursor": false }));
}

fn runtime_defaults() -> Value {
    json!({
        "retention_days": 7,
        "abandoned_journal_days": 14,
        "completions_days": 30,
        "pass_budget_ms": 60000,
        "discard_unreflected_journals": false,
        "max_bytes": TWO_GIB,
    })
}

#[test]
fn memory_runtime_applies_the_documented_retention_defaults() {
    let value = parsed(&omo_memory_settings_schema(), json!({}));
    assert_eq!(value["runtime"], runtime_defaults());
    let value = parsed(
        &omo_memory_settings_schema(),
        json!({ "runtime": { "discard_unreflected_journals": true } }),
    );
    assert_eq!(value["runtime"]["discard_unreflected_journals"], true);
}

#[test]
fn memory_runtime_keeps_explicit_windows_on_the_block_the_layer_and_the_agent() {
    let value = parsed(
        &omo_memory_settings_schema(),
        json!({
            "runtime": { "retention_days": 3, "abandoned_journal_days": 5, "pass_budget_ms": 5000 },
            "agents": { "agent-a": { "runtime": { "retention_days": 30, "pass_budget_ms": 1000 } } },
        }),
    );
    assert_eq!(
        value["runtime"],
        json!({
            "retention_days": 3,
            "abandoned_journal_days": 5,
            "completions_days": 30,
            "pass_budget_ms": 5000,
            "discard_unreflected_journals": false,
            "max_bytes": TWO_GIB,
        })
    );
    assert_eq!(
        value["agents"]["agent-a"]["runtime"],
        json!({ "retention_days": 30, "pass_budget_ms": 1000 })
    );
    let agent_only = parsed(
        &omo_memory_settings_schema(),
        json!({ "agents": { "b": { "runtime": { "discard_unreflected_journals": true } } } }),
    );
    assert_eq!(
        agent_only["agents"]["b"]["runtime"],
        json!({ "discard_unreflected_journals": true })
    );
    let layer = parsed(
        &omo_memory_settings_layer_schema(),
        json!({ "runtime": { "completions_days": 60 } }),
    );
    assert_eq!(layer["runtime"], json!({ "completions_days": 60 }));
}

#[test]
fn memory_runtime_rejects_unknown_keys_and_non_integer_windows() {
    for runtime in [
        json!({ "keep_forever": true }),
        json!({ "retention_days": 1.5 }),
        json!({ "abandoned_journal_days": 0 }),
        json!({ "discard_unreflected_journals": "yes" }),
        json!({ "pass_budget_ms": 0 }),
        json!({ "pass_budget_ms": 2.5 }),
    ] {
        assert!(
            !accepted(
                &omo_memory_settings_schema(),
                json!({ "runtime": runtime.clone() })
            ),
            "{runtime}"
        );
    }
    assert!(!accepted(
        &omo_memory_settings_layer_schema(),
        json!({ "runtime": { "completions_days": "30" } })
    ));
    assert!(!accepted(
        &omo_memory_settings_schema(),
        json!({ "agents": { "a": { "runtime": { "retention_days": -1 } } } })
    ));
}

#[test]
fn memory_runtime_caps_each_identity_at_two_gib_and_keeps_zero_and_agent_caps() {
    let value = parsed(&omo_memory_settings_schema(), json!({}));
    assert_eq!(value["runtime"]["max_bytes"], TWO_GIB);
    let value = parsed(
        &omo_memory_settings_schema(),
        json!({ "runtime": { "max_bytes": 0 }, "agents": { "agent-a": { "runtime": { "max_bytes": 1024 } } } }),
    );
    assert_eq!(value["runtime"]["max_bytes"], 0);
    assert_eq!(
        value["agents"]["agent-a"]["runtime"],
        json!({ "max_bytes": 1024 })
    );
    let layer = parsed(
        &omo_memory_settings_layer_schema(),
        json!({ "runtime": { "max_bytes": 0 } }),
    );
    assert_eq!(layer["runtime"], json!({ "max_bytes": 0 }));
}

#[test]
fn memory_runtime_rejects_a_negative_or_fractional_cap() {
    assert!(!accepted(
        &omo_memory_settings_schema(),
        json!({ "runtime": { "max_bytes": -1 } })
    ));
    assert!(!accepted(
        &omo_memory_settings_schema(),
        json!({ "runtime": { "max_bytes": 1.5 } })
    ));
    assert!(!accepted(
        &omo_memory_settings_layer_schema(),
        json!({ "agents": { "a": { "runtime": { "max_bytes": "2GiB" } } } })
    ));
}

#[test]
fn memory_runtime_keeps_unreflected_journal_discard_off_unless_an_agent_turns_it_on() {
    let value = parsed(&omo_memory_settings_schema(), json!({}));
    assert_eq!(value["runtime"]["discard_unreflected_journals"], false);
    let value = parsed(
        &omo_memory_settings_schema(),
        json!({
            "runtime": { "discard_unreflected_journals": false },
            "agents": { "a": { "runtime": { "discard_unreflected_journals": true } } },
        }),
    );
    assert_eq!(value["runtime"]["discard_unreflected_journals"], false);
    assert_eq!(
        value["agents"]["a"]["runtime"],
        json!({ "discard_unreflected_journals": true })
    );
}
