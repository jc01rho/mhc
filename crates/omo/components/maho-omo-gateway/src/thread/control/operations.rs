//! The one body of each control tool, over the control `locate_control` found.
//!
//! Upstream counterpart: `thread/control/operations.ts` and `thread/tools/archive-ops.ts`. A control's typed refusal
//! propagates as `ControlError::Refused`; the tool answers it as data. Every other answer is the tool's result JSON.

use serde::Serialize;
use serde_json::{Value, json};

use crate::thread::contracts_results::{
    ThreadArchiveOk, ThreadCloseOk, ThreadCloseReason, ThreadInterruptOk, ThreadSetModelOk,
    ThreadSetReasoningOk,
};
use crate::thread::contracts_state::ThreadModel;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::model_match::{
    MatchedModel, ModelChoice, exact_model, match_model_with_thinking, model_pattern_failure,
};

use super::port::{
    CloseOptions, CloseOutcome, ControlError, ControlResult, SessionControl, SessionModelState,
    control_identity, refuse,
};

/// A result's ok branch: the value with `kind: "ok"`.
pub fn ok_json<T: Serialize>(value: &T) -> Value {
    match serde_json::to_value(value) {
        Ok(Value::Object(mut object)) => {
            object.insert("kind".into(), json!("ok"));
            Value::Object(object)
        }
        Ok(other) => json!({ "kind": "ok", "value": other }),
        Err(error) => error_json(thread_tool_failure(
            "internal_error",
            format!("The thread result could not be encoded: {error}"),
            "Retry the call.",
            None,
        )),
    }
}

/// A result's error branch: the failure as data.
pub fn error_json(failure: ThreadToolFailure) -> Value {
    json!({ "kind": "error", "error": failure })
}

/// A control's answer for a thread no engine holds live, on this device or another.
pub fn not_live() -> ThreadToolFailure {
    thread_tool_failure(
        "not_resumable",
        "The thread has no live owner.",
        "Retry when the target is live.",
        None,
    )
}

pub async fn interrupt_thread(
    control: &dyn SessionControl,
    turn_id: Option<String>,
) -> ControlResult<Value> {
    let outcome = control.interrupt(turn_id).await?;
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadInterruptOk {
        thread,
        turn_id: outcome.turn_id,
        interrupted: outcome.interrupted,
        background: outcome.background,
        device,
    }))
}

/// Nothing live holds the thread: there is nothing to close, and it stays resumable from its session file.
pub fn close_not_live(thread_id: &str, device: Option<&str>) -> Value {
    ok_json(&ThreadCloseOk {
        thread: thread_id.to_owned(),
        closed: false,
        reason: ThreadCloseReason::NotLive,
        dropped_deliveries: Vec::new(),
        device: device.map(str::to_owned),
    })
}

/// Close options from `thread_close` inputs.
pub fn close_options(
    force: Option<bool>,
    graceful: Option<bool>,
    wait_ms: Option<i64>,
) -> CloseOptions {
    CloseOptions {
        force: force.unwrap_or(false),
        graceful: graceful.unwrap_or(false),
        wait_ms,
    }
}

pub async fn close_thread_with_options(
    control: &dyn SessionControl,
    options: CloseOptions,
) -> ControlResult<Value> {
    if options.graceful && options.force {
        return Ok(error_json(thread_tool_failure(
            "invalid_arguments",
            "graceful and force exclude each other: a graceful close lets the turn end, a forced one stops it.",
            "Pass one of them.",
            None,
        )));
    }
    if options.wait_ms.is_some() && !options.graceful {
        return Ok(error_json(thread_tool_failure(
            "invalid_arguments",
            "wait_ms only bounds a graceful close: a close without graceful does not wait for the turn.",
            "Pass graceful: true with wait_ms, or drop wait_ms.",
            None,
        )));
    }
    let answer = control.close(options).await?;
    let (thread, device) = control_identity(control);
    if answer.outcome == CloseOutcome::HeldElsewhere {
        return Ok(error_json(thread_tool_failure(
            "foreign_live_owner",
            format!(
                "Thread {thread} has no session file yet and another client still holds it, so only this tool's hold was released."
            ),
            "Close it from the client that holds it, or send it a message first so it can be closed for every holder.",
            device.map(|device| json!({ "device": device })),
        )));
    }
    let closed = answer.outcome == CloseOutcome::Closed;
    let mut result = ok_json(&ThreadCloseOk {
        thread,
        closed,
        reason: if closed {
            ThreadCloseReason::Closed
        } else {
            ThreadCloseReason::NotLive
        },
        dropped_deliveries: answer.dropped_deliveries,
        device,
    });
    if let Some(graceful) = answer.graceful
        && let Some(object) = result.as_object_mut()
    {
        object.insert("graceful".into(), json!(graceful));
    }
    Ok(result)
}

