//! Port of oh-my-openagent `script/sdk-parity/authority-contract.test.ts` and the contract-table cases of
//! `contract.test.ts` (v5.1.84, slices P1a #14948 and P1a-s #14949) that hold over the contract's own data. The
//! cases that read the TS registry rows, the scenario files, the security design document or the engine snapshot stay
//! with the TS parity tooling.

use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use omo_sdk_contract::behaviour_changes::{Exists, ExpectedOutcome, behaviour_change};
use omo_sdk_contract::codes::{code_entry, held_code};
use omo_sdk_contract::inputs::custody::AccountAdopt;
use omo_sdk_contract::namespaces::is_namespace;
use omo_sdk_contract::{
    AUTH_ACCOUNT_CHECK_CODES, AUTH_CHECK_FREE_FIELDS, BEHAVIOUR_CHANGES, BehaviourChange,
    CUSTODY_RESULT_CALLS, Cell, ChangeId, FreeFieldOrigin, HELD_CODES, OMO_CODE_REGISTRY,
    OMO_SDK_CODES, PRINCIPAL_KINDS, SESSION_RESULT_CALLS, asserted_expectation,
    custody_answer_accepts, custody_input_accepts, is_principal, principal_matrix, refusals,
    session_answer_accepts,
};
use serde_json::{Value, json};

const CANARY: &str = "omo-canary-sk-0000";
const KEY: &str = "sk-ant-api03-omo-canary-0000";
const ADDRESS: &str = "sk-ant-api03-omo-canary-0000@leak.test";

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|path| (*path).to_string()).collect()
}

/// Every key path of a JSON value, as "accounts[].check.message"; an array adds its own "name[]" path.
fn field_paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.insert(path.clone());
                field_paths(child, &path, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{prefix}[]");
            out.insert(path.clone());
            for item in items {
                field_paths(item, &path, out);
            }
        }
        _ => {}
    }
}

fn paths_of(samples: &[Value]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for sample in samples {
        field_paths(sample, "", &mut out);
    }
    out
}

/// Every string leaf of a JSON value: its field path and its JSON pointer.
fn string_leaves(value: &Value, path: &str, pointer: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(_) => out.push((path.to_string(), pointer.to_string())),
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                string_leaves(child, &child_path, &format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                string_leaves(
                    item,
                    &format!("{path}[]"),
                    &format!("{pointer}/{index}"),
                    out,
                );
            }
        }
        _ => {}
    }
}

/// The string leaves of `sample` that still pass `accepts` with free text in them: enums and literals do not.
fn free_string_paths(sample: &Value, accepts: impl Fn(&Value) -> bool) -> BTreeSet<String> {
    let mut leaves = Vec::new();
    string_leaves(sample, "", "", &mut leaves);
    let mut free = BTreeSet::new();
    for (path, pointer) in leaves {
        let mut copy = sample.clone();
        *copy.pointer_mut(&pointer).expect("leaf pointer") = json!(KEY);
        if accepts(&copy) {
            free.insert(path);
        }
    }
    free
}

/// A copy of `answer` with `value` set at `path`, creating the objects on the way; "[]" addresses the first element.
fn plant_at(answer: &Value, path: &str, value: &str) -> Value {
    let mut copy = answer.clone();
    let steps: Vec<&str> = path.split('.').collect();
    let mut cursor: &mut Value = &mut copy;
    for (index, raw) in steps.iter().enumerate() {
        let name = raw.trim_end_matches("[]");
        let last = index + 1 == steps.len();
        let current = cursor;
        if !current.is_object() {
            *current = json!({});
        }
        let object = current.as_object_mut().expect("an object on the path");
        if raw.ends_with("[]") {
            let list = object.entry(name).or_insert_with(|| json!([{}]));
            if list.as_array().is_none_or(Vec::is_empty) {
                *list = json!([{}]);
            }
            cursor = list.get_mut(0).expect("the first element");
        } else if last {
            object.insert(name.to_string(), json!(value));
            break;
        } else {
            let next = object.entry(name).or_insert_with(|| json!({}));
            if !next.is_object() {
                *next = json!({});
            }
            cursor = next;
        }
    }
    copy
}

