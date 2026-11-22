//! The one execution gate for Claude Fable models (mhc divergence, docs/divergences.md GATE-1).
//!
//! Fable catalog rows, routing rungs and prompt presets are ported exactly as upstream ships them,
//! but while `model_policy.block_fable` is on (the default) no Fable model runs: a chain walk treats
//! a blocked model as unavailable and moves to the next rung, and a direct selection is refused
//! with [`blocked_model_message`]. Every caller asks [`is_execution_blocked`], so this module is
//! the only definition of "blocked".

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

/// The omo.json(c) setting that switches the gate (`"model_policy": { "block_fable": false }`).
pub const BLOCK_FABLE_SETTING: &str = "model_policy.block_fable";

static BLOCK_FABLE: AtomicBool = AtomicBool::new(true);

/// A Claude Fable model id in any spelling a catalog uses: `claude-fable-5-2`, `claude-fable-5.1`,
/// `anthropic/claude-fable-5-2`, `us.anthropic.claude-fable-5-1`, cursor's `claude-fable-5-thinking`.
pub fn is_fable_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude-fable")
}

/// The setting as an omo config view spells it; `None` when the view leaves it unset.
pub fn block_fable_setting(config: &Value) -> Option<bool> {
    config.get("model_policy")?.get("block_fable")?.as_bool()
}

/// Publishes a loaded omo config's setting for the selection paths that never see the config
/// (`/model`, `--model`, the senpi retry chain, the provider stream).
pub fn apply_config(config: &Value) {
    BLOCK_FABLE.store(
        block_fable_setting(config).unwrap_or(true),
        Ordering::Relaxed,
    );
}

/// The process-wide gate state: on until a loaded config turns it off.
pub fn fable_blocked() -> bool {
    BLOCK_FABLE.load(Ordering::Relaxed)
}

/// The gate state for an omo config view: its own setting, else the process-wide one.
pub fn fable_blocked_in(config: &Value) -> bool {
    block_fable_setting(config).unwrap_or_else(fable_blocked)
}

pub fn is_execution_blocked(model: &str, blocked: bool) -> bool {
    blocked && is_fable_model(model)
}

/// The one-line refusal for a direct selection of a blocked model.
pub fn blocked_model_message(model: &str) -> String {
    format!(
        "{model} is a Claude Fable model and does not run while {BLOCK_FABLE_SETTING} is on; set it to false in omo.json to allow Fable models."
    )
}

/// The refusal for running `provider/id` under the process-wide setting, if the gate blocks it.
pub fn execution_refusal(provider: &str, id: &str) -> Option<String> {
    is_execution_blocked(id, fable_blocked())
        .then(|| blocked_model_message(&format!("{provider}/{id}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_catalog_spelling_of_a_fable_id_is_a_fable_model() {
        for model in [
            "claude-fable-5-2",
            "claude-fable-5.1",
            "anthropic/claude-fable-5-2",
            "github-copilot/claude-fable-5.2",
            "us.anthropic.claude-fable-5-1",
            "openrouter/anthropic/claude-fable-5.2",
            "cursor/claude-fable-5-thinking-high",
            "Claude-Fable-5",
        ] {
            assert!(is_fable_model(model), "{model}");
        }
        for model in [
            "claude-opus-5-5",
            "claude-mythos-5",
            "anthropic/claude-sonnet-5-5",
            "kimi-k3.1-pro",
        ] {
            assert!(!is_fable_model(model), "{model}");
        }
    }

    #[test]
    fn the_gate_is_on_by_default() {
        assert!(fable_blocked());
        assert!(fable_blocked_in(&json!({})));
        assert!(is_execution_blocked(
            "anthropic/claude-fable-5-2",
            fable_blocked_in(&json!({}))
        ));
    }

    #[test]
    fn a_config_view_switches_the_gate_for_itself() {
        let off = json!({ "model_policy": { "block_fable": false } });
        let on = json!({ "model_policy": { "block_fable": true } });
        assert_eq!(block_fable_setting(&off), Some(false));
        assert_eq!(block_fable_setting(&json!({ "model_policy": {} })), None);
        assert!(!fable_blocked_in(&off));
        assert!(fable_blocked_in(&on));
        assert!(!is_execution_blocked("anthropic/claude-fable-5-2", false));
        assert!(!is_execution_blocked("anthropic/claude-opus-5-5", true));
    }

    #[test]
    fn the_process_wide_refusal_covers_fable_ids_only() {
        let refusal = execution_refusal("anthropic", "claude-fable-5-2").expect("refused");
        assert!(refusal.contains("anthropic/claude-fable-5-2"));
        assert_eq!(execution_refusal("anthropic", "claude-opus-5-5"), None);
    }

    #[test]
    fn the_refusal_names_the_model_and_the_setting() {
        let message = blocked_model_message("anthropic/claude-fable-5-2");
        assert!(message.contains("anthropic/claude-fable-5-2"));
        assert!(message.contains(BLOCK_FABLE_SETTING));
        assert!(!message.contains('\n'));
    }
}