pub async fn close_thread(control: &dyn SessionControl, force: bool) -> ControlResult<Value> {
    close_thread_with_options(
        control,
        CloseOptions {
            force,
            ..Default::default()
        },
    )
    .await
}

fn match_model_for_set(
    catalog: &[ModelChoice],
    model: &str,
    provider: Option<&str>,
) -> Result<(ModelChoice, Option<String>), Box<ThreadToolFailure>> {
    let MatchedModel {
        model,
        thinking_level,
    } = match_model_with_thinking(catalog, model, provider)?;
    Ok((model, thinking_level))
}

/// One matching rule on the thread's own catalog. A catalog the control holds decides only an exact provider/id, at
/// once; any other pattern is matched against a fresh catalog. A set the engine refuses is answered as a fresh catalog
/// answers it: its own miss, the model it matches set, or the set's refusal when it matches the same model.
pub async fn set_model_on(
    control: &dyn SessionControl,
    model: &str,
    provider: Option<&str>,
) -> ControlResult<Value> {
    if let Some(failure) = model_pattern_failure(model) {
        return Ok(error_json(failure));
    }
    let first = control.get_available_models(false).await?;
    let matched = match (first.held, exact_model(&first.models, model, provider)) {
        (true, Some(held)) => Ok((held, None)),
        (true, None) => {
            let fresh = control.get_available_models(true).await?;
            match_model_for_set(&fresh.models, model, provider)
        }
        (false, _) => match_model_for_set(&first.models, model, provider),
    };
    let (chosen, mut thinking_level) = match matched {
        Ok(found) => found,
        Err(failure) => return Ok(error_json(*failure)),
    };
    let mut previous = model_before_switch(control, thinking_level.as_deref()).await?;
    let selected = match control
        .set_model(chosen.provider.clone(), chosen.id.clone())
        .await
    {
        Ok(selected) => selected,
        Err(error) => {
            let Some(refusal) = error.refusal() else {
                return Err(error);
            };
            if refusal.code.as_str() == "idempotency_uncertain" {
                return Err(error);
            }
            let Ok(fresh) = control.get_available_models(true).await else {
                return Err(error);
            };
            let (rematched, rematched_thinking) =
                match match_model_for_set(&fresh.models, model, provider) {
                    Ok(found) => found,
                    Err(failure) => return Ok(error_json(*failure)),
                };
            if rematched.provider == chosen.provider && rematched.id == chosen.id {
                return Err(error);
            }
            // The engine's refusal is the more specific answer when the target also cannot report its model state now;
            // any other failure (a transport error reading that state) is its own answer (#14482 review L2).
            match model_before_switch(control, rematched_thinking.as_deref()).await {
                Ok(state) => previous = state,
                Err(state_error) => {
                    let state_unavailable = state_error
                        .refusal()
                        .and_then(|refusal| refusal.details.as_ref())
                        .and_then(|details| details.get("reason"))
                        .and_then(Value::as_str)
                        == Some("model_state_unavailable");
                    if state_unavailable {
                        return Err(error);
                    }
                    return Err(state_error);
                }
            }
            thinking_level = rematched_thinking;
            control.set_model(rematched.provider, rematched.id).await?
        }
    };
    if let Some(previous) = previous
        && let Some(level) = thinking_level.as_deref()
        && let Some(refusal) = apply_model_thinking(control, &selected, &previous, level).await?
    {
        return Ok(refusal);
    }
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadSetModelOk {
        thread,
        model: selected,
        device,
    }))
}

