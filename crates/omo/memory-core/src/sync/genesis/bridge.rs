use super::types::GenesisPrepared;
use crate::sync::{crypto::parse::Principal, recovery::ports::PortFuture};

pub struct GenesisBridgeRequest {
    pub account: String,
    pub rp_id: String,
    pub user_name: String,
    pub roaming: bool,
    pub first_device: Principal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisBridgeRefusal {
    Cancelled,
    NoPrf,
    NoUserVerification,
    NotSurvivable,
    CredentialReused,
    RpMismatch,
    KeyMismatch,
    Unavailable,
}

#[derive(Debug)]
pub enum GenesisBridgeResult {
    Prepared(Box<GenesisPrepared>),
    Refused(GenesisBridgeRefusal),
}

/// A recovery-phrase genesis: no RP id, user name or roaming; the surface shows its own account label.
pub struct PhraseGenesisBridgeRequest {
    pub account: String,
    pub first_device: Principal,
}

pub trait GenesisBridge: Send + Sync {
    fn prf_supported(&self) -> bool;
    /// True only where a protected phrase surface exists (descriptor `phraseGenesis: "supported"`, or the CLI tty).
    fn phrase_supported(&self) -> bool {
        false
    }
    fn create_genesis(&self, request: &GenesisBridgeRequest)
    -> PortFuture<'_, GenesisBridgeResult>;
    fn create_phrase_genesis(
        &self,
        _request: &PhraseGenesisBridgeRequest,
    ) -> PortFuture<'_, GenesisBridgeResult> {
        Box::pin(async {
            Ok(GenesisBridgeResult::Refused(
                GenesisBridgeRefusal::Unavailable,
            ))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisEnableReason {
    MalformedReply,
    IncompleteChain,
    UnusableKey,
    IntentMismatch,
    CorruptIntent,
    MissingConfiguration,
    BridgeRefused,
    AppendRefused,
}

#[derive(Debug)]
pub struct GenesisEnableError {
    pub reason: GenesisEnableReason,
    pub cause: Option<Box<dyn std::error::Error>>,
}

impl GenesisEnableError {
    pub fn new(reason: GenesisEnableReason) -> Self {
        Self {
            reason,
            cause: None,
        }
    }
    pub fn with_cause(
        reason: GenesisEnableReason,
        cause: impl std::error::Error + 'static,
    ) -> Self {
        Self {
            reason,
            cause: Some(Box::new(cause)),
        }
    }
    pub const fn label(&self) -> &'static str {
        match self.reason {
            GenesisEnableReason::MalformedReply => "malformed_reply",
            GenesisEnableReason::IncompleteChain => "incomplete_chain",
            GenesisEnableReason::UnusableKey => "unusable_key",
            GenesisEnableReason::IntentMismatch => "intent_mismatch",
            GenesisEnableReason::CorruptIntent => "corrupt_intent",
            GenesisEnableReason::MissingConfiguration => "missing_configuration",
            GenesisEnableReason::BridgeRefused => "bridge_refused",
            GenesisEnableReason::AppendRefused => "append_refused",
        }
    }
}

impl std::fmt::Display for GenesisEnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "first-device sync: {}", self.label())
    }
}

impl std::error::Error for GenesisEnableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref()
    }
}
