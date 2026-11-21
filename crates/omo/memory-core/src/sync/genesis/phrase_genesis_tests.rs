use super::{
    bridge::GenesisEnableReason,
    intent_store::{
        GenesisIntent, IntentOwner, IntentPrincipal, load_genesis_intent, store_genesis_intent,
    },
    phrase_gate::{
        PhraseGate, PhraseGateRefusal, ServiceFloor, ServiceFloorPort, phrase_creation_refusal,
        read_service_floor,
    },
    phrase_marker::{
        GenesisPhraseFacts, GenesisPhraseState, PhraseGenesisMarker, PhraseGenesisPlace,
        delete_phrase_genesis_marker, genesis_intent_present, genesis_phrase_state,
        write_phrase_genesis_marker,
    },
};
use crate::sync::{
    account::http::{AccountHttp, create_account_http},
    crypto::parse::{PHRASE_RECOVERY_RP_ID, Survivable, is_phrase_key, parse_body, parse_recovery},
    engine::{
        adapters::http_transport::{
            HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
        },
        ports::PortFuture,
    },
    keyring_port::DeviceKeyring,
    recovery::testing::{ACCOUNT, device},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc};

fn phrase_key(credential_seed: u8) -> Value {
    json!({
        "credential_id": URL_SAFE_NO_PAD.encode([credential_seed; 16]),
        "rp_id": PHRASE_RECOVERY_RP_ID,
        "enrollment_nonce": URL_SAFE_NO_PAD.encode([credential_seed; 32]),
        "sealing_public_key": URL_SAFE_NO_PAD.encode([credential_seed; 32]),
        "signing_public_key": URL_SAFE_NO_PAD.encode([credential_seed; 32]),
        "survivable": "phrase",
    })
}

fn device_principal() -> Value {
    json!({
        "kind": "device",
        "id": URL_SAFE_NO_PAD.encode([9u8; 16]),
        "sealing_public_key": URL_SAFE_NO_PAD.encode([3u8; 32]),
        "signing_public_key": URL_SAFE_NO_PAD.encode([4u8; 32]),
    })
}

fn genesis_body(key: Value) -> Value {
    json!({
        "kind": "genesis",
        "account": ACCOUNT,
        "epoch": 1,
        "bootstrap": {
            "account": ACCOUNT,
            "custody_nonce": URL_SAFE_NO_PAD.encode([5u8; 16]),
            "first_device": device_principal(),
            "recovery_key": key,
        },
        "custody_root": URL_SAFE_NO_PAD.encode([6u8; 32]),
        "countersign": URL_SAFE_NO_PAD.encode([7u8; 64]),
        "wraps_digest": URL_SAFE_NO_PAD.encode([8u8; 32]),
    })
}

#[test]
fn a_phrase_key_parses_as_the_phrase_kind() {
    let body = genesis_body(phrase_key(1));
    let parsed = parse_body(&body).expect("phrase genesis parses");
    let crate::sync::crypto::parse::ParsedBody::Genesis { recovery_key, .. } = parsed else {
        panic!("not a genesis body");
    };
    assert_eq!(recovery_key.survivable, Survivable::Phrase);
    assert_eq!(recovery_key.rp_id, PHRASE_RECOVERY_RP_ID);
    assert!(is_phrase_key(&recovery_key));
}

#[test]
fn the_passkey_kinds_still_parse_unchanged() {
    let passkey = json!({
        "credential_id": URL_SAFE_NO_PAD.encode([1u8; 16]),
        "rp_id": "recovery.omo.dev",
        "enrollment_nonce": URL_SAFE_NO_PAD.encode([2u8; 32]),
        "sealing_public_key": URL_SAFE_NO_PAD.encode([3u8; 32]),
        "signing_public_key": URL_SAFE_NO_PAD.encode([4u8; 32]),
        "survivable": "synced",
    });
    let key = parse_recovery(&passkey).expect("passkey key parses");
    assert_eq!(key.survivable, Survivable::Synced);
    assert_eq!(key.rp_id, "recovery.omo.dev");
    assert!(!is_phrase_key(&key));
}

