//! Port of senpi packages/coding-agent/src/core/extensions/tool-hook-wait.ts (senpi#7069,
//! oh-my-openagent#14354).
//!
//! How long the host waits on one tool hook handler. A `tool_call` / `tool_result` handler runs in-process;
//! one that never settles used to hold the tool loop forever, because the wait had no deadline and ignored the
//! turn's abort signal, so neither Esc nor an RPC interrupt ended it. The wait now ends when the handler
//! settles, when the turn is aborted, or (for `tool_result`) at a deadline.
//!
//! Native difference: a JavaScript handler the host stopped waiting on keeps running detached and whatever it
//! later returns or throws is discarded. A native handler future borrows the emission's event and context, so
//! the host drops it instead: the handler is cancelled at its current await point and has no late outcome
//! (see `handler_budget`). The `failed` outcome has no counterpart either: a handler future settles with its
//! own `Result`, which the caller receives as the `Settled` value.

use std::future::Future;
use std::time::Duration;

use maho_ext_api::{AbortSignal, ExtensionContext};

/// senpi `SELF_BOUNDED_TOOL_HOOK_PATH`: the builtin hooks extension runs user command hooks under their own
/// configured timeout, so it gets no host deadline.
pub const SELF_BOUNDED_TOOL_HOOK_PATH: &str = "<builtin:hooks>";

/// senpi `DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS`: the deadline of one `tool_result` handler of an extension
/// that does not bound itself.
pub const DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS: u64 = 120_000;

/// Environment names of the `tool_result` deadline override, most specific first: the brand prefix, then
/// senpi's own `SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS` (`brand_env_names` order; this crate sits below
/// `maho-core`, so the two names are spelled out).
pub const TOOL_RESULT_HANDLER_TIMEOUT_ENVS: [&str; 2] = [
    "MAHO_TOOL_RESULT_HANDLER_TIMEOUT_MS",
    "SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS",
];

/// senpi `toolResultHandlerTimeoutMs`: the `tool_result` deadline for an extension, in milliseconds. 0 means
/// none: a self-bounded extension has no deadline, and an override of 0 disables it. Otherwise the first
/// defined override applies, and an empty or invalid one falls back to the default.
pub fn tool_result_handler_timeout_ms(
    extension_path: &str,
    env: impl Fn(&str) -> Option<String>,
) -> u64 {
    if extension_path == SELF_BOUNDED_TOOL_HOOK_PATH {
        return 0;
    }
    let Some(raw) = TOOL_RESULT_HANDLER_TIMEOUT_ENVS
        .iter()
        .find_map(|name| env(name))
        .filter(|raw| !raw.trim().is_empty())
    else {
        return DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS;
    };
    match raw.trim().parse::<f64>() {
        Ok(parsed) if parsed.is_finite() && parsed >= 0.0 => parsed as u64,
        _ => DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS,
    }
}

/// The signal a tool hook's wait follows: the turn's own, and none while the agent is idle. Senpi's `ctx.signal`
/// is undefined outside a run, but the session's signal stays aborted after an Esc until the next turn builds a
/// new one, so an idle context (a detached eval cell calling a tool) must not read that stale abort as its own.
pub fn running_turn_signal(context: &ExtensionContext) -> Option<&AbortSignal> {
    if context.is_idle() {
        None
    } else {
        context.signal.as_ref()
    }
}

/// How one wait on a tool hook handler ended (senpi `ToolHookWaitOutcome`).
#[derive(Debug, PartialEq, Eq)]
pub enum ToolHookWaitOutcome<T> {
    /// The handler settled first; its own success or failure is the value.
    Settled(T),
    /// The deadline passed first; the handler was dropped.
    TimedOut { elapsed_ms: u64 },
    /// The turn's abort signal fired first, or had already fired; the handler was dropped.
    Aborted,
}

