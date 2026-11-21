//! `manager/model-refusals.ts` (omo #12010): under the designated-chain-only policy
//! (`task.parent_model_fallback: false`) a child never runs outside its designated models, so each
//! designated model that refuses the task is recorded, and a child that spends its whole chain on a
//! recognized refusal fails typed (`model_chain_exhausted`) listing every model it tried and why.

use std::sync::LazyLock;

use regex::Regex;

use crate::manager::credential_failure::{
    is_credential_failure, runtime_fallback_candidates, terminal_failure_message, usage_limit_scope,
};
use crate::manager::parent_model_fallback::base_selector;
use crate::state::{ModelRefusal, ModelRefusalReason, ParentModelFallbackReason, TaskRecord};

pub const MODEL_CHAIN_EXHAUSTED: &str = "model_chain_exhausted";

// The provider declined the request itself (a content or usage policy), not the account or its capacity.
static POLICY_REFUSAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bflagged\b|content[_ ]?(?:policy|filter|management)|usage[_ ]policy|safety (?:system|policy)|\brefus(?:ed|al)\b|\bviolat(?:es|ed|ion)\b")
        .expect("policy refusal pattern")
});
// Capacity: an overload, an outage, a timeout, a 5xx, or a rate limit that is not a spent quota.
static UNAVAILABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)overload|\b(?:500|502|503|504|529)\b|unavailable|capacity|timed? ?out|rate[_ ]limit|too many requests")
        .expect("unavailable pattern")
});

fn reason_label(reason: ModelRefusalReason) -> &'static str {
    match reason {
        ModelRefusalReason::UsageLimit => "usage limit",
        ModelRefusalReason::NotSignedIn => "not signed in",
        ModelRefusalReason::Refused => "refused",
        ModelRefusalReason::Unavailable => "overloaded or unavailable",
        ModelRefusalReason::Error => "failed",
    }
}

pub fn reason_of_failure(message: &str, model_id: Option<&str>) -> ModelRefusalReason {
    if usage_limit_scope(message).is_some() {
        ModelRefusalReason::UsageLimit
    } else if is_credential_failure(message, model_id) {
        ModelRefusalReason::NotSignedIn
    } else if POLICY_REFUSAL.is_match(message) {
        ModelRefusalReason::Refused
    } else if UNAVAILABLE.is_match(message) {
        ModelRefusalReason::Unavailable
    } else {
        ModelRefusalReason::Error
    }
}

/// The failure itself outranks the engine's hop label: an account limit the engine retried through
/// its transient lane still reads "usage limit".
pub fn reason_of_engine_fallback(
    limited: bool,
    engine_reason: Option<&str>,
    detail: Option<&str>,
) -> ModelRefusalReason {
    if limited {
        return ModelRefusalReason::UsageLimit;
    }
    if let Some(detail) = detail {
        let reason = reason_of_failure(detail, None);
        if reason != ModelRefusalReason::Error {
            return reason;
        }
    }
    match engine_reason {
        Some("billing") => ModelRefusalReason::UsageLimit,
        Some("refusal") => ModelRefusalReason::Refused,
        Some("transient") => ModelRefusalReason::Unavailable,
        _ => ModelRefusalReason::Error,
    }
}

pub fn build_model_refusal(
    model: &str,
    reason: ModelRefusalReason,
    message: Option<&str>,
    at: String,
) -> ModelRefusal {
    let excerpt = crate::manager::parent_model_fallback::build(
        String::new(),
        ParentModelFallbackReason::Error,
        message,
        &[],
        at.clone(),
    );
    ModelRefusal {
        model: base_selector(model).to_string(),
        reason,
        detail: excerpt.detail,
        resets: excerpt.resets,
        at,
    }
}

pub fn refusal_of(record: &TaskRecord, message: &str, at: String) -> ModelRefusal {
    let model_id = record
        .resolved_model
        .as_ref()
        .map(|model| model.model_id.as_str());
    build_model_refusal(
        &record.model,
        reason_of_failure(message, model_id),
        Some(message),
        at,
    )
}

pub fn with_refusal(record: &TaskRecord, refusal: ModelRefusal) -> Vec<ModelRefusal> {
    let mut refusals = record.model_refusals.clone().unwrap_or_default();
    refusals.push(refusal);
    refusals
}

/// The terminal error of a failed run: when the designated chain is spent on a recognized refusal the
/// task fails typed and lists every designated model it tried and why.
pub fn terminal_failure_fields(
    record: &TaskRecord,
    failure_message: &str,
    model_failure: bool,
) -> (String, Option<&'static str>) {
    let message = terminal_failure_message(Some(record), failure_message);
    let last = refusal_of(record, failure_message, record.updated_at.clone());
    if !model_failure
        || last.reason == ModelRefusalReason::Error
        || !runtime_fallback_candidates(record, failure_message)
            .remaining
            .is_empty()
    {
        return (message, None);
    }
    let refusals = with_refusal(record, last);
    (
        format!("{message}\n{}", describe_exhausted_chain(record, &refusals)),
        Some(MODEL_CHAIN_EXHAUSTED),
    )
}