fn with_extra_field(sample: &Value) -> Value {
    let mut copy = sample.clone();
    copy.as_object_mut()
        .expect("an object answer")
        .insert("leaked".to_string(), json!(CANARY));
    copy
}

fn device() -> Value {
    json!({ "id": "dev-1", "name": "studio" })
}

fn terminal() -> Value {
    json!({
        "kind": "terminal",
        "tty": "/dev/pts/3",
        "session_id": 0,
        "leader_pid": 4100,
        "leader_started_at": "2026-11-25T00:00:00Z",
        "host_identity": "host-a",
        "host_pid": 4000,
        "host_started_at": "2026-11-25T00:00:00Z",
        "uid": 501
    })
}

fn peer() -> Value {
    json!({
        "kind": "peer",
        "executable_identity": "sha256:0000",
        "pid": 4242,
        "started_at": "2026-11-25T00:00:00Z",
        "allowlist_entry": "desktop"
    })
}

fn keyring() -> Value {
    json!({ "device_id": "dev-1", "signing_public_key": "sig", "sealing_public_key": "seal" })
}

/// A person's engine.auth.check answer with every optional field present.
fn person_auth_status() -> Value {
    json!({
        "status": "not_ready",
        "provider": "anthropic",
        "reason": "credentials_not_configured",
        "authType": "oauth",
        "accounts": [{
            "name": "work",
            "displayName": "Work",
            "source": "login",
            "blocked": true,
            "blockedReason": "rate_limit",
            "blockedUntil": 1,
            "pinned": false,
            "blockedModels": [{ "model": "opus", "until": 2 }],
            "unused": true,
            "route": "siwc",
            "planUsageEnabled": true,
            "signedOut": false,
            "store": "claude-cli",
            "signedInAt": 3,
            "email": "ada@example.test",
            "orgName": "Org",
            "subscriptionType": "max",
            "check": {
                "model": "opus",
                "outcome": "failed",
                "code": "auth_error",
                "message": "the provider refused the credential",
                "status": 401,
                "at": 4,
                "provider_text": { "reason": "invalid_grant", "message": "token expired" }
            },
            "signIn": "signed_in",
            "active": true,
            "paused": true,
            "usageUrl": "https://example.test/usage"
        }]
    })
}

fn delivered(destination: Value) -> Value {
    json!({ "delivered": true, "provider": "anthropic", "expires_at": "2026-11-26T00:00:00Z", "destination": destination })
}

/// Every credential-bearing call's answers, with every optional field and both destination shapes.
fn custody_samples(call: &str) -> Vec<Value> {
    let handed_over = |destination: Value| {
        json!({
            "delivered": true,
            "device": device(),
            "expires_at": "2026-11-26T00:00:00Z",
            "owner": "ada",
            "destination": destination
        })
    };
    match call {
        "account.credentials.handOver" => vec![handed_over(terminal()), handed_over(peer())],
        "account.adopt" => vec![json!({ "device": device(), "storage": "omo" })],
        "account.signIn" => vec![json!({
            "api": "https://api.example.test",
            "device": device(),
            "replaced_device": "dev-0",
            "device_service": "kept_off"
        })],
        "account.keys.init" | "account.keys.status" => vec![keyring()],
        "sync.enable" => vec![json!({ "enabled": true })],
        "sync.recover" => vec![json!({ "recovered": true })],
        "engine.auth.check" => vec![person_auth_status()],
        "engine.auth.credentials" | "engine.auth.apiKey" | "engine.auth.bearerToken" => {
            vec![delivered(terminal()), delivered(peer())]
        }
        "engine.trust.ensure" => vec![json!({ "action": "ensure", "generation": "g1" })],
        "mcp.oauth.login" => vec![
            json!({ "server": "linear", "stored": true, "expires_at": "2026-11-26T00:00:00Z" }),
        ],
        "mcp.oauth.logout" => vec![json!({ "server": "linear", "removed": true })],
        "mcp.oauth.status" => vec![json!({
            "servers": [{ "server": "linear", "authenticated": true, "expired": false, "expires_at": "2026-11-26T00:00:00Z" }]
        })],
        _ => Vec::new(),
    }
}

