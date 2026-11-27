//! Ultrawork keyword detection, session arming, and directive delivery.
//!
//! Built from the clean-room specification in `docs/cleanroom/specs/ultrawork.md`;
//! the provenance record is in `PROVENANCE.md`.

pub mod advice;
pub mod classify;
pub mod engine;
pub mod extension;
pub mod mask;
mod mask_phrases;
pub mod state;
pub mod texts;

pub use advice::mask_keyword_advice;
pub use classify::{
    Classification, ClassifyInput, Stage, classify, classify_for_session,
    classify_with_session_role, has_visible_keyword, visible_keyword_spans,
};
pub use engine::{Engine, InputOutcome, InputRequest, TodoResultView};
pub use extension::{
    DISABLE_FLAG, KeywordDisabledFn, ModelContractFn, UltraworkExtension,
    keyword_activation_allowed,
};
pub use state::{SessionArming, Sessions};
pub use texts::{CORE_CONTRACT, DIRECTIVE_MESSAGE_TYPE, REMINDER_MESSAGE_TYPE, mass_block};

/// Revision of the embedded contract. Increments whenever the contract text changes; a session
/// armed under an older revision is re-armed with the full contract on its next mention.
pub const fn contract_revision() -> u64 {
    1
}
