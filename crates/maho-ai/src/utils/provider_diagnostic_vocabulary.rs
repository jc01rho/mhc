use crate::types::{
    ProviderDiagnostic, ProviderDiagnosticCategory as Category,
    ProviderDiagnosticEvidence as Evidence,
};

pub use crate::types::PROVIDER_DIAGNOSTIC_MAX_BYTES;

/// senpi's own token for a credential pool whose every account waits on a rate or usage limit (senpi#3975).
pub const CREDENTIAL_POOL_BLOCKED_CODE: &str = "credential_pool_blocked";

/// A credential pool whose every account is limited until `resets_at`, the soonest one being due back.
pub fn credential_pool_blocked_diagnostic(resets_at: i64) -> Option<ProviderDiagnostic> {
    crate::types::sanitize_provider_diagnostic(&ProviderDiagnostic {
        category: Category::RateLimit,
        http_status: None,
        code: Some(CREDENTIAL_POOL_BLOCKED_CODE.to_owned()),
        evidence: Evidence::StructuredCode,
        resets_at: Some(resets_at),
    })
}

pub fn category_for_provider_code(code: &str) -> Option<Category> {
    Some(match code {
        "authentication_error" | "invalid_api_key" | "account_pool_auth_blocked" => Category::Auth,
        "rate_limit_error"
        | "rate_limit_exceeded"
        | "account_pool_blocked"
        | "credential_pool_blocked" => Category::RateLimit,
        "insufficient_quota"
        | "exceeded_current_quota_error"
        | "usage_limit_reached"
        | "usage_not_included" => Category::Quota,
        "context_length_exceeded" => Category::ContextLimit,
        "invalid_request_error" => Category::InvalidRequest,
        "overloaded_error" | "api_error" | "timeout_error" => Category::ProviderUnavailable,
        "billing_error" | "permission_error" | "not_found_error" => Category::Unknown,
        _ => return None,
    })
}

/// A provider error type outside the allowlist that is still safe to show: lowercase words joined
/// by `_` or `.`. It only ever names an `unknown` failure, with no status beside it.
pub fn is_opaque_provider_code(code: &str) -> bool {
    if code.len() > 64 || category_for_provider_code(code).is_some() {
        return false;
    }
    code.split(['_', '.'])
        .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()))
}

/// A reset instant (epoch ms) a limit diagnostic may carry: an integer between 2000 and 2100.
pub fn is_provider_diagnostic_resets_at(value: &serde_json::Value) -> bool {
    value
        .as_i64()
        .is_some_and(|v| v > 946_684_800_000 && v < 4_102_444_800_000)
}

pub fn category_for_transport_code(code: &str) -> Option<Category> {
    let diagnostic = ProviderDiagnostic {
        category: Category::Network,
        http_status: None,
        code: Some(code.to_owned()),
        evidence: Evidence::StructuredTransport,
        resets_at: None,
    };
    crate::types::sanitize_provider_diagnostic(&diagnostic).map(|value| value.category)
}

pub fn category_for_provider_status(status: u16) -> Category {
    match status {
        401 => Category::Auth,
        400 => Category::InvalidRequest,
        500..=599 => Category::ProviderUnavailable,
        _ => Category::Unknown,
    }
}

pub fn is_provider_code_status_compatible(code: &str, status: u16) -> bool {
    let Some(category) = category_for_provider_code(code) else {
        return false;
    };
    crate::types::sanitize_provider_diagnostic(&ProviderDiagnostic {
        category,
        http_status: Some(status),
        code: Some(code.to_owned()),
        evidence: Evidence::StructuredCode,
        resets_at: None,
    })
    .is_some()
}

pub struct ProviderFailureFacts<'a> {
    pub status: Option<&'a serde_json::Value>,
    pub code: Option<&'a str>,
}

/// senpi#4950: `error.type` values an OpenAI-compatible server uses for exactly one failure, so
/// they stand in for a missing `error.code`. Coarse buckets such as `invalid_request_error` never do.
pub const OPENAI_COMPATIBLE_PRECISE_TYPES: &[&str] =
    &["exceeded_current_quota_error", "insufficient_quota"];

