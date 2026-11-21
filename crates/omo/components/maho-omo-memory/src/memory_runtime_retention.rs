//! Port of `memory-runtime-retention.ts`: `memory.runtime` resolved for the identity this session
//! binds, with `agents.<id>.runtime` winning field by field.

use memory_core::retention::{
    DEFAULT_RUNTIME_MAX_BYTES, DEFAULT_RUNTIME_RETENTION, RuntimeRetentionPolicy,
};
use serde_json::Value;

pub use memory_core::retention::resolve_foreign_identity_discard_unreflected;

/// One key of a `runtime` layer, treating an explicit JSON `null` as absent so the layer below wins
/// (the TypeScript `override?.key ?? base.key`).
fn layer_value<'a>(layer: &'a Value, key: &str) -> Option<&'a Value> {
    layer.get(key).filter(|value| !value.is_null())
}

/// The value of `key` for one identity: `agents.<id>.runtime` wins over `runtime`.
fn field<'a>(settings: &'a Value, identity: &str, key: &str) -> Option<&'a Value> {
    let agent = settings
        .get("agents")
        .and_then(|agents| agents.get(identity))
        .and_then(|agent| agent.get("runtime"))
        .and_then(|runtime| layer_value(runtime, key));
    agent.or_else(|| {
        settings
            .get("runtime")
            .and_then(|runtime| layer_value(runtime, key))
    })
}

fn window(settings: &Value, identity: &str, key: &str, fallback: f64) -> f64 {
    field(settings, identity, key)
        .and_then(Value::as_f64)
        .unwrap_or(fallback)
}

/// `memory.runtime` for one identity this session binds: `agents.<id>.runtime` wins field by field.
///
/// `settings` is the resolved `memory` block (`resolve_memory_settings`), which the config schema has
/// already defaulted.
pub fn resolve_runtime_retention_policy(
    settings: &Value,
    identity: &str,
) -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        retention_days: window(
            settings,
            identity,
            "retention_days",
            DEFAULT_RUNTIME_RETENTION.retention_days,
        ),
        abandoned_journal_days: window(
            settings,
            identity,
            "abandoned_journal_days",
            DEFAULT_RUNTIME_RETENTION.abandoned_journal_days,
        ),
        discard_unreflected_journals: field(settings, identity, "discard_unreflected_journals")
            .and_then(Value::as_bool)
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.discard_unreflected_journals),
        completions_days: window(
            settings,
            identity,
            "completions_days",
            DEFAULT_RUNTIME_RETENTION.completions_days,
        ),
        pass_budget_ms: field(settings, identity, "pass_budget_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.pass_budget_ms),
    }
}

/// `memory.runtime.max_bytes` for one identity (`0` = unbounded): `agents.<id>.runtime` wins.
pub fn resolve_runtime_max_bytes(settings: &Value, identity: &str) -> u64 {
    field(settings, identity, "max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_RUNTIME_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Value {
        crate::reflection_settings::resolve_memory_settings(None).expect("memory defaults")
    }

    #[test]
    fn an_identity_without_an_override_gets_the_global_windows() {
        // Given
        let settings = defaults();
        // When
        let policy = resolve_runtime_retention_policy(&settings, "reviewer");
        // Then
        assert_eq!(policy, DEFAULT_RUNTIME_RETENTION);
        assert_eq!(
            resolve_runtime_max_bytes(&settings, "reviewer"),
            DEFAULT_RUNTIME_MAX_BYTES
        );
    }

    #[test]
    fn a_per_agent_override_wins_field_by_field() {
        // Given
        let settings = serde_json::json!({
            "runtime": { "retention_days": 3, "abandoned_journal_days": 5, "pass_budget_ms": 5000 },
            "agents": { "scout": { "runtime": { "retention_days": 30, "max_bytes": 1024 } } },
        });
        // When
        let scout = resolve_runtime_retention_policy(&settings, "scout");
        let builder = resolve_runtime_retention_policy(&settings, "builder");
        // Then
        assert_eq!(
            scout,
            RuntimeRetentionPolicy {
                retention_days: 30.0,
                abandoned_journal_days: 5.0,
                completions_days: 30.0,
                pass_budget_ms: 5000,
                discard_unreflected_journals: false,
            }
        );
        assert_eq!(builder.retention_days, 3.0);
        assert_eq!(resolve_runtime_max_bytes(&settings, "scout"), 1024);
        assert_eq!(
            resolve_runtime_max_bytes(&settings, "builder"),
            DEFAULT_RUNTIME_MAX_BYTES
        );
    }

    #[test]
    fn an_unbounded_cap_stays_zero_and_an_agent_may_switch_unreflected_discard_on() {
        // Given
        let settings = serde_json::json!({
            "runtime": { "max_bytes": 0, "discard_unreflected_journals": false },
            "agents": { "scout": { "runtime": { "discard_unreflected_journals": true } } },
        });
        // When
        let scout = resolve_runtime_retention_policy(&settings, "scout");
        let builder = resolve_runtime_retention_policy(&settings, "builder");
        // Then
        assert_eq!(resolve_runtime_max_bytes(&settings, "scout"), 0);
        assert!(
            scout.discard_unreflected_journals,
            "the agent layer switches deletion on for itself only"
        );
        assert!(!builder.discard_unreflected_journals);
    }

    #[test]
    fn an_explicit_null_override_falls_through_to_the_global_value() {
        // Given
        let settings = serde_json::json!({
            "runtime": { "retention_days": 3 },
            "agents": { "scout": { "runtime": { "retention_days": null } } },
        });
        // When
        let scout = resolve_runtime_retention_policy(&settings, "scout");
        // Then
        assert_eq!(scout.retention_days, 3.0);
    }
}
