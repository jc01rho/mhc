//! The one state rule a thread row follows.
//!
//! Upstream counterpart: `thread/tools/thread-state.ts`. Another device's listing row (`rowState`) belongs to the
//! account-wide half and is not ported here.

use std::collections::BTreeSet;

use serde_json::Value;

use super::contracts_state::{
    ThreadActivity, ThreadModel, ThreadRetryWait, ThreadRetryWaitReason, ThreadState,
};

/// The archived flag wins; a thread no engine holds is `inactive`; a held thread its engine is still opening is
/// `opening`; otherwise the engine's activity, where a queued or approval-waiting thread is mid-turn (running) and a
/// failed turn has ended (idle). A field an older engine does not report reads as not archived and idle.
pub fn thread_state(
    archived: bool,
    held: bool,
    opening: bool,
    activity: Option<&str>,
) -> ThreadState {
    if archived {
        return ThreadState::Archived;
    }
    if !held {
        return ThreadState::Inactive;
    }
    if opening {
        return ThreadState::Opening;
    }
    match activity {
        Some("running" | "queued" | "awaiting_approval") => ThreadState::Running,
        _ => ThreadState::Idle,
    }
}

/// A held thread's activity detail as its engine reports it; nothing for an older engine or a thread no engine holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityDetail {
    pub activity: Option<ThreadActivity>,
    pub error_code: Option<String>,
    pub turn_id: Option<String>,
}

pub fn activity_detail(
    held: bool,
    activity: Option<&str>,
    error_code: Option<&str>,
    turn_id: Option<&str>,
) -> ActivityDetail {
    let Some(activity) = activity.filter(|_| held).and_then(ThreadActivity::parse) else {
        return ActivityDetail::default();
    };
    let present = |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_owned);
    ActivityDetail {
        activity: Some(activity),
        error_code: if activity == ThreadActivity::Errored {
            present(error_code)
        } else {
            None
        },
        turn_id: present(turn_id),
    }
}

fn is_valid_iso_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    let year_ok = bytes[0..4].iter().all(u8::is_ascii_digit);
    let month_ok = bytes[4] == b'-' && bytes[5..7].iter().all(u8::is_ascii_digit);
    let day_ok = bytes[7] == b'-' && bytes[8..10].iter().all(u8::is_ascii_digit);
    if !(year_ok && month_ok && day_ok) {
        return false;
    }
    if bytes.len() == 10 {
        return true;
    }
    matches!(bytes[10], b'T' | b't' | b' ')
}

/// A running thread asleep between provider retries, from senpi#7025's `waiting` on its
/// `list_sessions` row (`{kind: "rate_limit" | "retry", attempt, maxAttempts, delayMs, until, error?}`).
/// Returns `None` for an older engine, a thread that is not running, or an unknown wait shape.
pub fn retry_wait(running: bool, waiting: Option<&Value>) -> Option<ThreadRetryWait> {
    if !running {
        return None;
    }
    let waiting = waiting?.as_object()?;
    let reason = waiting
        .get("kind")
        .and_then(Value::as_str)
        .and_then(ThreadRetryWaitReason::parse)?;
    let until = waiting
        .get("until")
        .and_then(Value::as_str)
        .filter(|until| is_valid_iso_timestamp(until))?;
    let attempt = waiting.get("attempt").and_then(Value::as_i64)?;
    let max_attempts = waiting
        .get("maxAttempts")
        .or_else(|| waiting.get("max_attempts"))
        .and_then(Value::as_i64)?;
    Some(ThreadRetryWait {
        kind: "retry".to_owned(),
        reason,
        until: until.to_owned(),
        attempt,
        max_attempts,
    })
}

/// omo-relay-spec #314: a holder version is an opaque, nonempty string of at most 128 UTF-16 code units (a Rust
/// string is always well-formed); anything else is unknown and shown as nothing.
const ENGINE_VERSION_MAX_UNITS: usize = 128;

pub fn holder_version(value: Option<&Value>) -> Option<String> {
    let version = value?.as_str()?;
    (!version.is_empty() && version.encode_utf16().count() <= ENGINE_VERSION_MAX_UNITS)
        .then(|| version.to_owned())
}

/// A reported model, when it is exactly a non-empty provider and id.
pub fn thread_model(value: Option<&Value>) -> Option<ThreadModel> {
    let value = value?;
    let provider = value.get("provider")?.as_str()?;
    let id = value.get("id")?.as_str()?;
    (!provider.is_empty() && !id.is_empty()).then(|| ThreadModel {
        provider: provider.to_owned(),
        id: id.to_owned(),
    })
}

