//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

use std::sync::atomic::AtomicU64;

/// Each tool call in these modules takes its own call id, so no two calls share a receipt key.
pub(crate) static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

/// Validates `mhc.session.open` args identically to `omo-nyang` `src/relay/mhc-ops.ts` (`openArgs` + strict `launchArgs`).
pub(crate) fn validate_open_args(args: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;

    const TOP_LEVEL: &[&str] = &["session", "fork_from", "launch", "bind"];
    const LAUNCH_KEYS: &[&str] = &[
        "cwd",
        "model",
        "thinking_level",
        "permission_preset",
        "name",
        "kind",
        "context",
        "auto_title",
        "attended",
        "prompt_surface",
        "browser_engine",
        "retry_fallback",
        "spawned_by",
        "env",
    ];

    let Some(obj) = args.as_object() else {
        return Err("invalid_request".to_owned());
    };
    for key in obj.keys() {
        if !TOP_LEVEL.contains(&key.as_str()) {
            return Err("invalid_request".to_owned());
        }
    }
    let check_ref = |key: &str| -> Result<bool, String> {
        match obj.get(key) {
            None => Ok(false),
            Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= 256 => Ok(true),
            _ => Err("invalid_request".to_owned()),
        }
    };
    let has_session = check_ref("session")?;
    let has_fork_from = check_ref("fork_from")?;
    if has_session && has_fork_from {
        return Err("invalid_request".to_owned());
    }
    if obj.get("bind").is_some_and(|bind| !bind.is_boolean()) {
        return Err("invalid_request".to_owned());
    }
    if let Some(launch_val) = obj.get("launch") {
        let Some(launch) = launch_val.as_object() else {
            return Err("invalid_request".to_owned());
        };
        for (key, value) in launch {
            if !LAUNCH_KEYS.contains(&key.as_str()) {
                return Err("invalid_request".to_owned());
            }
            match key.as_str() {
                "cwd" => {
                    if value.as_str().is_none_or(|s| !s.starts_with('/')) {
                        return Err("invalid_request".to_owned());
                    }
                }
                "model" => {
                    let valid = value.as_str().is_some_and(|s| {
                        let Some((provider, id)) = s.split_once('/') else {
                            return false;
                        };
                        !provider.is_empty()
                            && !provider.chars().any(char::is_whitespace)
                            && !id.is_empty()
                            && !id.chars().any(char::is_whitespace)
                    });
                    if !valid {
                        return Err("invalid_request".to_owned());
                    }
                }
                "name" => {
                    let valid = value
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.chars().count() <= 200);
                    if !valid {
                        return Err("invalid_request".to_owned());
                    }
                }
                "thinking_level" | "permission_preset" | "kind" | "prompt_surface"
                | "browser_engine" => {
                    if value.as_str().is_none_or(str::is_empty) {
                        return Err("invalid_request".to_owned());
                    }
                }
                "context" | "spawned_by" => {
                    if !value.is_object() {
                        return Err("invalid_request".to_owned());
                    }
                }
                "env" => {
                    let valid = value
                        .as_object()
                        .is_some_and(|env| env.values().all(|v| v.is_string()));
                    if !valid {
                        return Err("invalid_request".to_owned());
                    }
                }
                "auto_title" | "attended" | "retry_fallback" => {}
                _ => return Err("invalid_request".to_owned()),
            }
        }
    }
    Ok(())
}

#[path = "agentd_client_tests.rs"]
mod agentd_client_tests;
#[path = "gateway_receipt_tests.rs"]
mod gateway_receipt_tests;
#[path = "gateway_schema_tests.rs"]
mod gateway_schema_tests;
#[path = "gateway_store_tests.rs"]
mod gateway_store_tests;
#[path = "intake_tests.rs"]
mod intake_tests;
#[path = "native_thread_host_tests.rs"]
mod native_thread_host_tests;
#[path = "prompt_tests.rs"]
mod prompt_tests;
#[path = "remote_consumers_tests.rs"]
mod remote_consumers_tests;
#[path = "remote_list_and_close_tests.rs"]
mod remote_list_and_close_tests;
#[path = "remote_thread_tests.rs"]
mod remote_thread_tests;
#[path = "reply_scope_tests.rs"]
mod reply_scope_tests;
#[path = "rules_block_tests.rs"]
mod rules_block_tests;
#[path = "run_alerts_tests.rs"]
mod run_alerts_tests;
#[path = "same_device_wake_tests.rs"]
mod same_device_wake_tests;
#[path = "store_extension_tests.rs"]
mod store_extension_tests;
#[path = "thread_control_tests.rs"]
mod thread_control_tests;
#[path = "thread_create_tests.rs"]
mod thread_create_tests;
#[path = "tool_receipts_tests.rs"]
mod tool_receipts_tests;
#[path = "wake_carrier_tests.rs"]
mod wake_carrier_tests;
#[path = "wake_pending_sweep_tests.rs"]
mod wake_pending_sweep_tests;
#[path = "wake_runtime_tests.rs"]
mod wake_runtime_tests;
#[path = "wake_session_waker_tests.rs"]
mod wake_session_waker_tests;
#[path = "wake_store_tests.rs"]
mod wake_store_tests;
#[path = "wake_support.rs"]
mod wake_support;
#[path = "endpoint_registry_tests.rs"]
mod endpoint_registry_tests;
