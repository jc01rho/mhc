//! Upstream `request-options.ts`: the per-request timeout every direct skill MCP operation uses (#6631).
use serde_json::Value;
use std::time::Duration;

// The SDK arms setTimeout with this value; a delay above 2^31-1 ms is clamped to 1 ms.
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// The request timeout a skill MCP operation uses when the server configures none.
pub const DEFAULT_SKILL_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