const DEVICE_FIELDS: [&str; 3] = ["device", "device.id", "device.name"];
const DELIVERED_TO_FIELDS: [&str; 14] = [
    "destination",
    "destination.kind",
    "destination.tty",
    "destination.session_id",
    "destination.leader_pid",
    "destination.leader_started_at",
    "destination.host_identity",
    "destination.host_pid",
    "destination.host_started_at",
    "destination.uid",
    "destination.executable_identity",
    "destination.pid",
    "destination.started_at",
    "destination.allowlist_entry",
];
const KEYRING_FIELDS: [&str; 3] = ["device_id", "signing_public_key", "sealing_public_key"];
const ACCOUNT_FIELDS: [&str; 35] = [
    "",
    ".name",
    ".displayName",
    ".source",
    ".blocked",
    ".blockedReason",
    ".blockedUntil",
    ".pinned",
    ".blockedModels",
    ".blockedModels[]",
    ".blockedModels[].model",
    ".blockedModels[].until",
    ".unused",
    ".route",
    ".planUsageEnabled",
    ".signedOut",
    ".store",
    ".signedInAt",
    ".email",
    ".orgName",
    ".subscriptionType",
    ".check",
    ".check.model",
    ".check.outcome",
    ".check.code",
    ".check.message",
    ".check.status",
    ".check.at",
    ".check.provider_text",
    ".check.provider_text.reason",
    ".check.provider_text.message",
    ".signIn",
    ".active",
    ".paused",
    ".usageUrl",
];

/// `CUSTODY_FIELDS` of authority-expected.ts: the whole non-secret answer of each credential-bearing call.
fn custody_fields(call: &str) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    let mut add = |paths: &[&str]| fields.extend(paths.iter().map(|path| (*path).to_string()));
    match call {
        "account.credentials.handOver" => {
            add(&["delivered", "expires_at", "owner"]);
            add(&DEVICE_FIELDS);
            add(&DELIVERED_TO_FIELDS);
        }
        "account.adopt" => {
            add(&DEVICE_FIELDS);
            add(&["storage"]);
        }
        "account.signIn" => {
            add(&["api", "replaced_device", "device_service"]);
            add(&DEVICE_FIELDS);
        }
        "account.keys.init" | "account.keys.status" => add(&KEYRING_FIELDS),
        "sync.enable" => add(&["enabled"]),
        "sync.recover" => add(&["recovered"]),
        "engine.auth.check" => {
            add(&["status", "provider", "reason", "authType", "accounts"]);
            let accounts: Vec<String> = ACCOUNT_FIELDS
                .iter()
                .map(|path| format!("accounts[]{path}"))
                .collect();
            fields.extend(accounts);
        }
        "engine.auth.credentials" | "engine.auth.apiKey" | "engine.auth.bearerToken" => {
            add(&["delivered", "provider", "expires_at"]);
            add(&DELIVERED_TO_FIELDS);
        }
        "engine.trust.ensure" => add(&["action", "generation"]),
        "mcp.oauth.login" => add(&["server", "stored", "expires_at"]),
        "mcp.oauth.logout" => add(&["server", "removed"]),
        "mcp.oauth.status" => add(&[
            "servers",
            "servers[]",
            "servers[].server",
            "servers[].authenticated",
            "servers[].expired",
            "servers[].expires_at",
        ]),
        _ => {}
    }
    fields
}

fn valid_session_answer() -> Value {
    json!({ "status": "ready", "provider": "p", "accounts": [{ "signed_in": true, "check": "auth_error" }] })
}

#[test]
fn root_has_the_five_principals_of_section_three_each_with_its_id() {
    let kinds: Vec<&str> = PRINCIPAL_KINDS.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["person", "script", "connector", "session", "service"]
    );
    let ids: Vec<Option<&str>> = PRINCIPAL_KINDS.iter().map(|kind| kind.id_field()).collect();
    assert_eq!(
        ids,
        [
            Some("uid"),
            Some("uid"),
            Some("binding"),
            Some("session_id"),
            None
        ]
    );
}

