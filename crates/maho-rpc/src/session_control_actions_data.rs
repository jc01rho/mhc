//! The session-control answers a multi-session host and a terminal control endpoint share (senpi
//! `rpc/session-control-actions-data.ts`): the model catalog, the thinking-level refusal, and the
//! `interrupt` outcome.
use maho_ai::types::ModelThinkingLevel;
use maho_core::agent_session::AgentSession;
use maho_ext_api::RunBackgroundStopOutcome;
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;

/// The refusal of a level the active model cannot run; callers match this text.
pub fn unsupported_thinking_level(level: &Value) -> String {
    let level = match level {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    };
    format!("Thinking level {level} is not supported by the active model.")
}

/// `get_available_models`: each model with the values the session itself acts on (#3666), a models.json or
/// extension definition may declare the priority pairing without putting it on the model.
pub fn available_models_data(session: &AgentSession) -> Value {
    let registry = session.model_registry();
    let thinking_enabled = session.thinking_level() != ModelThinkingLevel::Off;
    let models: Vec<Value> = registry
        .get_available()
        .into_iter()
        .map(|model| {
            let mut value = json!(model);
            if let Some(tier) = registry.get_service_tier(&model) {
                value["serviceTier"] = json!(tier);
            }
            if let Some(upstream) = registry.get_upstream_model_id(&model) {
                value["upstreamModelId"] = json!(upstream);
            }
            value["supportedThinkingLevels"] = json!(
                maho_core::thinking_levels::get_supported_thinking_levels(&model)
            );
            value["supportsAssistantPrefill"] = json!(
                maho_ai::model::model_supports_assistant_prefill(&model, thinking_enabled)
            );
            value
        })
        .collect();
    json!({ "models": models })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptOutcome {
    pub interrupted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Present when the caller asked `background: "stop"`: the interrupted turn's background jobs it stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<RunBackgroundStopOutcome>,
}

/// `interrupt { turnId?, background? }`: stops the running turn and only that. An idle session answers
/// `interrupted: false`; so does a `turnId` naming a turn that is no longer the running one, which then
/// answers the running turn's id. `stop` resolves once the turn has stopped; with `background: "stop"` the
/// background jobs that turn started are stopped after it, and the answer names them.
pub async fn interrupt_running_turn<F, Fut>(
    session: &AgentSession,
    requested_turn_id: Option<&Value>,
    background: Option<&str>,
    stop: F,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let turn_id = session.external_admission().turn_epoch().to_string();
    decide_interrupt(
        session.is_streaming() || session.retry_attempt() != 0,
        turn_id,
        requested_turn_id,
        background == Some("stop"),
        stop,
        |turn_id| async move { session.stop_run_background_jobs(&turn_id).await },
    )
    .await
}

async fn decide_interrupt<F, Fut, G, GFut>(
    running: bool,
    turn_id: String,
    requested_turn_id: Option<&Value>,
    stops_background: bool,
    stop: F,
    stop_background: G,
) -> InterruptOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
    G: FnOnce(String) -> GFut,
    GFut: Future<Output = RunBackgroundStopOutcome>,
{
    let nothing = || stops_background.then(RunBackgroundStopOutcome::default);
    if !running {
        return InterruptOutcome {
            interrupted: false,
            turn_id: None,
            background: nothing(),
        };
    }
    if let Some(Value::String(requested)) = requested_turn_id
        && *requested != turn_id
    {
        return InterruptOutcome {
            interrupted: false,
            turn_id: Some(turn_id),
            background: nothing(),
        };
    }
    stop().await;
    let background = if stops_background {
        Some(stop_background(turn_id.clone()).await)
    } else {
        None
    };
    InterruptOutcome {
        interrupted: true,
        turn_id: Some(turn_id),
        background,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{RunBackgroundJobKind, StoppedRunBackgroundJob};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn no_background(_turn_id: String) -> std::future::Ready<RunBackgroundStopOutcome> {
        std::future::ready(RunBackgroundStopOutcome::default())
    }

    #[test]
    fn the_refusal_text_names_the_level() {
        assert_eq!(
            unsupported_thinking_level(&json!("xhigh")),
            "Thinking level xhigh is not supported by the active model."
        );
    }

    #[tokio::test]
    async fn idle_and_stale_turns_are_not_interrupted() {
        let stopped = AtomicBool::new(false);
        let idle = decide_interrupt(
            false,
            "3".into(),
            None,
            false,
            || async { stopped.store(true, Ordering::SeqCst) },
            no_background,
        )
        .await;
        assert_eq!(
            idle,
            InterruptOutcome {
                interrupted: false,
                turn_id: None,
                background: None
            }
        );
        let stale = decide_interrupt(
            true,
            "3".into(),
            Some(&json!("2")),
            false,
            || async { stopped.store(true, Ordering::SeqCst) },
            no_background,
        )
        .await;
        assert_eq!(
            stale,
            InterruptOutcome {
                interrupted: false,
                turn_id: Some("3".into()),
                background: None
            }
        );
        assert!(!stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_running_turn_is_stopped_for_a_matching_or_absent_id() {
        let stopped = AtomicBool::new(false);
        let background_stopped = AtomicBool::new(false);
        let outcome = decide_interrupt(
            true,
            "3".into(),
            Some(&json!("3")),
            false,
            || async { stopped.store(true, Ordering::SeqCst) },
            |_| async {
                background_stopped.store(true, Ordering::SeqCst);
                RunBackgroundStopOutcome::default()
            },
        )
        .await;
        assert_eq!(
            serde_json::to_value(&outcome).expect("wire"),
            json!({"interrupted":true,"turnId":"3"})
        );
        assert!(stopped.load(Ordering::SeqCst));
        assert!(!background_stopped.load(Ordering::SeqCst));
        // A non-string turnId does not name a turn.
        let any = decide_interrupt(
            true,
            "3".into(),
            Some(&json!(3)),
            false,
            || async {},
            no_background,
        )
        .await;
        assert!(any.interrupted);
    }

    #[tokio::test]
    async fn background_stop_runs_after_the_turn_stopped_and_names_what_it_stopped() {
        let log = std::sync::Mutex::new(Vec::new());
        let log = &log;
        let outcome = decide_interrupt(
            true,
            "3".into(),
            Some(&json!("3")),
            true,
            || async move { log.lock().expect("log").push("turn".to_owned()) },
            |turn_id| async move {
                log.lock()
                    .expect("log")
                    .push(format!("background of {turn_id}"));
                RunBackgroundStopOutcome {
                    stopped: vec![StoppedRunBackgroundJob {
                        kind: RunBackgroundJobKind::Bash,
                        id: "bash_1".into(),
                        description: Some("sleep 60".into()),
                    }],
                    failed: Vec::new(),
                }
            },
        )
        .await;
        assert_eq!(*log.lock().expect("log"), ["turn", "background of 3"]);
        assert_eq!(
            serde_json::to_value(&outcome).expect("wire"),
            json!({
                "interrupted": true,
                "turnId": "3",
                "background": {
                    "stopped": [{"kind": "bash", "id": "bash_1", "description": "sleep 60"}],
                    "failed": []
                }
            })
        );
    }

    #[tokio::test]
    async fn an_idle_or_stale_interrupt_answers_an_empty_stop_list_when_background_was_asked() {
        let idle =
            decide_interrupt(false, "3".into(), None, true, || async {}, no_background).await;
        assert_eq!(
            serde_json::to_value(&idle).expect("wire"),
            json!({"interrupted": false, "background": {"stopped": [], "failed": []}})
        );
        let stale = decide_interrupt(
            true,
            "3".into(),
            Some(&json!("2")),
            true,
            || async {},
            no_background,
        )
        .await;
        assert_eq!(
            serde_json::to_value(&stale).expect("wire"),
            json!({"interrupted": false, "turnId": "3", "background": {"stopped": [], "failed": []}})
        );
    }
}