pub fn describe_exhausted_chain(record: &TaskRecord, refusals: &[ModelRefusal]) -> String {
    let subject = match (&record.category, &record.agent_type) {
        (Some(category), _) => format!("category \"{category}\""),
        (None, Some(agent)) => format!("agent \"{agent}\""),
        (None, None) => "this task".to_string(),
    };
    let mut lines = vec![format!(
        "Every designated model for {subject} was refused, and a child never runs on any other model:"
    )];
    lines.extend(refusals.iter().map(|refusal| {
        format!(
            "- {}: {}{}",
            refusal.model,
            reason_label(refusal.reason),
            refusal_suffix(refusal)
        )
    }));
    lines.join("\n")
}

fn refusal_suffix(refusal: &ModelRefusal) -> String {
    if let Some(resets) = &refusal.resets {
        return format!(" (resets {resets})");
    }
    match (&refusal.detail, refusal.reason) {
        (Some(detail), reason) if reason != ModelRefusalReason::UsageLimit => {
            format!(" ({detail})")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ResolvedModelRecord, ResolvedModelSource};

    fn record(model: &str, fallbacks: &[&str]) -> TaskRecord {
        let (provider, model_id) = model.split_once('/').expect("selector");
        let mut record = crate::test_support::base_record("st_refusals", "parent-1");
        record.model = model.to_string();
        record.category = Some("deep-high".to_string());
        record.resolved_model = Some(ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            provider,
            model_id,
        ));
        record.fallback_models = Some(
            fallbacks
                .iter()
                .map(|selector| {
                    let (provider, model_id) = selector.split_once('/').expect("selector");
                    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
                })
                .collect(),
        );
        record.updated_at = "2026-11-21T00:00:00.000Z".to_string();
        record
    }

    #[test]
    fn refusal_reasons_classify_quota_auth_policy_and_capacity() {
        for (message, expected) in [
            (
                "You have hit your weekly limit (resets 3:20am)",
                ModelRefusalReason::UsageLimit,
            ),
            ("401 unauthorized", ModelRefusalReason::NotSignedIn),
            (
                "Your request was flagged by our content policy",
                ModelRefusalReason::Refused,
            ),
            ("529 overloaded_error", ModelRefusalReason::Unavailable),
            ("something odd happened", ModelRefusalReason::Error),
        ] {
            assert_eq!(reason_of_failure(message, None), expected, "{message}");
        }
        assert_eq!(
            reason_of_engine_fallback(false, Some("transient"), Some("weekly limit exhausted")),
            ModelRefusalReason::UsageLimit
        );
        assert_eq!(
            reason_of_engine_fallback(true, Some("transient"), None),
            ModelRefusalReason::UsageLimit
        );
        assert_eq!(
            reason_of_engine_fallback(false, Some("refusal"), Some("odd")),
            ModelRefusalReason::Refused
        );
    }

    // designated-models-only.test.ts: the only designated model hits its usage limit.
    #[test]
    fn a_spent_single_model_chain_fails_typed_naming_the_model_and_its_reset() {
        let record = record("anthropic/primary", &[]);
        let (message, kind) = terminal_failure_fields(
            &record,
            "You have hit your weekly limit (resets 3:20am)",
            true,
        );
        assert_eq!(kind, Some(MODEL_CHAIN_EXHAUSTED));
        assert!(message.contains(
            "Every designated model for category \"deep-high\" was refused, and a child never runs on any other model:"
        ));
        assert!(message.contains("- anthropic/primary: usage limit (resets 3:20am)"));
    }

    #[test]
    fn every_walked_model_is_listed_with_its_reason() {
        let mut record = record("alt/rung", &[]);
        record.model_refusals = Some(vec![build_model_refusal(
            "anthropic/primary:high",
            ModelRefusalReason::Refused,
            Some("Your request was flagged by our content policy"),
            "t0".to_string(),
        )]);
        let (message, kind) = terminal_failure_fields(&record, "401 unauthorized", true);
        assert_eq!(kind, Some(MODEL_CHAIN_EXHAUSTED));
        assert!(message.contains(
            "- anthropic/primary: refused (Your request was flagged by our content policy)"
        ));
        assert!(message.contains("- alt/rung: not signed in (401 unauthorized)"));
    }

    #[test]
    fn a_plain_error_a_tool_run_or_a_live_rung_is_not_an_exhausted_chain() {
        let single = record("anthropic/primary", &[]);
        assert_eq!(
            terminal_failure_fields(&single, "something odd happened", true).1,
            None
        );
        assert_eq!(
            terminal_failure_fields(&single, "401 unauthorized", false).1,
            None
        );
        let chained = record("anthropic/primary", &["alt/rung"]);
        assert_eq!(
            terminal_failure_fields(&chained, "529 overloaded_error", true).1,
            None
        );
    }
}
