//! Native port of `@latent-variable/pi-auto-continue` (`src/index.ts`): after each agent loop ends,
//! send a "continue" message until a cap, toggled with the `auto-continue-*` commands.

use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionRuntime, InputEventResult, InputSource, NotificationType, SendUserMessageOptions,
    UserMessageContent,
};
use std::sync::{Arc, Mutex};

pub const STATUS_KEY: &str = "auto-continue";
const DEFAULT_TEXT: &str = "continue";
const DEFAULT_MAX: u32 = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub text: String,
    pub max: u32,
    pub count: u32,
}

impl Default for State {
    fn default() -> Self {
        Self { enabled: false, text: DEFAULT_TEXT.into(), max: DEFAULT_MAX, count: 0 }
    }
}

impl State {
    pub fn status(&self) -> Option<String> {
        self.enabled.then(|| format!("auto-continue {}/{}", self.count, self.max))
    }
}

/// What the agent_end handler decided for one finished loop.
#[derive(Debug, PartialEq, Eq)]
pub enum EndOutcome {
    Idle,
    DisabledByAbort,
    ReachedMax(u32),
    Continue(String),
}

pub fn on_agent_end(state: &mut State, aborted: bool) -> EndOutcome {
    if !state.enabled {
        return EndOutcome::Idle;
    }
    // The user hit Escape (or otherwise aborted the turn): break the loop.
    if aborted {
        state.enabled = false;
        return EndOutcome::DisabledByAbort;
    }
    if state.count >= state.max {
        state.enabled = false;
        return EndOutcome::ReachedMax(state.max);
    }
    state.count += 1;
    EndOutcome::Continue(state.text.clone())
}