#[test]
fn refused_kind_pairs_are_refused_at_every_boundary() {
    let pairs = [
        json!({"survivable": "phrase", "rp_id": "recovery.omo.dev"}),
        json!({"survivable": "synced", "rp_id": PHRASE_RECOVERY_RP_ID}),
        json!({"survivable": "roaming", "rp_id": PHRASE_RECOVERY_RP_ID}),
        json!({"survivable": "synced", "rp_id": "omo-sync/v2/other"}),
        json!({"survivable": "roaming", "rp_id": "omo-sync/"}),
        json!({"survivable": "paper", "rp_id": PHRASE_RECOVERY_RP_ID}),
    ];
    for pair in pairs {
        let mut key = phrase_key(1);
        for (field, value) in pair.as_object().expect("override") {
            key[field.as_str()] = value.clone();
        }
        assert!(parse_recovery(&key).is_none(), "override {pair} parsed");
        assert!(
            parse_body(&genesis_body(key)).is_none(),
            "override {pair} parsed"
        );
    }
}

#[test]
fn a_phrase_credential_id_must_be_sixteen_bytes() {
    for length in [15, 17, 32] {
        let mut key = phrase_key(1);
        key["credential_id"] = json!(URL_SAFE_NO_PAD.encode(vec![7u8; length]));
        assert!(parse_recovery(&key).is_none(), "{length}-byte id parsed");
        assert!(parse_body(&genesis_body(key)).is_none());
    }
}

fn place(dir: &std::path::Path) -> PhraseGenesisPlace {
    PhraseGenesisPlace {
        pin_dir: dir.to_path_buf(),
        account: ACCOUNT.into(),
        principal_id: URL_SAFE_NO_PAD.encode([9u8; 16]),
    }
}

fn marker(digest: Option<&str>) -> PhraseGenesisMarker {
    PhraseGenesisMarker {
        version: 1,
        account: ACCOUNT.into(),
        started_at: 1_700_000_000_000.0,
        genesis_digest: digest.map(str::to_owned),
    }
}

fn facts<'a>(
    intent_present: bool,
    pinned: Option<&'a str>,
    chain_empty: Option<bool>,
) -> GenesisPhraseFacts<'a> {
    GenesisPhraseFacts {
        intent_present,
        pinned_genesis_digest: pinned,
        chain_empty,
    }
}

#[test]
fn no_marker_or_a_pending_intent_reports_nothing() {
    let dir = tempfile::tempdir().expect("pin dir");
    let place = place(dir.path());
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, None, None)).expect("state"),
        GenesisPhraseState::None
    );
    write_phrase_genesis_marker(&place, &marker(None)).expect("marker write");
    assert_eq!(
        genesis_phrase_state(&place, &facts(true, None, Some(false))).expect("state"),
        GenesisPhraseState::None
    );
}

#[test]
fn a_shown_phrase_over_an_empty_chain_is_void() {
    let dir = tempfile::tempdir().expect("pin dir");
    let place = place(dir.path());
    write_phrase_genesis_marker(&place, &marker(None)).expect("marker write");
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, None, Some(true))).expect("state"),
        GenesisPhraseState::Void
    );
}

#[test]
fn a_marker_digest_equal_to_the_pinned_root_is_saved() {
    let dir = tempfile::tempdir().expect("pin dir");
    let place = place(dir.path());
    let digest = URL_SAFE_NO_PAD.encode([4u8; 32]);
    write_phrase_genesis_marker(&place, &marker(Some(digest.as_str()))).expect("marker write");
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, Some(digest.as_str()), Some(false)))
            .expect("state"),
        GenesisPhraseState::Saved
    );
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, Some("other"), Some(false))).expect("state"),
        GenesisPhraseState::Void
    );
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, None, Some(false))).expect("state"),
        GenesisPhraseState::Void
    );
}

#[test]
fn an_unreadable_marker_over_a_chain_is_unresolved_and_never_void() {
    let dir = tempfile::tempdir().expect("pin dir");
    let place = place(dir.path());
    write_phrase_genesis_marker(&place, &marker(Some("d"))).expect("marker write");
    std::fs::write(
        super::phrase_marker::phrase_genesis_marker_path(&place),
        b"{broken",
    )
    .expect("corrupt marker");
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, None, Some(false))).expect("state"),
        GenesisPhraseState::Unresolved
    );
}

#[test]
fn deleting_the_marker_returns_the_state_to_none() {
    let dir = tempfile::tempdir().expect("pin dir");
    let place = place(dir.path());
    write_phrase_genesis_marker(&place, &marker(Some("d"))).expect("marker write");
    delete_phrase_genesis_marker(&place).expect("marker delete");
    assert_eq!(
        genesis_phrase_state(&place, &facts(false, None, Some(false))).expect("state"),
        GenesisPhraseState::None
    );
    assert!(!genesis_intent_present(&place).expect("intent presence"));
}

