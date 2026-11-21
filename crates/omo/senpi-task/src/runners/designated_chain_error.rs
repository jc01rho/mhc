//! `runners/designated-chain-error.ts`: a child's engine did not take its designated model chain, so
//! the child is never prompted (omo #12759). `models` comes from the child's own spec, never from
//! engine output; the engine's refusal text stays in the message only.

use crate::manager::start_failure_cause::{StartCause, StartCauseClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignatedChainNotHeldReason {
    Unsupported,
    Refused,
    Timeout,
}

impl DesignatedChainNotHeldReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Refused => "refused",
            Self::Timeout => "timeout",
        }
    }
}

/// The start cause the manager reads back (`classifyStartCause` -> `chain_not_held`).
pub fn chain_not_held_cause(reason: DesignatedChainNotHeldReason, models: &[String]) -> StartCause {
    let named: Vec<String> = models
        .iter()
        .filter(|model| crate::manager::start_failure_cause::is_model_selector(model))
        .cloned()
        .collect();
    StartCause {
        cause_class: StartCauseClass::ChainNotHeld,
        timed_out_command: None,
        cause_code: Some(reason.as_str().to_string()),
        provider: None,
        designated_models: (!named.is_empty()).then_some(named),
    }
}
