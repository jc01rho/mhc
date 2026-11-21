//! Execution and recovery predicates of the compaction port (senpi#6833, senpi#6764, senpi#6772).

pub use super::compaction::{compact, generate_turn_prefix_summary};
use super::lifecycle::CompactionLifecycleState;

pub fn is_non_latching_rejection_cause(cause: &str) -> bool {
    matches!(
        cause,
        "external-owner" | "transient-failure" | "circuit-breaker"
    )
}

pub fn failed_compaction_latches_admission(
    state: &CompactionLifecycleState,
    has_compaction_entry: bool,
) -> bool {
    let CompactionLifecycleState::Failed(_, _, cause, _) = state else {
        return false;
    };
    has_compaction_entry
        && !cause
            .as_deref()
            .is_some_and(is_non_latching_rejection_cause)
}

pub const fn should_exclude_from_provider_fallback(
    is_local_failure: bool,
    is_required_compaction_error: bool,
    is_model_config_failure: bool,
) -> bool {
    is_local_failure || is_required_compaction_error || is_model_config_failure
}