/// senpi `waitForToolHook`: waits on one handler until it settles, `signal` aborts or `timeout_ms` passes,
/// whichever comes first. `timeout_ms` 0 waits without a deadline. A signal that has already aborted ends the
/// wait before the handler is polled.
pub async fn wait_for_tool_hook<T>(
    handler: impl Future<Output = T>,
    signal: Option<&AbortSignal>,
    timeout_ms: u64,
) -> ToolHookWaitOutcome<T> {
    let started_at = tokio::time::Instant::now();
    if signal.is_some_and(AbortSignal::is_aborted) {
        return ToolHookWaitOutcome::Aborted;
    }
    if signal.is_none() && timeout_ms == 0 {
        return ToolHookWaitOutcome::Settled(handler.await);
    }
    let aborted = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(handler);
    tokio::pin!(aborted);
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        value = &mut handler => ToolHookWaitOutcome::Settled(value),
        () = &mut aborted => ToolHookWaitOutcome::Aborted,
        () = &mut deadline, if timeout_ms > 0 => ToolHookWaitOutcome::TimedOut {
            elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_timely_handler_settles_with_its_value_or_its_own_failure() {
        assert_eq!(
            wait_for_tool_hook(async { Ok::<_, &str>("ok") }, None, 60_000).await,
            ToolHookWaitOutcome::Settled(Ok("ok"))
        );
        assert_eq!(
            wait_for_tool_hook(async { Err::<&str, _>("boom") }, None, 60_000).await,
            ToolHookWaitOutcome::Settled(Err("boom"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_handler_that_never_settles_times_out_at_the_deadline() {
        let outcome = wait_for_tool_hook(std::future::pending::<()>(), None, 100).await;
        assert!(
            matches!(outcome, ToolHookWaitOutcome::TimedOut { elapsed_ms } if elapsed_ms >= 100),
            "{outcome:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_deadline_waits_for_the_handler_without_one() {
        let signal = AbortSignal::default();
        let outcome = wait_for_tool_hook(
            async {
                tokio::time::sleep(Duration::from_secs(3_600)).await;
                7
            },
            Some(&signal),
            0,
        )
        .await;
        assert_eq!(outcome, ToolHookWaitOutcome::Settled(7));
    }

    #[tokio::test]
    async fn a_signal_that_already_aborted_never_starts_the_handler() {
        let signal = AbortSignal::default();
        signal.abort();
        let ran = AtomicBool::new(false);
        let outcome = wait_for_tool_hook(
            async {
                ran.store(true, Ordering::SeqCst);
            },
            Some(&signal),
            0,
        )
        .await;
        assert_eq!(outcome, ToolHookWaitOutcome::Aborted);
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn an_abort_ends_the_wait_on_a_handler_that_never_settles() {
        let signal = AbortSignal::default();
        let (outcome, ()) = tokio::join!(
            wait_for_tool_hook(std::future::pending::<()>(), Some(&signal), 0),
            async { signal.abort() },
        );
        assert_eq!(outcome, ToolHookWaitOutcome::Aborted);
    }

    #[test]
    fn deadlines_follow_the_extension_and_the_environment() {
        let senpi = [("SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS", "5")];
        assert_eq!(
            tool_result_handler_timeout_ms(SELF_BOUNDED_TOOL_HOOK_PATH, env(&senpi)),
            0
        );
        assert_eq!(
            tool_result_handler_timeout_ms("/ext/other.ts", env(&senpi)),
            5
        );
        assert_eq!(
            tool_result_handler_timeout_ms("/ext/other.ts", env(&[])),
            DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS
        );
    }

    #[test]
    fn the_brand_override_wins_and_zero_disables_the_deadline() {
        let both = [
            ("SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS", "5"),
            ("MAHO_TOOL_RESULT_HANDLER_TIMEOUT_MS", "7"),
        ];
        assert_eq!(
            tool_result_handler_timeout_ms("/ext/other.ts", env(&both)),
            7
        );
        let zero = [("MAHO_TOOL_RESULT_HANDLER_TIMEOUT_MS", "0")];
        assert_eq!(
            tool_result_handler_timeout_ms("/ext/other.ts", env(&zero)),
            0
        );
    }

    #[test]
    fn an_empty_or_invalid_override_falls_back_to_the_default() {
        for raw in ["", "  ", "soon", "-1", "NaN", "Infinity"] {
            let pairs = [("SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS", raw)];
            assert_eq!(
                tool_result_handler_timeout_ms("/ext/other.ts", env(&pairs)),
                DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS,
                "{raw:?}"
            );
        }
    }
}