/// The states a `thread_list` returns: the ones asked for, else every state but `archived`.
pub fn listed_states(asked: Option<&[ThreadState]>) -> BTreeSet<ThreadState> {
    match asked {
        Some(asked) => asked.iter().copied().collect(),
        None => ThreadState::ALL
            .into_iter()
            .filter(|state| *state != ThreadState::Archived)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_version_is_a_nonempty_string_of_at_most_128_utf16_units() {
        let at_limit = "x".repeat(128);
        let over = "x".repeat(129);
        assert_eq!(
            holder_version(Some(&Value::from("2026.11.19-3"))).as_deref(),
            Some("2026.11.19-3")
        );
        assert_eq!(
            holder_version(Some(&Value::from(at_limit.clone()))),
            Some(at_limit)
        );
        assert_eq!(holder_version(Some(&Value::from(over))), None);
        assert_eq!(holder_version(Some(&Value::from(""))), None);
        assert_eq!(holder_version(Some(&Value::from(7))), None);
        assert_eq!(holder_version(Some(&Value::Null)), None);
        assert_eq!(holder_version(None), None);
        let wide = "\u{1f600}".repeat(65);
        assert_eq!(holder_version(Some(&Value::from(wide))), None);
    }

    #[test]
    fn the_archived_flag_wins_then_holding_then_activity() {
        assert_eq!(
            thread_state(true, true, false, Some("running")),
            ThreadState::Archived
        );
        assert_eq!(
            thread_state(false, false, false, Some("running")),
            ThreadState::Inactive
        );
        for running in ["running", "queued", "awaiting_approval"] {
            assert_eq!(
                thread_state(false, true, false, Some(running)),
                ThreadState::Running
            );
        }
        assert_eq!(
            thread_state(false, true, false, Some("errored")),
            ThreadState::Idle
        );
        assert_eq!(thread_state(false, true, false, None), ThreadState::Idle);
        assert_eq!(
            thread_state(false, true, true, Some("running")),
            ThreadState::Opening
        );
        assert_eq!(
            thread_state(false, false, true, None),
            ThreadState::Inactive
        );
        assert_eq!(thread_state(true, true, true, None), ThreadState::Archived);
    }

    #[test]
    fn an_activity_detail_needs_a_held_thread_and_a_known_activity() {
        let errored = activity_detail(true, Some("errored"), Some("rate_limit"), Some("t-1"));
        assert_eq!(errored.activity, Some(ThreadActivity::Errored));
        assert_eq!(errored.error_code.as_deref(), Some("rate_limit"));
        assert_eq!(errored.turn_id.as_deref(), Some("t-1"));
        let running = activity_detail(true, Some("running"), Some("rate_limit"), None);
        assert_eq!(running.error_code, None);
        assert_eq!(
            activity_detail(false, Some("running"), None, None),
            ActivityDetail::default()
        );
        assert_eq!(
            activity_detail(true, Some("dancing"), None, None),
            ActivityDetail::default()
        );
    }

    #[test]
    fn a_model_needs_both_a_provider_and_an_id() {
        let model = serde_json::json!({ "provider": "anthropic", "id": "opus" });
        assert_eq!(
            thread_model(Some(&model)),
            Some(ThreadModel {
                provider: "anthropic".into(),
                id: "opus".into()
            })
        );
        let empty = serde_json::json!({ "provider": "", "id": "opus" });
        assert_eq!(thread_model(Some(&empty)), None);
        assert_eq!(thread_model(None), None);
    }

    #[test]
    fn retry_wait_carries_reason_until_and_attempts_for_a_running_thread() {
        let rate_limited = serde_json::json!({
            "kind": "rate_limit",
            "attempt": 2,
            "maxAttempts": 5,
            "delayMs": 45_000,
            "until": "2026-11-19T05:00:45.000Z",
            "error": "429 too many"
        });
        assert_eq!(
            retry_wait(true, Some(&rate_limited)),
            Some(ThreadRetryWait {
                kind: "retry".into(),
                reason: ThreadRetryWaitReason::RateLimit,
                until: "2026-11-19T05:00:45.000Z".into(),
                attempt: 2,
                max_attempts: 5,
            })
        );
        let retrying = serde_json::json!({
            "kind": "retry",
            "attempt": 1,
            "maxAttempts": 3,
            "delayMs": 1_000,
            "until": "2026-11-19T05:01:00.000Z"
        });
        assert_eq!(
            retry_wait(true, Some(&retrying)).map(|wait| wait.reason),
            Some(ThreadRetryWaitReason::Retry)
        );
    }

    #[test]
    fn retry_wait_ignores_non_running_threads_and_unknown_shapes() {
        let valid = serde_json::json!({
            "kind": "rate_limit",
            "attempt": 2,
            "maxAttempts": 5,
            "until": "2026-11-19T05:00:45.000Z"
        });
        assert_eq!(retry_wait(false, Some(&valid)), None);
        assert_eq!(retry_wait(true, None), None);
        let unknown_kind = serde_json::json!({
            "kind": "quota",
            "attempt": 2,
            "maxAttempts": 5,
            "until": "2026-11-19T05:00:45.000Z"
        });
        assert_eq!(retry_wait(true, Some(&unknown_kind)), None);
        let bad_date = serde_json::json!({
            "kind": "retry",
            "attempt": 2,
            "maxAttempts": 5,
            "until": "not-a-date"
        });
        assert_eq!(retry_wait(true, Some(&bad_date)), None);
    }

    #[test]
    fn the_default_listing_leaves_archived_threads_out() {
        let shown = listed_states(None);
        assert!(!shown.contains(&ThreadState::Archived));
        assert!(shown.contains(&ThreadState::Opening));
        assert_eq!(shown.len(), 4);
        let asked = listed_states(Some(&[ThreadState::Archived]));
        assert_eq!(
            asked.into_iter().collect::<Vec<_>>(),
            vec![ThreadState::Archived]
        );
    }
}
