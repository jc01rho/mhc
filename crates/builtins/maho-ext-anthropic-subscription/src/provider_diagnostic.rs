//! The provider diagnostic of a refused Anthropic Subscription account pool (senpi `provider-diagnostic.ts`,
//! senpi#4672 and senpi#3975).
//!
//! Only the pool-refusal half of senpi's `anthropicSubscriptionDiagnostic` exists here: the diagnostic of a classified
//! SDK failure (auth, rate limit, overloaded, billing, organization, thinking-binding mismatch) is not ported.
use crate::affinity::AllAccountsBlockedError;
use maho_ai::types::{
    ProviderDiagnostic, ProviderDiagnosticCategory as Category,
    ProviderDiagnosticEvidence as Evidence, sanitize_provider_diagnostic,
};

fn structured(
    category: Category,
    code: &str,
    resets_at: Option<i64>,
) -> Option<ProviderDiagnostic> {
    sanitize_provider_diagnostic(&ProviderDiagnostic {
        category,
        http_status: None,
        code: Some(code.to_owned()),
        evidence: Evidence::StructuredCode,
        resets_at,
    })
}

/// A pool the extension refused locally is `auth` when every account waits on a new login, and `rate_limit` with the
/// soonest reset only when every account that can come back on its own is rate-limited (senpi#3975). A pool held by
/// billing, organization, or unknown blocks, and a pin to an account that is no longer configured, get no
/// diagnostic, so a goal never parks on them. The refusal is found as the error itself or as a cause it carries, so
/// wrapping never hides it; the result is revalidated against the closed vocabulary.
pub fn pool_refusal_diagnostic(error: &anyhow::Error) -> Option<ProviderDiagnostic> {
    let pool = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AllAccountsBlockedError>())?;
    if pool
        .unavailable_account
        .as_ref()
        .is_some_and(|account| !account.configured)
    {
        return None;
    }
    if pool.block_reason == Some("auth_error") {
        return structured(Category::Auth, "account_pool_auth_blocked", None);
    }
    let resets_at = pool.rate_limited_until?;
    structured(
        Category::RateLimit,
        "account_pool_blocked",
        Some(resets_at as i64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::affinity::UnavailableAccount;

    const SOONEST: f64 = 1_795_359_840_000.0;

    fn refusal(
        block_reason: Option<&'static str>,
        rate_limited_until: Option<f64>,
    ) -> AllAccountsBlockedError {
        AllAccountsBlockedError {
            soonest_unblock_at: Some(SOONEST),
            block_reason,
            unavailable_account: None,
            limited_model: None,
            rate_limited_until,
        }
    }

    #[test]
    fn a_pool_waiting_only_on_rate_limits_is_a_rate_limit_with_the_soonest_reset() {
        let diagnostic = pool_refusal_diagnostic(&anyhow::Error::new(refusal(None, Some(SOONEST))))
            .expect("diagnostic");
        assert_eq!(diagnostic.category, Category::RateLimit);
        assert_eq!(diagnostic.code.as_deref(), Some("account_pool_blocked"));
        assert_eq!(diagnostic.resets_at, Some(SOONEST as i64));
    }

    #[test]
    fn a_pool_where_every_account_needs_a_new_login_is_auth_without_a_reset() {
        let diagnostic =
            pool_refusal_diagnostic(&anyhow::Error::new(refusal(Some("auth_error"), None)))
                .expect("diagnostic");
        assert_eq!(diagnostic.category, Category::Auth);
        assert_eq!(
            diagnostic.code.as_deref(),
            Some("account_pool_auth_blocked")
        );
        assert_eq!(diagnostic.resets_at, None);
    }

    #[test]
    fn a_pool_held_by_billing_or_an_unknown_block_gets_no_diagnostic_so_a_goal_never_parks_on_it() {
        assert!(pool_refusal_diagnostic(&anyhow::Error::new(refusal(None, None))).is_none());
        assert!(
            pool_refusal_diagnostic(&anyhow::Error::new(refusal(Some("rate_limit"), None)))
                .is_none()
        );
    }

    #[test]
    fn a_pin_to_an_account_that_is_no_longer_configured_is_a_configuration_error_not_a_limit() {
        let mut pool = refusal(None, Some(SOONEST));
        pool.unavailable_account = Some(UnavailableAccount {
            name: "gone".into(),
            configured: false,
            pinned_by: None,
        });
        assert!(pool_refusal_diagnostic(&anyhow::Error::new(pool)).is_none());
    }

    #[test]
    fn the_refusal_is_found_as_the_cause_of_a_wrapping_failure() {
        let wrapped =
            anyhow::Error::new(refusal(None, Some(SOONEST))).context("while preparing the turn");
        let diagnostic = pool_refusal_diagnostic(&wrapped).expect("diagnostic");
        assert_eq!(diagnostic.code.as_deref(), Some("account_pool_blocked"));
        assert!(pool_refusal_diagnostic(&anyhow::anyhow!("connection reset")).is_none());
    }
}
