pub use crate::utils::provider_diagnostic_carrier::{
    SIWC_ERRORS, siwc_error_for_code, siwc_recovery_from_diagnostic,
};
use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

static MISSING_RESPONSES_SCOPE: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"\bMissing scopes?:[^\n]*?\bapi\.responses\.write\b")
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("valid regex: {e}"))
});

pub const SIWC_MISSING_RESPONSES_SCOPE_GUIDANCE: &str = concat!(
    "This ChatGPT account or plan does not have access to the Responses API through Sign in with ChatGPT ",
    "(missing scope api.responses.write). Signing in again will not change that: check that plan usage is ",
    "enabled for this account or workspace, or use an OpenAI API key."
);

pub fn is_siwc_missing_responses_scope(text: &str) -> bool {
    MISSING_RESPONSES_SCOPE.is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_api_responses_write_in_single_and_multi_scope_errors() {
        assert!(is_siwc_missing_responses_scope(
            "You have insufficient permissions for this operation. Missing scopes: api.responses.write. Check that you have the correct role."
        ));
        assert!(is_siwc_missing_responses_scope(
            "Missing scopes: model.request, api.responses.write"
        ));
        assert!(!is_siwc_missing_responses_scope(
            "Missing scopes: api.completions.write"
        ));
    }
}
