//! The window a subscription request gets (senpi `context-window.ts`).

/// Claude Code's default window when a request does not ask for the long context.
pub const CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// The most of a request's output budget Claude Code holds back from its window (`zoo` in Claude Code 2.1.321).
const CLAUDE_CODE_OUTPUT_RESERVE_CAP: u64 = 20_000;

/// The margin Claude Code keeps below its reserved window before it refuses a request (`Woo` in Claude Code 2.1.321).
const CLAUDE_CODE_BLOCKING_BUFFER: u64 = 3_000;

/// The `CLAUDE_CODE_MAX_CONTEXT_TOKENS` that makes Claude Code accept a conversation up to `window` (senpi#6303).
///
/// With `DISABLE_COMPACT` set, Claude Code takes that variable as the model's window, then refuses a request on its
/// own, with no API call, once the conversation passes the window minus its output reserve (the request's output
/// budget, capped at 20k) minus a 3k buffer. Handing it the bare planned window put that limit 23k below the window
/// compaction plans against, so every turn between the two failed in milliseconds with "Prompt is too long". The
/// variable therefore carries the window plus Claude Code's reserve, and Claude Code's limit lands on `window` itself.
///
/// `output_tokens` is the output budget Claude Code reserves for the request. Without an explicit
/// `CLAUDE_CODE_MAX_OUTPUT_TOKENS` it is the model's own default, which is at least the cap for every Claude model,
/// so `None` means the cap.
pub fn claude_code_max_context_tokens(window: u64, output_tokens: Option<u64>) -> u64 {
    let reserve = output_tokens
        .unwrap_or(CLAUDE_CODE_OUTPUT_RESERVE_CAP)
        .min(CLAUDE_CODE_OUTPUT_RESERVE_CAP);
    window + reserve + CLAUDE_CODE_BLOCKING_BUFFER
}

/// Models whose 1M window is reachable only through Claude Code's `[1m]` opt-in. Every other model above
/// 200k has a native 1M window and is requested by its plain id (senpi#3801).
pub const LEGACY_LONG_CONTEXT_MODEL_IDS: [&str; 5] = [
    "claude-3-7-sonnet",
    "claude-opus-4-6",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
];

fn opens_legacy_long_context(id: &str, context_window: u64) -> bool {
    context_window > CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW
        && LEGACY_LONG_CONTEXT_MODEL_IDS.contains(&id)
}

/// The window a request gets, which is what compaction plans against: only a legacy id loses 1M on a refusal.
pub fn planned_context_window(id: &str, context_window: u64, long_context: bool) -> u64 {
    if opens_legacy_long_context(id, context_window) && !long_context {
        CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW
    } else {
        context_window
    }
}

/// `planned_context_window` against the persisted refusal state.
pub fn planned_context_window_now(id: &str, context_window: u64) -> u64 {
    planned_context_window(
        id,
        context_window,
        crate::long_context::long_context_available(),
    )
}

/// The model id Claude Code is given: `[1m]` only for legacy ids while the account has not refused it.
pub fn sdk_model_id(id: &str, context_window: u64, long_context: bool) -> String {
    if opens_legacy_long_context(id, context_window) && long_context {
        format!("{id}[1m]")
    } else {
        id.to_owned()
    }
}

pub fn sdk_model_id_now(id: &str, context_window: u64) -> String {
    sdk_model_id(
        id,
        context_window,
        crate::long_context::long_context_available(),
    )
}

/// Whether a built request asked for the legacy long-context window, the only request it can be refused for.
pub fn asks_legacy_long_context(sdk_model: Option<&str>) -> bool {
    sdk_model.is_some_and(|model| model.ends_with("[1m]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_legacy_ids_use_the_opt_in() {
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 1_000_000, true),
            "claude-sonnet-4-5[1m]"
        );
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 1_000_000, false),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            sdk_model_id("claude-opus-5-5", 1_000_000, true),
            "claude-opus-5-5"
        );
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 200_000, true),
            "claude-sonnet-4-5"
        );
    }
    #[test]
    fn refusal_caps_only_legacy_ids() {
        assert_eq!(
            planned_context_window("claude-opus-4-6", 1_000_000, false),
            200_000
        );
        assert_eq!(
            planned_context_window("claude-opus-4-6", 1_000_000, true),
            1_000_000
        );
        assert_eq!(
            planned_context_window("claude-opus-5-5", 1_000_000, false),
            1_000_000
        );
        assert!(asks_legacy_long_context(Some("claude-opus-4-6[1m]")));
        assert!(!asks_legacy_long_context(Some("claude-opus-4-6")));
        assert!(!asks_legacy_long_context(None));
    }
    /// Claude Code 2.1.321 refuses a request once the conversation passes `window - min(maxOutputTokens, 20_000) -
    /// 3_000`; spelled out here so the test pins Claude Code's formula instead of this module's copy of it.
    fn claude_code_refuses_above(max_context_tokens: u64, max_output_tokens: u64) -> u64 {
        max_context_tokens - max_output_tokens.min(20_000) - 3_000
    }
    #[test]
    fn claude_code_refuses_a_request_only_past_the_window_that_is_planned() {
        for window in [200_000_u64, 1_000_000] {
            for max_output_tokens in [20_000_u64, 64_000, 128_000] {
                assert_eq!(
                    claude_code_refuses_above(
                        claude_code_max_context_tokens(window, None),
                        max_output_tokens
                    ),
                    window
                );
            }
        }
        assert_eq!(claude_code_max_context_tokens(200_000, None), 223_000);
    }
    #[test]
    fn a_request_budget_below_the_cap_reserves_only_what_it_asks_for() {
        assert_eq!(
            claude_code_max_context_tokens(200_000, Some(8_192)),
            211_192
        );
        assert_eq!(
            claude_code_max_context_tokens(200_000, Some(64_000)),
            223_000
        );
    }
}
