//! Upstream `request-options.ts`: the per-request timeout every direct skill MCP operation uses (#6631).
use super::manager::SkillMcpClientOptions;
use maho_ext_mcp::request_origin::{McpRequestOptions, mcp_request_options};
use serde_json::Value;
use std::time::Duration;

// The SDK arms setTimeout with this value; a delay above 2^31-1 ms is clamped to 1 ms.
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// The request timeout a skill MCP operation uses when the server configures none.
pub const DEFAULT_SKILL_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) const SDK_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The server's configured request timeout in milliseconds, when it is usable: a finite
/// number in (0, 2^31-1]. Zero, negatives and larger values would arm an immediate or
/// never-firing timer, so they fall back to the default.
pub fn resolve_request_timeout_ms(config: &Value) -> Option<f64> {
    let timeout = config.get("timeout")?.as_f64()?;
    (timeout.is_finite() && timeout > 0.0 && timeout <= MAX_TIMER_DELAY_MS).then_some(timeout)
}

/// The request timeout for every direct MCP operation (the abort signal travels separately).
pub fn request_timeout(config: &Value) -> Duration {
    resolve_request_timeout_ms(config)
        .map(|ms| Duration::from_secs_f64(ms / 1000.0))
        .unwrap_or(DEFAULT_SKILL_MCP_REQUEST_TIMEOUT)
}

pub(super) fn build_request_options(
    config: &Value,
    options: Option<&SkillMcpClientOptions>,
) -> (Duration, McpRequestOptions) {
    let timeout = maho_ext_mcp::skill_server::skill_request_timeout_ms(config)
        .map(|ms| Duration::from_millis(ms.max(1.0) as u64))
        .unwrap_or(SDK_REQUEST_TIMEOUT);
    let signal = options.and_then(|options| options.signal.clone());
    (timeout, mcp_request_options(None, signal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_timeout_falls_back_without_arming_an_immediate_timer() {
        for invalid in [
            json!(0),
            json!(-1),
            json!(2_147_483_648_u64),
            json!("300"),
            Value::Null,
        ] {
            assert_eq!(
                build_request_options(&json!({"timeout":invalid}), None).0,
                SDK_REQUEST_TIMEOUT
            );
        }
    }

    #[test]
    fn configured_timeout_or_sdk_default() {
        for ms in [300, 120_000, i32::MAX] {
            assert_eq!(
                build_request_options(&json!({"timeout":ms}), None).0,
                Duration::from_millis(ms as u64)
            );
        }
        assert_eq!(
            build_request_options(&json!({"timeout":0.5}), None).0,
            Duration::from_millis(1)
        );
        assert_eq!(
            build_request_options(&json!({}), None).0,
            SDK_REQUEST_TIMEOUT
        );
    }

    #[test]
    fn timeout_does_not_replace_the_caller_signal() {
        let signal = maho_ext_api::AbortSignal::default();
        let options = SkillMcpClientOptions {
            signal: Some(signal.clone()),
            ..Default::default()
        };
        let (timeout, request) = build_request_options(&json!({"timeout":5000}), Some(&options));
        assert_eq!(timeout, Duration::from_secs(5));
        signal.abort();
        assert!(request.signal.expect("caller signal").is_aborted());
        assert!(build_request_options(&json!({}), None).1.signal.is_none());
    }
}