#[test]
fn a_principal_is_a_closed_object_of_its_kind_and_id() {
    for valid in [
        json!({ "kind": "person", "uid": 0 }),
        json!({ "kind": "script", "uid": 501 }),
        json!({ "kind": "connector", "binding": "slack:T1" }),
        json!({ "kind": "session", "session_id": "s1" }),
        json!({ "kind": "service" }),
    ] {
        assert!(is_principal(&valid), "{valid}");
    }
    for invalid in [
        json!({ "kind": "person", "uid": -1 }),
        json!({ "kind": "person", "uid": "0" }),
        json!({ "kind": "person", "uid": 0, "session_id": "s1" }),
        json!({ "kind": "session", "session_id": "" }),
        json!({ "kind": "connector" }),
        json!({ "kind": "service", "uid": 0 }),
        json!({ "kind": "admin", "uid": 0 }),
        json!("person"),
    ] {
        assert!(!is_principal(&invalid), "{invalid}");
    }
}

#[test]
fn every_refused_cell_answers_a_registered_code_of_its_namespace_before_any_side_effect() {
    let refusals = refusals();
    assert!(!refusals.is_empty());
    let mut problems = Vec::new();
    for refusal in &refusals {
        assert!(refusal.before_side_effect, "{}", refusal.scenario);
        let namespace = refusal.row.split('.').next().unwrap_or_default();
        match code_entry(refusal.code) {
            Some(entry) if entry.namespaces.contains(&namespace) => {}
            _ => problems.push(format!("{}: {}", refusal.scenario, refusal.code)),
        }
    }
    assert!(problems.is_empty(), "{problems:?}");
    let unique: BTreeSet<&str> = refusals
        .iter()
        .map(|refusal| refusal.scenario.as_str())
        .collect();
    assert_eq!(unique.len(), refusals.len());
}

#[test]
fn each_matrix_line_governs_rows_or_says_why_none_and_splits_person_from_script_only_with_a_quote()
{
    for line in principal_matrix() {
        assert_eq!(line.rows.is_empty(), line.no_row.is_some(), "{}", line.slug);
        assert!(
            ["4.1", "4.2", "4.3", "4.4"].contains(&line.section),
            "{}",
            line.slug
        );
        assert_eq!(
            line.split.is_some(),
            line.cells.person != line.cells.script,
            "{}",
            line.slug
        );
        for row in line.rows {
            assert!(
                is_namespace(row.split('.').next().unwrap_or_default()),
                "{row}"
            );
        }
        for text in [line.input, line.root].into_iter().flatten() {
            let value: Value = serde_json::from_str(text).expect("a JSON input");
            assert!(value.is_object(), "{}: {text}", line.slug);
        }
        for kind in PRINCIPAL_KINDS {
            if let Cell::Refuse {
                change: Some(change),
                ..
            } = line.cells.get(kind)
            {
                let entry = behaviour_change(change).expect("a behaviour change");
                for row in line.rows {
                    assert!(
                        entry.rows.contains(row),
                        "{} names {} for {row}",
                        line.slug,
                        change.as_str()
                    );
                }
            }
        }
    }
}

#[test]
fn b1_to_b7_are_data_and_each_scenario_asserts_what_its_evidence_flag_selects() {
    let ids: Vec<&str> = BEHAVIOUR_CHANGES
        .iter()
        .map(|change| change.id.as_str())
        .collect();
    assert_eq!(ids, ["B1", "B2", "B3", "B4", "B5", "B6", "B7"]);
    for change in BEHAVIOUR_CHANGES {
        assert!(!change.evidence_present, "{}", change.id.as_str());
        let asserted = asserted_expectation(change);
        match change.exists {
            Exists::Today(today) => assert_eq!(*asserted, today),
            Exists::New { why_new } => {
                assert!(!why_new.is_empty());
                assert_eq!(*asserted, change.approved);
            }
        }
        for expectation in [change.approved, *asserted] {
            if expectation.outcome == ExpectedOutcome::Refuse {
                let code = expectation.code.expect("a refusal names its code");
                assert!(code_entry(code).is_some(), "{code}");
            }
        }
        for row in change.rows {
            assert!(
                is_namespace(row.split('.').next().unwrap_or_default()),
                "{row}"
            );
        }
    }
    let b1 = behaviour_change(ChangeId::B1).expect("B1");
    let proven = BehaviourChange {
        evidence_present: true,
        ..*b1
    };
    assert_eq!(asserted_expectation(&proven), &b1.approved);
}