/// The token to classify: the precise `error.code` when present, else a precise `error.type` from
/// [`OPENAI_COMPATIBLE_PRECISE_TYPES`], else `None`.
pub fn precise_provider_token<'a>(code: Option<&'a str>, kind: Option<&'a str>) -> Option<&'a str> {
    match code {
        Some(code) => Some(code),
        None => kind.filter(|value| OPENAI_COMPATIBLE_PRECISE_TYPES.contains(value)),
    }
}

pub fn classify_provider_failure(facts: ProviderFailureFacts<'_>) -> Option<ProviderDiagnostic> {
    let status = match facts.status {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let status = u16::try_from(value.as_u64()?).ok()?;
            if !(400..=599).contains(&status) {
                return None;
            }
            Some(status)
        }
    };
    let diagnostic = match facts.code.and_then(category_for_provider_code) {
        Some(category) => ProviderDiagnostic {
            category,
            http_status: status,
            code: facts.code.map(str::to_owned),
            evidence: Evidence::StructuredCode,
            resets_at: None,
        },
        None => ProviderDiagnostic {
            category: category_for_provider_status(status?),
            http_status: status,
            code: None,
            evidence: Evidence::StructuredStatus,
            resets_at: None,
        },
    };
    crate::types::sanitize_provider_diagnostic(&diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_code_falls_back_to_status_without_retaining_raw_code() {
        let status = json!(429);
        let result = classify_provider_failure(ProviderFailureFacts {
            status: Some(&status),
            code: Some("secret-provider-text"),
        })
        .expect("status classification");
        assert_eq!(result.category, Category::Unknown);
        assert_eq!(result.code, None);
        assert_eq!(result.evidence, Evidence::StructuredStatus);
    }

    #[test]
    fn invalid_status_prevents_code_classification() {
        let status = json!(401.5);
        let result = classify_provider_failure(ProviderFailureFacts {
            status: Some(&status),
            code: Some("invalid_api_key"),
        });
        assert!(result.is_none());
    }

    #[test]
    fn a_blocked_pool_diagnostic_is_a_rate_limit_with_the_soonest_reset() {
        let resets_at = 1_795_359_840_000;
        let diagnostic = credential_pool_blocked_diagnostic(resets_at).expect("diagnostic");
        assert_eq!(diagnostic.category, Category::RateLimit);
        assert_eq!(
            diagnostic.code.as_deref(),
            Some(CREDENTIAL_POOL_BLOCKED_CODE)
        );
        assert_eq!(diagnostic.http_status, None);
        assert_eq!(diagnostic.resets_at, Some(resets_at));
        assert!(is_provider_code_status_compatible(
            CREDENTIAL_POOL_BLOCKED_CODE,
            429
        ));
    }

    /// senpi#4950: a precise `error.type` stands in for a missing `error.code`, a coarse bucket
    /// never does, and an explicit code always wins over the type.
    #[test]
    fn precise_error_type_stands_in_for_a_missing_code() {
        let status = json!(429);
        assert_eq!(
            precise_provider_token(None, Some("exceeded_current_quota_error")),
            Some("exceeded_current_quota_error")
        );
        assert_eq!(
            precise_provider_token(None, Some("insufficient_quota")),
            Some("insufficient_quota")
        );
        assert_eq!(
            precise_provider_token(None, Some("invalid_request_error")),
            None
        );
        assert_eq!(
            precise_provider_token(Some("rate_limit_exceeded"), Some("insufficient_quota")),
            Some("rate_limit_exceeded")
        );
        let classified = classify_provider_failure(ProviderFailureFacts {
            status: Some(&status),
            code: precise_provider_token(None, Some("exceeded_current_quota_error")),
        })
        .expect("quota classification");
        assert_eq!(classified.category, Category::Quota);
        assert!(is_provider_code_status_compatible(
            "exceeded_current_quota_error",
            429
        ));
        assert!(!is_provider_code_status_compatible(
            "exceeded_current_quota_error",
            400
        ));
    }
}
