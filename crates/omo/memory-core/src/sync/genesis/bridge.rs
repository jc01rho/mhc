//! The genesis bridge contract (A3 part 2a, #13335): what a protected surface (the CLI terminal,
//! the desktop) hands the engine to build a first-device genesis, and the closed set of refusals a
//! surface may answer. The bridge itself lives with the surface; this module keeps its types and the
//! enable error beside the genesis builder.

use super::types::GenesisPrepared;
use crate::sync::crypto::parse::Principal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisBridgeRequest {
    pub account: String,
    pub rp_id: String,
    pub user_name: String,
    pub roaming: bool,
    pub first_device: Principal,
}

/// A recovery-phrase genesis: no RP id, user name or roaming; the surface shows its own account label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseGenesisBridgeRequest {
    pub account: String,
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

impl GenesisBridgeRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::NoPrf => "no-prf",
            Self::NoUserVerification => "no-user-verification",
            Self::NotSurvivable => "not-survivable",
            Self::CredentialReused => "credential-reused",
            Self::RpMismatch => "rp-mismatch",
            Self::KeyMismatch => "key-mismatch",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenesisBridgeResult {
    Prepared(Box<GenesisPrepared>),
    Refused(GenesisBridgeRefusal),
}

/// A protected surface that can prepare a genesis. Implemented by the surface, never by this crate.
pub trait GenesisBridge {
    fn prf_supported(&self) -> bool;
    /// True only where a protected phrase surface exists (a descriptor stating support, or a tty).
    fn phrase_supported(&self) -> bool {
        false
    }
    fn create_genesis(&self, request: GenesisBridgeRequest) -> GenesisBridgeResult {
        let _ = request;
        GenesisBridgeResult::Refused(GenesisBridgeRefusal::Unavailable)
    }
    fn create_phrase_genesis(&self, request: PhraseGenesisBridgeRequest) -> GenesisBridgeResult {
        let _ = request;
        GenesisBridgeResult::Refused(GenesisBridgeRefusal::Unavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisEnableError {
    MalformedReply,
    IncompleteChain,
    UnusableKey,
    IntentMismatch,
    CorruptIntent,
    MissingConfiguration,
    BridgeRefused,
    AppendRefused,
}

impl GenesisEnableError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedReply => "malformed_reply",
            Self::IncompleteChain => "incomplete_chain",
            Self::UnusableKey => "unusable_key",
            Self::IntentMismatch => "intent_mismatch",
            Self::CorruptIntent => "corrupt_intent",
            Self::MissingConfiguration => "missing_configuration",
            Self::BridgeRefused => "bridge_refused",
            Self::AppendRefused => "append_refused",
        }
    }
}

impl std::fmt::Display for GenesisEnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "first-device sync: {}", self.as_str())
    }
}

impl std::error::Error for GenesisEnableError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct PasskeyOnlyBridge;
    impl GenesisBridge for PasskeyOnlyBridge {
        fn prf_supported(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_bridge_without_a_phrase_surface_refuses_phrase_genesis() {
        let bridge = PasskeyOnlyBridge;
        assert!(bridge.prf_supported());
        assert!(!bridge.phrase_supported());
        let outcome = bridge.create_phrase_genesis(PhraseGenesisBridgeRequest {
            account: "acct".into(),
            first_device: Principal {
                kind: "device".into(),
                id: "dev".into(),
                sealing_public_key: "seal".into(),
                signing_public_key: "sign".into(),
            },
        });
        assert_eq!(
            outcome,
            GenesisBridgeResult::Refused(GenesisBridgeRefusal::Unavailable)
        );
    }
}