async fn model_before_switch(
    control: &dyn SessionControl,
    level: Option<&str>,
) -> ControlResult<Option<SessionModelState>> {
    if level.is_none() {
        return Ok(None);
    }
    match control.get_model_state().await? {
        Some(previous) => Ok(Some(previous)),
        None => Err(refuse(
            "unsupported",
            "The target did not report a current model and thinking level to restore if the suffix fails.",
            "Retry when the target reports its active model and level, or switch the model without a thinking suffix.",
            Some(json!({ "reason": "model_state_unavailable" })),
        )),
    }
}

fn same_model(left: &ThreadModel, right: &ThreadModel) -> bool {
    left.provider == right.provider && left.id == right.id
}

enum PartialDetail<'a> {
    Error(&'a ControlError),
    Reason(&'static str),
}

fn partial_model_switch(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
    reason: &'static str,
    detail: Option<PartialDetail<'_>>,
) -> Value {
    let (thread, device) = control_identity(control);
    let mut details = serde_json::Map::new();
    details.insert("thread".into(), json!(thread));
    if let Some(device) = device {
        details.insert("device".into(), json!(device));
    }
    details.insert("reason".into(), json!(reason));
    details.insert("previous_model".into(), json!(previous.model));
    details.insert(
        "previous_thinking_level".into(),
        json!(previous.thinking_level),
    );
    details.insert("selected_model".into(), json!(selected));
    details.insert("requested_thinking_level".into(), json!(level));
    if let Some(detail) = detail {
        let text = match detail {
            PartialDetail::Reason(reason) => reason.to_owned(),
            PartialDetail::Error(ControlError::Refused(failure)) => {
                failure.code.as_str().to_owned()
            }
            PartialDetail::Error(ControlError::Transport(message)) => message.clone(),
        };
        details.insert("detail".into(), json!(text));
    }
    error_json(thread_tool_failure(
        "partial_commit",
        "The model and thinking change did not complete and the prior state could not be confirmed.",
        "Read the target's current model and thinking level before retrying.",
        Some(Value::Object(details)),
    ))
}

async fn restore_model_state(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
) -> Option<Value> {
    let partial_reason = |reason: &'static str| {
        partial_model_switch(
            control,
            selected,
            previous,
            level,
            "model_restore_failed",
            Some(PartialDetail::Reason(reason)),
        )
    };
    let partial_err = |error: &ControlError| {
        partial_model_switch(
            control,
            selected,
            previous,
            level,
            "model_restore_failed",
            Some(PartialDetail::Error(error)),
        )
    };
    let mut current = match control.get_model_state().await {
        Ok(Some(state)) => state,
        Ok(None) => return Some(partial_reason("model_state_unavailable")),
        Err(error) => return Some(partial_err(&error)),
    };
    if !same_model(&current.model, &previous.model) {
        if !same_model(&current.model, selected) {
            return Some(partial_reason("active_model_changed"));
        }
        if let Err(error) = control
            .set_model(previous.model.provider.clone(), previous.model.id.clone())
            .await
        {
            return Some(partial_err(&error));
        }
        current = match control.get_model_state().await {
            Ok(Some(state)) if same_model(&state.model, &previous.model) => state,
            Ok(_) => return Some(partial_reason("restore_unconfirmed")),
            Err(error) => return Some(partial_err(&error)),
        };
    }
    if current.thinking_level != previous.thinking_level {
        if let Err(error) = control
            .set_thinking_level(previous.thinking_level.clone())
            .await
        {
            return Some(partial_err(&error));
        }
        match control.get_model_state().await {
            Ok(Some(restored))
                if same_model(&restored.model, &previous.model)
                    && restored.thinking_level == previous.thinking_level => {}
            Ok(_) => return Some(partial_reason("restore_unconfirmed")),
            Err(error) => return Some(partial_err(&error)),
        }
    }
    None
}