struct StateService {
    body: RefCell<Option<Value>>,
}

impl HttpClient for StateService {
    fn send(&self, _request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        let body = self.body.borrow().clone();
        Box::pin(async move {
            let (status, value) = match body {
                Some(value) => (200, value),
                None => (500, json!({"error":{"code":"unavailable"}})),
            };
            Ok(HttpResponse {
                status,
                body: value.to_string().into_bytes(),
            })
        })
    }
    fn open_stream(&self, _request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(crate::sync::engine::error::EngineError::Network) })
    }
}

fn account_http(body: Option<Value>) -> AccountHttp {
    create_account_http(HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
        client: Rc::new(StateService {
            body: RefCell::new(body),
        }),
    })
}

fn state(accepts: Option<bool>) -> Value {
    let mut value = json!({
        "head": {"seq": 1, "link": "link", "keyring_link": "keyring"},
        "current_epoch": 1,
        "rotation_pending": false,
    });
    if let Some(accepts) = accepts {
        value["accepts_phrase_keys"] = json!(accepts);
    }
    value
}

struct FixedFloor(ServiceFloor);
impl ServiceFloorPort for FixedFloor {
    fn read<'a>(
        &'a self,
        _chain: &'a AccountHttp,
    ) -> Pin<Box<dyn Future<Output = ServiceFloor> + 'a>> {
        Box::pin(async move { self.0 })
    }
}

struct StalledFloor;
impl ServiceFloorPort for StalledFloor {
    fn read<'a>(
        &'a self,
        _chain: &'a AccountHttp,
    ) -> Pin<Box<dyn Future<Output = ServiceFloor> + 'a>> {
        Box::pin(std::future::pending::<ServiceFloor>())
    }
}

#[tokio::test]
async fn the_service_floor_reads_accepts_phrase_keys() {
    assert_eq!(
        read_service_floor(&account_http(Some(state(Some(true))))).await,
        ServiceFloor::On
    );
    assert_eq!(
        read_service_floor(&account_http(Some(state(Some(false))))).await,
        ServiceFloor::Off
    );
    assert_eq!(
        read_service_floor(&account_http(Some(state(None)))).await,
        ServiceFloor::Off
    );
    assert_eq!(
        read_service_floor(&account_http(None)).await,
        ServiceFloor::Unknown
    );
}

#[tokio::test]
async fn phrase_creation_is_refused_without_a_gate_or_with_the_flag_off() {
    let chain = account_http(Some(state(Some(true))));
    assert_eq!(
        phrase_creation_refusal(None, ACCOUNT, &chain).await,
        Some(PhraseGateRefusal::Disabled)
    );
    let on = FixedFloor(ServiceFloor::On);
    let disabled = PhraseGate {
        enabled: false,
        service_floor: Some(&on),
        floor_timeout_ms: None,
        refused: None,
    };
    assert_eq!(
        phrase_creation_refusal(Some(&disabled), ACCOUNT, &chain).await,
        Some(PhraseGateRefusal::Disabled)
    );
}

