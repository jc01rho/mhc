//! Genesis keyring builder: native-prepared bootstrap, validation and the signed first append.

pub mod bridge;
pub mod build;
pub mod intent_store;
pub mod phrase_gate;
pub mod phrase_marker;
pub mod prepare;
pub mod types;
pub mod validate;

#[cfg(test)]
mod phrase_genesis_tests;
#[cfg(test)]
mod vector_tests;

pub use bridge::{
    GenesisBridge, GenesisBridgeRefusal, GenesisBridgeRequest, GenesisBridgeResult,
    GenesisEnableError, GenesisEnableReason, PhraseGenesisBridgeRequest,
};
pub use build::build_genesis;
pub use intent_store::{
    GenesisIntent, IntentOwner, IntentPrincipal, genesis_intent_location, load_genesis_intent,
    parse_genesis_intent, retire_genesis_intent, store_genesis_intent, with_genesis_lock,
};
pub use phrase_gate::{
    AccountStateFloor, PhraseGate, PhraseGateRefusal, ServiceFloor, phrase_creation_refusal,
    read_service_floor,
};
pub use phrase_marker::{
    GenesisPhraseFacts, GenesisPhraseState, PhraseGenesisMarker, PhraseGenesisPlace,
    delete_phrase_genesis_marker, genesis_intent_present, genesis_lock_held_by_live_owner,
    genesis_phrase_state, write_phrase_genesis_marker,
};
pub use prepare::prepare_genesis;
pub use types::{
    GenesisAppend, GenesisBody, GenesisBootstrap, GenesisBuildError, GenesisCapabilities,
    GenesisCountersignPayload, GenesisPrepared,
};
pub use validate::{validate_bootstrap, validate_prepared};
