//! Port of senpi `packages/coding-agent/src/core/extensions/secret-input.ts` (senpi#5120).

use std::fmt;

pub const DEFAULT_SECRET_INPUT_TIMEOUT_MS: u64 = 30 * 60 * 1000;
pub const SECRET_INPUT_UNAVAILABLE_MESSAGE: &str =
    "This client cannot collect a secret; use the TUI or the host's settings route.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretInputUnavailableReason {
    Headless,
    ClientUnsupported,
    NoPresenter,
}

impl SecretInputUnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::ClientUnsupported => "client_unsupported",
            Self::NoPresenter => "no_presenter",
        }
    }
}

/// Fixed guidance only: errors from this boundary never carry the input or an underlying cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretInputUnavailableError {
    pub code: &'static str,
    pub reason: SecretInputUnavailableReason,
}

impl SecretInputUnavailableError {
    pub fn new(reason: SecretInputUnavailableReason) -> Self {
        Self {
            code: "secret_input_unavailable",
            reason,
        }
    }
}

impl fmt::Display for SecretInputUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SECRET_INPUT_UNAVAILABLE_MESSAGE)
    }
}

impl std::error::Error for SecretInputUnavailableError {}

pub fn resolve_secret_input_timeout(timeout_ms: Option<u64>) -> Result<u64, &'static str> {
    match timeout_ms {
        None => Ok(DEFAULT_SECRET_INPUT_TIMEOUT_MS),
        Some(0) => Err("Invalid secret input timeout"),
        Some(ms) => Ok(ms),
    }
}

/// A secret is never normalized: unrepresentable single-line input is refused instead.
pub fn is_secret_input_value(value: &str) -> bool {
    !value.chars().any(|ch| {
        let code = ch as u32;
        code <= 0x1f || (0x7f..=0x9f).contains(&code) || code == 0x2028 || code == 0x2029
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_and_validates_secret_timeout() {
        assert_eq!(
            resolve_secret_input_timeout(None),
            Ok(DEFAULT_SECRET_INPUT_TIMEOUT_MS)
        );
        assert_eq!(resolve_secret_input_timeout(Some(1_500)), Ok(1_500));
        assert!(resolve_secret_input_timeout(Some(0)).is_err());
    }

    #[test]
    fn refuses_control_and_line_separator_characters() {
        assert!(is_secret_input_value(""));
        assert!(is_secret_input_value("  padded secret \u{00e9}  "));
        assert!(!is_secret_input_value("bad\nvalue"));
        assert!(!is_secret_input_value("bad\rvalue"));
        assert!(!is_secret_input_value("bad\tvalue"));
        assert!(!is_secret_input_value("bad\x7fvalue"));
        assert!(!is_secret_input_value("bad\u{2028}value"));
        assert!(!is_secret_input_value("bad\u{2029}value"));
    }

    #[test]
    fn error_carries_fixed_guidance_only() {
        let err = SecretInputUnavailableError::new(SecretInputUnavailableReason::Headless);
        assert_eq!(err.code, "secret_input_unavailable");
        assert_eq!(err.reason.as_str(), "headless");
        assert_eq!(err.to_string(), SECRET_INPUT_UNAVAILABLE_MESSAGE);
    }
}