async fn apply_model_thinking(
    control: &dyn SessionControl,
    selected: &ThreadModel,
    previous: &SessionModelState,
    level: &str,
) -> ControlResult<Option<Value>> {
    // A set_model receipt can acknowledge a switch held for compaction; never apply the level to the old model.
    match control.get_model_state().await {
        Ok(Some(active)) if same_model(&active.model, selected) => {}
        Ok(_) => {
            return Ok(Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_switch_unconfirmed",
                None,
            )));
        }
        Err(error) => {
            return Ok(Some(partial_model_switch(
                control,
                selected,
                previous,
                level,
                "model_switch_unconfirmed",
                Some(PartialDetail::Error(&error)),
            )));
        }
    }
    match apply_thinking_level(control, level).await {
        Ok(None) => Ok(None),
        Ok(Some(refusal)) => match restore_model_state(control, selected, previous, level).await {
            Some(partial) => Ok(Some(partial)),
            None => Ok(Some(refusal)),
        },
        Err(error) => match restore_model_state(control, selected, previous, level).await {
            Some(partial) => Ok(Some(partial)),
            None => Err(error),
        },
    }
}

pub async fn set_thinking_level_on(
    control: &dyn SessionControl,
    level: &str,
    scope: Option<&str>,
) -> ControlResult<Value> {
    if let Some(refusal) = apply_thinking_level(control, level).await? {
        return Ok(refusal);
    }
    let (thread, device) = control_identity(control);
    Ok(ok_json(&ThreadSetReasoningOk {
        thread,
        level: level.to_owned(),
        scope: scope.unwrap_or("session").to_owned(),
        device,
    }))
}

async fn apply_thinking_level(
    control: &dyn SessionControl,
    level: &str,
) -> ControlResult<Option<Value>> {
    if let Err(error) = control.set_thinking_level(level.to_owned()).await {
        if error.refusal().map(|refusal| refusal.code.as_str())
            != Some("thinking_level_unsupported")
        {
            return Err(error);
        }
        let supported = control.get_available_thinking_levels().await?;
        return Ok(Some(error_json(thread_tool_failure(
            "thinking_level_unsupported",
            format!("Thinking level \"{level}\" is not supported by the active model."),
            "Choose a level from the supported list and retry.",
            Some(json!({ "supported": supported })),
        ))));
    }
    Ok(None)
}

/// `thread_archive` for a thread no engine holds: its session file cannot be changed now.
pub fn archive_not_live(thread_id: &str, archived: bool) -> Value {
    error_json(thread_tool_failure(
        "not_resumable",
        format!(
            "Thread {thread_id} has no engine holding it, so its session file cannot be {} now.",
            if archived { "archived" } else { "unarchived" }
        ),
        "Open it with thread_create (tool.thread_create({ thread }) in an eval cell), then retry.",
        None,
    ))
}

