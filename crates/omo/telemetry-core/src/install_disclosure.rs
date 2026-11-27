//! Rust port of `packages/telemetry-core/src/install-disclosure.ts`.

use crate::destinations::telemetry_destinations_phrase;

pub const TELEMETRY_PRIVACY_POLICY_URL: &str =
    "https://github.com/code-yeongyu/oh-my-openagent/blob/dev/docs/legal/privacy-policy.md";

/// Printed once when an installer finishes. `install.sh` and `install.ps1` cannot import this module and carry
/// a copy; `tests/all/install_disclosure.rs` fails when a copy drifts.
pub fn telemetry_install_disclosure_lines() -> [String; 4] {
    [
        "OmO sends anonymous usage telemetry (no prompts, code, file paths, or account data) to improve the product."
            .to_string(),
        format!("The data goes to {}.", telemetry_destinations_phrase()),
        "Turn it off with DO_NOT_TRACK=1 (or OMO_DISABLE_TELEMETRY=1).".to_string(),
        format!("Details: {TELEMETRY_PRIVACY_POLICY_URL}"),
    ]
}