#[test]
fn the_registry_names_real_namespaces_and_every_sdk_code_is_registered_or_held() {
    for (code, entry) in OMO_CODE_REGISTRY {
        assert!(!entry.namespaces.is_empty(), "{code}");
        for namespace in entry.namespaces {
            assert!(is_namespace(namespace), "{code}: {namespace}");
        }
        assert!(!entry.next_action.is_empty(), "{code}");
        assert!(
            held_code(code).is_none(),
            "{code} is both registered and held"
        );
    }
    for (code, _) in HELD_CODES {
        assert!(code_entry(code).is_none(), "{code}");
    }
    for code in OMO_SDK_CODES {
        assert!(
            code_entry(code).is_some() || held_code(code).is_some(),
            "{code}"
        );
    }
    let internal = code_entry("internal_error").expect("internal_error");
    assert!(internal.namespaces.contains(&"threads"));
}

#[test]
fn a_credential_bearing_call_answers_a_closed_shape_with_only_its_non_secret_fields() {
    for call in CUSTODY_RESULT_CALLS {
        let samples = custody_samples(call);
        assert!(!samples.is_empty(), "{call}");
        for sample in &samples {
            assert!(custody_answer_accepts(call, sample), "{call}: {sample}");
            assert!(
                !custody_answer_accepts(call, &with_extra_field(sample)),
                "{call} is not closed"
            );
        }
        assert_eq!(paths_of(&samples), custody_fields(call), "{call}");
    }
    let mut adopted = json!({ "device": device(), "storage": "login" });
    assert!(custody_answer_accepts("account.adopt", &adopted));
    adopted["device"]["token"] = json!(CANARY);
    assert!(!custody_answer_accepts("account.adopt", &adopted));
    assert!(!custody_answer_accepts(
        "account.adopt",
        &json!({ "device": device(), "storage": "keychain" })
    ));
    assert!(!custody_answer_accepts(
        "account.adopt",
        &json!({ "device": { "id": "", "name": "studio" }, "storage": "omo" })
    ));
    assert!(!custody_answer_accepts("account.whoami", &json!({})));
}

#[test]
fn a_sessions_auth_check_is_the_option_c_shape_with_no_free_string_but_the_provider() {
    assert_eq!(SESSION_RESULT_CALLS, ["engine.auth.check"]);
    let full = json!({
        "status": "ready",
        "provider": "p",
        "reason": "invalid_state",
        "auth_type": "api_key",
        "accounts": [{ "signed_in": true, "check": "ok" }]
    });
    assert!(session_answer_accepts("engine.auth.check", &full));
    assert_eq!(
        paths_of(&[full.clone()]),
        set(&[
            "status",
            "provider",
            "reason",
            "auth_type",
            "accounts",
            "accounts[]",
            "accounts[].signed_in",
            "accounts[].check"
        ])
    );
    assert!(!session_answer_accepts(
        "engine.auth.check",
        &with_extra_field(&full)
    ));
    let free = free_string_paths(&full, |value| {
        session_answer_accepts("engine.auth.check", value)
    });
    assert_eq!(free, set(&["provider"]));
}