#[tokio::test]
async fn phrase_creation_is_refused_unless_the_floor_is_positively_on() {
    let chain = account_http(Some(state(Some(false))));
    let logged = RefCell::new(Vec::new());
    let log = |reason: PhraseGateRefusal, account: &str| {
        logged.borrow_mut().push((reason, account.to_owned()));
    };
    for floor in [ServiceFloor::Off, ServiceFloor::Unknown] {
        let port = FixedFloor(floor);
        let gate = PhraseGate {
            enabled: true,
            service_floor: Some(&port),
            floor_timeout_ms: None,
            refused: Some(&log),
        };
        assert_eq!(
            phrase_creation_refusal(Some(&gate), ACCOUNT, &chain).await,
            Some(PhraseGateRefusal::ServiceFloorOff)
        );
    }
    let gate = PhraseGate {
        enabled: true,
        service_floor: None,
        floor_timeout_ms: None,
        refused: Some(&log),
    };
    assert_eq!(
        phrase_creation_refusal(Some(&gate), ACCOUNT, &chain).await,
        Some(PhraseGateRefusal::ServiceFloorOff)
    );
    assert_eq!(
        logged.borrow().as_slice(),
        [
            (PhraseGateRefusal::ServiceFloorOff, ACCOUNT.to_owned()),
            (PhraseGateRefusal::ServiceFloorOff, ACCOUNT.to_owned()),
            (PhraseGateRefusal::ServiceFloorOff, ACCOUNT.to_owned()),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_stalled_floor_read_counts_as_unknown() {
    let chain = account_http(Some(state(Some(true))));
    let gate = PhraseGate {
        enabled: true,
        service_floor: Some(&StalledFloor),
        floor_timeout_ms: Some(1),
        refused: None,
    };
    assert_eq!(
        phrase_creation_refusal(Some(&gate), ACCOUNT, &chain).await,
        Some(PhraseGateRefusal::ServiceFloorOff)
    );
}

#[tokio::test]
async fn phrase_creation_proceeds_only_on_an_on_floor() {
    let chain = account_http(Some(state(Some(true))));
    let gate = PhraseGate {
        enabled: true,
        service_floor: Some(&FixedStateFloor),
        floor_timeout_ms: None,
        refused: None,
    };
    assert_eq!(
        phrase_creation_refusal(Some(&gate), ACCOUNT, &chain).await,
        None
    );
}

struct FixedStateFloor;
impl ServiceFloorPort for FixedStateFloor {
    fn read<'a>(
        &'a self,
        chain: &'a AccountHttp,
    ) -> Pin<Box<dyn Future<Output = ServiceFloor> + 'a>> {
        Box::pin(read_service_floor(chain))
    }
}

fn intent(digest: &str) -> GenesisIntent {
    let keyring = device(9);
    let principal = keyring.principal();
    let keys = keyring.public_keys();
    GenesisIntent {
        version: 1,
        account: ACCOUNT.into(),
        service_origin: Some("https://sync.example.test".into()),
        service_device_id: "service-device".into(),
        sync_principal: IntentPrincipal {
            kind: principal.kind.into(),
            id: principal.id,
            sealing_public_key: keys.sealing,
            signing_public_key: keys.signing,
        },
        genesis_digest: digest.into(),
        request: json!({"record": {"kind": "genesis"}, "base_seq": 0}),
    }
}

#[test]
fn a_stored_intent_round_trips_and_retires() {
    let dir = tempfile::tempdir().expect("pin dir");
    let keyring = device(9);
    let owner = IntentOwner {
        account: ACCOUNT,
        service_origin: Some("https://sync.example.test"),
        service_device_id: "service-device",
        keyring: &keyring,
        pin_dir: dir.path(),
    };
    let digest = URL_SAFE_NO_PAD.encode([2u8; 32]);
    assert!(load_genesis_intent(&owner).expect("load").is_none());
    store_genesis_intent(&owner, &intent(&digest)).expect("stored intent");
    let loaded = load_genesis_intent(&owner).expect("load").expect("stored");
    assert_eq!(loaded.genesis_digest, digest);
    super::intent_store::retire_genesis_intent(&owner).expect("retired");
    assert!(load_genesis_intent(&owner).expect("load").is_none());
}

#[test]
fn an_intent_about_another_device_is_refused_not_resumed() {
    let dir = tempfile::tempdir().expect("pin dir");
    let keyring = device(9);
    let owner = IntentOwner {
        account: ACCOUNT,
        service_origin: Some("https://sync.example.test"),
        service_device_id: "service-device",
        keyring: &keyring,
        pin_dir: dir.path(),
    };
    let mut foreign = intent("digest");
    foreign.account = "another-account".into();
    let raw = serde_json::to_string(&foreign).expect("intent json");
    let error = super::intent_store::parse_genesis_intent(&raw, &owner).expect_err("refused");
    assert_eq!(error.reason, GenesisEnableReason::IntentMismatch);
    let corrupt =
        super::intent_store::parse_genesis_intent("{broken", &owner).expect_err("refused");
    assert_eq!(corrupt.reason, GenesisEnableReason::CorruptIntent);
}

#[test]
fn a_bridge_without_a_phrase_surface_refuses_phrase_genesis() {
    use super::bridge::GenesisBridge;
    assert!(!NoBridge.phrase_supported());
    assert!(!NoBridge.prf_supported());
}

struct NoBridge;
impl super::bridge::GenesisBridge for NoBridge {
    fn prf_supported(&self) -> bool {
        false
    }
    fn create_genesis(
        &self,
        _request: &super::bridge::GenesisBridgeRequest,
    ) -> crate::sync::recovery::ports::PortFuture<'_, super::bridge::GenesisBridgeResult> {
        Box::pin(async {
            Ok(super::bridge::GenesisBridgeResult::Refused(
                super::bridge::GenesisBridgeRefusal::Unavailable,
            ))
        })
    }
}