/// `thread_archive`: sets the archived flag in the thread's session file and nothing else; the session keeps running.
/// The caller's own thread is refused `turn_active` while its turn runs, so nothing is archived mid-turn.
pub async fn archive_on(
    control: &dyn SessionControl,
    archived: bool,
    caller_id: &str,
) -> ControlResult<Value> {
    if archived
        && control.device().is_none()
        && control.thread_id() == caller_id
        && control.running().await?
    {
        return Ok(error_json(thread_tool_failure(
            "turn_active",
            "This is the calling thread and its turn is still running; archiving it mid-turn is refused.",
            "Archive it from another thread, or after this turn ends.",
            Some(json!({ "thread": control.thread_id() })),
        )));
    }
    control.set_archived(archived).await?;
    Ok(ok_json(&ThreadArchiveOk {
        thread: control.listed(archived).await?,
        archived,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use futures::executor::block_on;

    use crate::thread::adapter::BoxFuture;
    use crate::thread::addressing::ThreadAddressEntry;
    use crate::thread::contracts_results::ThreadResultSummary;

    use super::super::interrupt::InterruptOutcome;
    use super::super::port::{CloseAnswer, CloseGraceful, CloseOutcome, ModelCatalog};
    use super::*;

    type StateReply = ControlResult<Option<SessionModelState>>;
    type SetModelReply = ControlResult<ThreadModel>;
    type ThinkingReply = ControlResult<()>;

    struct MockControl {
        models: Mutex<VecDeque<ModelCatalog>>,
        states: Mutex<VecDeque<StateReply>>,
        set_models: Mutex<VecDeque<SetModelReply>>,
        set_thinking: Mutex<VecDeque<ThinkingReply>>,
        supported_levels: Vec<String>,
        close_answer: Mutex<Option<CloseAnswer>>,
        calls: Mutex<Vec<String>>,
    }

    impl MockControl {
        fn new() -> Self {
            Self {
                models: Mutex::new(VecDeque::new()),
                states: Mutex::new(VecDeque::new()),
                set_models: Mutex::new(VecDeque::new()),
                set_thinking: Mutex::new(VecDeque::new()),
                supported_levels: vec!["off".into(), "medium".into()],
                close_answer: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls").clone()
        }
    }

    fn model_state(provider: &str, id: &str, level: &str) -> SessionModelState {
        SessionModelState {
            model: ThreadModel {
                provider: provider.into(),
                id: id.into(),
            },
            thinking_level: level.into(),
        }
    }

    impl SessionControl for MockControl {
        fn thread_id(&self) -> &str {
            "dur-peer"
        }

        fn device(&self) -> Option<&str> {
            None
        }

        fn title(&self) -> &str {
            "peer"
        }

        fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn interrupt(
            &self,
            _turn_id: Option<String>,
        ) -> BoxFuture<ControlResult<InterruptOutcome>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn close(&self, options: CloseOptions) -> BoxFuture<ControlResult<CloseAnswer>> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("close:graceful={}", options.graceful));
            let answer = self
                .close_answer
                .lock()
                .expect("close")
                .take()
                .ok_or_else(|| ControlError::Transport("unexpected close".into()));
            Box::pin(async move { answer })
        }

        fn set_name(&self, _name: String) -> BoxFuture<ControlResult<()>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn get_available_models(&self, fresh: bool) -> BoxFuture<ControlResult<ModelCatalog>> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("models:fresh={fresh}"));
            let reply = self
                .models
                .lock()
                .expect("models")
                .pop_front()
                .ok_or_else(|| ControlError::Transport("unexpected models".into()));
            Box::pin(async move { reply })
        }

        fn get_model_state(&self) -> BoxFuture<ControlResult<Option<SessionModelState>>> {
            self.calls.lock().expect("calls").push("get_state".into());
            let reply = self
                .states
                .lock()
                .expect("states")
                .pop_front()
                .unwrap_or(Err(ControlError::Transport("unexpected state".into())));
            Box::pin(async move { reply })
        }

        fn set_model(&self, provider: String, id: String) -> BoxFuture<ControlResult<ThreadModel>> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("set_model:{provider}/{id}"));
            let reply = self
                .set_models
                .lock()
                .expect("set_models")
                .pop_front()
                .unwrap_or(Ok(ThreadModel { provider, id }));
            Box::pin(async move { reply })
        }

        fn get_available_thinking_levels(&self) -> BoxFuture<ControlResult<Vec<String>>> {
            let levels = self.supported_levels.clone();
            Box::pin(async move { Ok(levels) })
        }

        fn set_thinking_level(&self, level: String) -> BoxFuture<ControlResult<()>> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("set_thinking:{level}"));
            let reply = self
                .set_thinking
                .lock()
                .expect("set_thinking")
                .pop_front()
                .unwrap_or(Ok(()));
            Box::pin(async move { reply })
        }

        fn get_messages(&self) -> BoxFuture<ControlResult<Vec<Value>>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn running(&self) -> BoxFuture<ControlResult<bool>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn set_archived(&self, _archived: bool) -> BoxFuture<ControlResult<()>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }

        fn listed(&self, _archived: bool) -> BoxFuture<ControlResult<ThreadResultSummary>> {
            Box::pin(async { Err(ControlError::Transport("unused".into())) })
        }
    }

    fn choice(provider: &str, id: &str) -> ModelChoice {
        ModelChoice {
            provider: provider.into(),
            id: id.into(),
            name: None,
        }
    }

    #[test]
    fn set_model_rematch_keeps_engine_refusal_when_model_state_unavailable_and_propagates_transport()
     {
        let missing_state = MockControl::new();
        *missing_state.models.lock().expect("models") = VecDeque::from([
            ModelCatalog {
                models: vec![choice("openai", "gpt-x")],
                held: false,
            },
            ModelCatalog {
                models: vec![choice("azure", "gpt-x")],
                held: false,
            },
        ]);
        *missing_state.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
            Ok(None),
        ]);
        *missing_state.set_models.lock().expect("set_models") = VecDeque::from([Err(refuse(
            "model_not_found",
            "The engine no longer offers openai/gpt-x.",
            "Pick another model.",
            None,
        ))]);
        let err = block_on(set_model_on(&missing_state, "gpt-x:max", None))
            .expect_err("original refusal");
        assert_eq!(
            err.refusal().map(|r| r.code.as_str()),
            Some("model_not_found")
        );

        let transport_fail = MockControl::new();
        *transport_fail.models.lock().expect("models") = VecDeque::from([
            ModelCatalog {
                models: vec![choice("openai", "gpt-x")],
                held: false,
            },
            ModelCatalog {
                models: vec![choice("azure", "gpt-x")],
                held: false,
            },
        ]);
        *transport_fail.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
            Err(ControlError::Transport(
                "host_unavailable: closed".to_owned(),
            )),
        ]);
        *transport_fail.set_models.lock().expect("set_models") = VecDeque::from([Err(refuse(
            "model_not_found",
            "The engine no longer offers openai/gpt-x.",
            "Pick another model.",
            None,
        ))]);
        let err = block_on(set_model_on(&transport_fail, "gpt-x:max", None))
            .expect_err("transport error");
        assert_eq!(
            err,
            ControlError::Transport("host_unavailable: closed".to_owned())
        );
    }

    #[test]
    fn set_model_applies_thinking_suffix_and_prefers_literal_colon_model_id() {
        let control = MockControl::new();
        *control.models.lock().expect("models") = VecDeque::from([ModelCatalog {
            models: vec![choice("openai", "gpt-x"), choice("openai", "gpt-other:max")],
            held: false,
        }]);
        *control.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
            Ok(Some(model_state("openai", "gpt-x", "off"))),
        ]);
        let result = block_on(set_model_on(&control, "openai/gpt-x:max", None)).expect("ok");
        assert_eq!(result["kind"], "ok");
        assert_eq!(
            result["model"],
            json!({ "provider": "openai", "id": "gpt-x" })
        );
        assert!(control.calls().contains(&"set_thinking:max".to_owned()));

        let literal = MockControl::new();
        *literal.models.lock().expect("models") = VecDeque::from([ModelCatalog {
            models: vec![choice("openai", "gpt-x"), choice("openai", "gpt-other:max")],
            held: false,
        }]);
        let result =
            block_on(set_model_on(&literal, "openai/gpt-other:max", None)).expect("literal ok");
        assert_eq!(
            result["model"],
            json!({ "provider": "openai", "id": "gpt-other:max" })
        );
        assert!(
            !literal
                .calls()
                .iter()
                .any(|c| c.starts_with("set_thinking"))
        );
    }

    #[test]
    fn set_model_restores_prior_state_when_thinking_suffix_is_refused() {
        let control = MockControl::new();
        *control.models.lock().expect("models") = VecDeque::from([ModelCatalog {
            models: vec![choice("openai", "gpt-x")],
            held: false,
        }]);
        *control.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "low"))),
            Ok(Some(model_state("openai", "gpt-x", "low"))),
            Ok(Some(model_state("openai", "gpt-x", "low"))),
            Ok(Some(model_state("openai", "gpt-previous", "low"))),
        ]);
        *control.set_thinking.lock().expect("set_thinking") = VecDeque::from([Err(refuse(
            "thinking_level_unsupported",
            "unsupported",
            "choose",
            None,
        ))]);
        let result = block_on(set_model_on(&control, "openai/gpt-x:max", None)).expect("refusal");
        assert_eq!(result["kind"], "error");
        assert_eq!(result["error"]["code"], "thinking_level_unsupported");
        assert_eq!(
            result["error"]["details"]["supported"],
            json!(["off", "medium"])
        );
        assert!(
            control
                .calls()
                .contains(&"set_model:openai/gpt-previous".to_owned())
        );
    }

    #[test]
    fn set_model_returns_partial_commit_when_switch_held_or_restore_fails() {
        let held = MockControl::new();
        *held.models.lock().expect("models") = VecDeque::from([ModelCatalog {
            models: vec![choice("openai", "gpt-x")],
            held: false,
        }]);
        *held.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
        ]);
        let result = block_on(set_model_on(&held, "openai/gpt-x:max", None)).expect("partial");
        assert_eq!(result["kind"], "error");
        assert_eq!(result["error"]["code"], "partial_commit");
        assert_eq!(
            result["error"]["details"]["reason"],
            "model_switch_unconfirmed"
        );
        assert!(!held.calls().iter().any(|c| c.starts_with("set_thinking")));

        let broken_restore = MockControl::new();
        *broken_restore.models.lock().expect("models") = VecDeque::from([ModelCatalog {
            models: vec![choice("openai", "gpt-x")],
            held: false,
        }]);
        *broken_restore.states.lock().expect("states") = VecDeque::from([
            Ok(Some(model_state("openai", "gpt-previous", "off"))),
            Ok(Some(model_state("openai", "gpt-x", "off"))),
            Ok(Some(model_state("openai", "gpt-x", "off"))),
        ]);
        *broken_restore.set_models.lock().expect("set_models") = VecDeque::from([
            Ok(ThreadModel {
                provider: "openai".into(),
                id: "gpt-x".into(),
            }),
            Err(ControlError::Transport("previous unavailable".into())),
        ]);
        *broken_restore.set_thinking.lock().expect("set_thinking") = VecDeque::from([Err(refuse(
            "thinking_level_unsupported",
            "unsupported",
            "choose",
            None,
        ))]);
        let result = block_on(set_model_on(&broken_restore, "openai/gpt-x:max", None))
            .expect("partial restore");
        assert_eq!(result["kind"], "error");
        assert_eq!(result["error"]["code"], "partial_commit");
        assert_eq!(result["error"]["details"]["reason"], "model_restore_failed");
    }

    #[test]
    fn close_thread_with_options_validates_flags_and_includes_graceful_summary() {
        let control = MockControl::new();
        let both = block_on(close_thread_with_options(
            &control,
            close_options(Some(true), Some(true), None),
        ))
        .expect("both");
        assert_eq!(both["kind"], "error");
        assert_eq!(both["error"]["code"], "invalid_arguments");

        let wait_without_graceful = block_on(close_thread_with_options(
            &control,
            close_options(None, Some(false), Some(5_000)),
        ))
        .expect("wait without graceful");
        assert_eq!(wait_without_graceful["kind"], "error");
        assert_eq!(wait_without_graceful["error"]["code"], "invalid_arguments");
        assert!(control.calls().is_empty());

        *control.close_answer.lock().expect("close") = Some(CloseAnswer {
            outcome: CloseOutcome::Closed,
            dropped_deliveries: vec!["d-1".into()],
            graceful: Some(CloseGraceful {
                turn: 4,
                waited_ms: 1200,
                stopped_wake_sources: vec!["terminal".into()],
                returned_user_messages: vec!["draft".into()],
            }),
        });
        let ok = block_on(close_thread_with_options(
            &control,
            close_options(None, Some(true), Some(5_000)),
        ))
        .expect("ok");
        assert_eq!(ok["kind"], "ok");
        assert_eq!(ok["closed"], true);
        assert_eq!(ok["graceful"]["turn"], 4);
        assert_eq!(ok["graceful"]["waited_ms"], 1200);
        assert_eq!(ok["graceful"]["stopped_wake_sources"], json!(["terminal"]));
        assert_eq!(ok["graceful"]["returned_user_messages"], json!(["draft"]));
    }
}