#[test]
fn a_canary_in_every_free_field_of_the_persons_answer_reaches_no_field_of_a_sessions_answer() {
    let planted: BTreeSet<String> = AUTH_CHECK_FREE_FIELDS
        .iter()
        .map(|entry| entry.field.to_string())
        .collect();
    let free = free_string_paths(&person_auth_status(), |value| {
        custody_answer_accepts("engine.auth.check", value)
    });
    assert_eq!(planted, free);
    let engine_owned: Vec<&str> = AUTH_CHECK_FREE_FIELDS
        .iter()
        .filter(|entry| entry.origin == FreeFieldOrigin::Engine)
        .map(|entry| entry.field)
        .collect();
    assert_eq!(engine_owned, ["accounts[].check.message"]);
    assert!(
        AUTH_CHECK_FREE_FIELDS
            .iter()
            .all(|entry| entry.may_embed == (entry.origin != FreeFieldOrigin::Engine))
    );

    let valid = valid_session_answer();
    assert!(session_answer_accepts("engine.auth.check", &valid));
    let carriers: Vec<&str> = AUTH_CHECK_FREE_FIELDS
        .iter()
        .map(|entry| entry.field)
        .filter(|path| {
            *path != "provider"
                && [KEY, ADDRESS].iter().any(|canary| {
                    session_answer_accepts("engine.auth.check", &plant_at(&valid, path, canary))
                })
        })
        .collect();
    assert!(carriers.is_empty(), "{carriers:?}");
    assert!(!session_answer_accepts(
        "engine.auth.check",
        &plant_at(&valid, "accounts[].check", ADDRESS)
    ));
}

#[test]
fn the_closed_check_codes_are_the_engines_set() {
    for code in AUTH_ACCOUNT_CHECK_CODES {
        let mut person = person_auth_status();
        person["accounts"][0]["check"]["code"] = json!(code);
        assert!(
            custody_answer_accepts("engine.auth.check", &person),
            "{code}"
        );
        let mut session = valid_session_answer();
        session["accounts"][0]["check"] = json!(code);
        assert!(
            session_answer_accepts("engine.auth.check", &session),
            "{code}"
        );
    }
    let mut person = person_auth_status();
    person["accounts"][0]["check"]["code"] = json!("ok");
    assert!(!custody_answer_accepts("engine.auth.check", &person));
}

#[test]
fn a_secret_offered_as_text_is_no_input_and_a_hand_over_with_no_destination_is_refused() {
    let peer = json!({ "kind": "peer", "pid": 4242, "started_at": "2026-11-25T00:00:00Z" });
    let hand_over = |destination: Option<Value>| {
        let mut input = json!({ "what": "access_token" });
        if let Some(destination) = destination {
            input["destination"] = destination;
        }
        custody_input_accepts("account.credentials.handOver", &input)
    };
    assert!(hand_over(Some(peer.clone())));
    assert!(hand_over(Some(json!({ "kind": "terminal" }))));
    assert!(!hand_over(Some(json!(CANARY))));
    assert!(!hand_over(None));
    assert!(!hand_over(Some(
        json!({ "kind": "terminal", "path": "/dev/tty" })
    )));
    assert!(!hand_over(Some(
        json!({ "kind": "peer", "pid": 0, "started_at": "2026-11-25T00:00:00Z" })
    )));
    assert!(!hand_over(Some(json!({ "kind": "peer", "pid": 4242 }))));
    assert!(!custody_input_accepts(
        "engine.auth.apiKey",
        &json!({ "provider": "p" })
    ));
    assert!(custody_input_accepts(
        "engine.auth.apiKey",
        &json!({ "provider": "p", "destination": peer })
    ));
    assert!(!custody_input_accepts(
        "account.adopt",
        &json!({ "credential": CANARY })
    ));
    // The engine.launch.* api_key cases belong to the launch inputs (inputs/launch.ts), which this crate does not
    // carry yet; see docs/omo-sync/v5.1.84/omo-sdk.md.
    assert!(custody_input_accepts(
        "mcp.oauth.login",
        &json!({ "server": "linear", "scopes": ["read"] })
    ));
    assert!(!custody_input_accepts(
        "mcp.oauth.login",
        &json!({ "server": "linear", "scopes": [] })
    ));
    assert!(custody_input_accepts("engine.auth.check", &json!({})));
}

#[test]
fn account_adopt_reads_its_credential_from_the_handle_the_caller_holds() {
    let mut adopt = AccountAdopt {
        credential: Cursor::new(b"stored-credential".to_vec()),
        replace: Some(true),
    };
    let mut read = String::new();
    adopt
        .credential
        .read_to_string(&mut read)
        .expect("the handle reads");
    assert_eq!(read, "stored-credential");
    assert_eq!(adopt.replace, Some(true));
}
