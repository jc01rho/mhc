//! The one state rule a thread row follows.
//!
//! Upstream counterpart: `thread/tools/thread-state.ts`. Another device's listing row (`rowState`) belongs to the
//! account-wide half and is not ported here.

use std::collections::BTreeSet;

use serde_json::Value;

use super::contracts_state::{ThreadActivity, ThreadModel, ThreadState};

/// The archived flag wins; a thread no engine holds is `inactive`; otherwise the engine's activity, where a queued or
/// approval-waiting thread is mid-turn (running) and a failed turn has ended (idle). A field an older engine does not
/// report reads as not archived and idle.
pub fn thread_state(archived: bool, held: bool, activity: Option<&str>) -> ThreadState {
    if archived {
        return ThreadState::Archived;
    }
    if !held {
        return ThreadState::Inactive;
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
    fn the_archived_flag_wins_then_holding_then_activity() {
        assert_eq!(
            thread_state(true, true, Some("running")),
            ThreadState::Archived
        );
        assert_eq!(
            thread_state(false, false, Some("running")),
            ThreadState::Inactive
        );
        for running in ["running", "queued", "awaiting_approval"] {
            assert_eq!(
                thread_state(false, true, Some(running)),
                ThreadState::Running
            );
        }
        assert_eq!(
            thread_state(false, true, Some("errored")),
            ThreadState::Idle
        );
        assert_eq!(thread_state(false, true, None), ThreadState::Idle);
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
    fn the_default_listing_leaves_archived_threads_out() {
        let shown = listed_states(None);
        assert!(!shown.contains(&ThreadState::Archived));
        assert_eq!(shown.len(), 3);
        let asked = listed_states(Some(&[ThreadState::Archived]));
        assert_eq!(
            asked.into_iter().collect::<Vec<_>>(),
            vec![ThreadState::Archived]
        );
    }
}