/// `/auto-continue-max` argument: a positive integer prefix, as `Number.parseInt(args, 10)` reads it.
pub fn parse_max(args: &str) -> Option<u32> {
    let trimmed = args.trim();
    let sign_len = usize::from(trimmed.starts_with(['+', '-']));
    let digits = trimmed[sign_len..].chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || trimmed.starts_with('-') {
        return None;
    }
    trimmed[sign_len..sign_len + digits].parse::<u32>().ok().filter(|n| *n > 0)
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn update_status(ctx: &ExtensionContext, state: &State) {
    ctx.ui.set_status(STATUS_KEY, Some(state.status().unwrap_or_default().as_str()));
}

fn send_next(runtime: ExtensionRuntime, ctx: &ExtensionContext, state: Shared) {
    let idle = (ctx.wait_for_idle_fn)();
    let ui = Arc::clone(&ctx.ui);
    // Defer so the current agent turn fully settles into idle before the next user message.
    tokio::spawn(async move {
        idle.await;
        let text = {
            let state = lock(&state);
            if !state.enabled {
                return;
            }
            state.text.clone()
        };
        let sent = runtime.message_actions().and_then(|actions| {
            actions.send_user_message(UserMessageContent::Text(text), SendUserMessageOptions::default())
        });
        if let Err(error) = sent {
            let mut state = lock(&state);
            state.enabled = false;
            ui.notify(&format!("auto-continue: {}", error.message), NotificationType::Error);
            ui.set_status(STATUS_KEY, Some(""));
        }
    });
}

pub struct AutoContinue;

impl Extension for AutoContinue {
    fn register(&self, api: &mut ExtensionApi) {
        let state: Shared = Arc::default();

        let input_state = Arc::clone(&state);
        api.on(
            EventKind::Input,
            Arc::new(move |event, _ctx| {
                let state = Arc::clone(&input_state);
                Box::pin(async move {
                    // Reset the counter whenever the user types something themselves.
                    if let ExtensionEvent::Input(input) = event
                        && input.source == InputSource::Interactive
                    {
                        lock(&state).count = 0;
                    }
                    Ok(EventResult::Input(InputEventResult::Continue))
                })
            }),
        );

        let end_state = Arc::clone(&state);
        let runtime = api.runtime.clone();
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let state = Arc::clone(&end_state);
                let runtime = runtime.clone();
                Box::pin(async move {
                    let event_aborted = matches!(event, ExtensionEvent::AgentEnd { aborted: Some(true), .. });
                    let aborted = event_aborted || ctx.signal.as_ref().is_some_and(|s| s.is_aborted());
                    let outcome = {
                        let mut guard = lock(&state);
                        let outcome = on_agent_end(&mut guard, aborted);
                        if outcome != EndOutcome::Idle {
                            update_status(ctx, &guard);
                        }
                        outcome
                    };
                    match outcome {
                        EndOutcome::Idle => {}
                        EndOutcome::DisabledByAbort => {
                            ctx.ui.notify("auto-continue disabled (turn aborted)", NotificationType::Info);
                        }
                        EndOutcome::ReachedMax(max) => {
                            ctx.ui.notify(&format!("auto-continue: reached max {max}, stopping"), NotificationType::Warning);
                        }
                        EndOutcome::Continue(_) => send_next(runtime, ctx, state),
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let on_state = Arc::clone(&state);
        api.register_command(
            "auto-continue-on",
            Some("Enable auto-continue.".into()),
            None,
            Arc::new(move |_args, ctx| {
                let state = Arc::clone(&on_state);
                Box::pin(async move {
                    let mut state = lock(&state);
                    state.enabled = true;
                    state.count = 0;
                    update_status(ctx, &state);
                    ctx.ui.notify(&format!("auto-continue ON (\"{}\")", state.text), NotificationType::Info);
                    Ok(())
                })
            }),
        );

        let off_state = Arc::clone(&state);
        api.register_command(
            "auto-continue-off",
            Some("Disable auto-continue.".into()),
            None,
            Arc::new(move |_args, ctx| {
                let state = Arc::clone(&off_state);
                Box::pin(async move {
                    let mut state = lock(&state);
                    state.enabled = false;
                    update_status(ctx, &state);
                    ctx.ui.notify("auto-continue OFF", NotificationType::Info);
                    Ok(())
                })
            }),
        );

        let set_state = Arc::clone(&state);
        api.register_command(
            "auto-continue-set",
            Some("Set message and enable.".into()),
            None,
            Arc::new(move |args, ctx| {
                let state = Arc::clone(&set_state);
                Box::pin(async move {
                    let trimmed = args.trim();
                    if trimmed.is_empty() {
                        ctx.ui.notify("Usage: /auto-continue-set <message>", NotificationType::Warning);
                        return Ok(());
                    }
                    let mut state = lock(&state);
                    state.text = trimmed.to_owned();
                    state.enabled = true;
                    state.count = 0;
                    update_status(ctx, &state);
                    ctx.ui.notify(&format!("auto-continue ON (\"{}\")", state.text), NotificationType::Info);
                    Ok(())
                })
            }),
        );

        api.register_command(
            "auto-continue-max",
            Some("Set iteration cap.".into()),
            None,
            Arc::new(move |args, ctx| {
                let state = Arc::clone(&state);
                Box::pin(async move {
                    let Some(max) = parse_max(args) else {
                        ctx.ui.notify("Usage: /auto-continue-max <positive integer>", NotificationType::Warning);
                        return Ok(());
                    };
                    let mut state = lock(&state);
                    state.max = max;
                    update_status(ctx, &state);
                    ctx.ui.notify(&format!("auto-continue max={max}"), NotificationType::Info);
                    Ok(())
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_counts_up_to_the_cap_then_stops() {
        let mut state = State { enabled: true, max: 2, ..State::default() };
        assert_eq!(on_agent_end(&mut state, false), EndOutcome::Continue("continue".into()));
        assert_eq!(on_agent_end(&mut state, false), EndOutcome::Continue("continue".into()));
        assert_eq!(state.status().as_deref(), Some("auto-continue 2/2"));
        assert_eq!(on_agent_end(&mut state, false), EndOutcome::ReachedMax(2));
        assert!(!state.enabled);
        assert_eq!(on_agent_end(&mut state, false), EndOutcome::Idle);
    }

    #[test]
    fn abort_disables_without_counting() {
        let mut state = State { enabled: true, ..State::default() };
        assert_eq!(on_agent_end(&mut state, true), EndOutcome::DisabledByAbort);
        assert_eq!((state.enabled, state.count), (false, 0));
        assert_eq!(state.status(), None);
    }

    #[test]
    fn max_parses_like_parse_int() {
        assert_eq!(parse_max(" 12 "), Some(12));
        assert_eq!(parse_max("7abc"), Some(7));
        assert_eq!(parse_max("+3"), Some(3));
        assert_eq!(parse_max("0"), None);
        assert_eq!(parse_max("-4"), None);
        assert_eq!(parse_max("x"), None);
    }
}
