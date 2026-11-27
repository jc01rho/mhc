use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use device_service_core::hand_device_keys::device_service_endpoint_path;
use device_service_core::local_api::LocalApiError;
use maho_omo_remote::account_cli::{
    AccountCommandDependencies, RefreshOutcome, logout_account_session, run_account_verb,
};
use maho_omo_remote::account_store::{
    AccountCredentialStore, AccountExit, AccountStoreError, BoxFuture, CredentialDevice,
    Credentials, FileAccountCredentialStore, clear_signed_out_marker, credential_id,
    retire_sync_pin_on_discard, signed_out_marker_path, write_signed_out_marker,
};
use maho_omo_remote::device_service_handoff::{
    CliDeviceServiceHandoff, DeviceServiceHandoff, HandoffClient, HeldDeviceCredential,
    MachineDeviceState,
};
use memory_core::sync::crypto::signatures::sha256_b64;
use serde_json::{Value, json};

/// Writes the trust pin `load_trust_anchor` reads, in the shape `parse_pin` accepts: a 32-byte digest
/// in base64url without padding, which 43 `A` characters are.
fn pin_join(pin_dir: &Path, account: &str) {
    const DIGEST: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let dir = pin_dir
        .join("trust-anchor")
        .join(sha256_b64(account.as_bytes()));
    std::fs::create_dir_all(&dir).expect("pin dir");
    let pin = format!(
        "{{\"version\":1,\"account\":\"{account}\",\"genesis_digest\":\"{DIGEST}\",\"pinned_by\":\"pairing\",\"pinned_at\":{{\"seq\":2,\"keyring_link\":\"{DIGEST}\"}}}}\n"
    );
    std::fs::write(dir.join("pin.json"), pin).expect("pin file");
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

const NOW_MS: i64 = 1_795_745_000_000;

fn iso_at(offset_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(NOW_MS + offset_ms)
        .expect("timestamp")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn credential(device_id: &str, expires_offset_ms: i64, refresh_token: &str) -> Credentials {
    let value = json!({
        "accessToken": format!("at-{device_id}"),
        "accessTokenExpiresAt": iso_at(expires_offset_ms),
        "refreshToken": refresh_token,
        "device": { "id": device_id, "name": format!("device {device_id}") },
        "signingPrivateKey": "c2lnbmluZw==",
        "sealingPrivateKey": "c2VhbGluZw==",
    });
    Credentials::parse(&value).expect("valid credential")
}

fn credential_with_account(device_id: &str, account_id: &str) -> Credentials {
    let mut cred = credential(device_id, 3_600_000, "rt-1");
    cred.account_id = Some(account_id.to_owned());
    cred.raw
        .insert("accountId".into(), Value::String(account_id.to_owned()));
    cred
}

struct MemoryStore {
    value: Mutex<Option<Credentials>>,
    calls: Mutex<Vec<&'static str>>,
    read_err: Mutex<Option<AccountStoreError>>,
    write_err: Mutex<Option<AccountStoreError>>,
    clear_err: Mutex<Option<AccountStoreError>>,
    storage_kind: Option<&'static str>,
}

impl MemoryStore {
    fn new(initial: Option<Credentials>) -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(initial),
            calls: Mutex::new(Vec::new()),
            read_err: Mutex::new(None),
            write_err: Mutex::new(None),
            clear_err: Mutex::new(None),
            storage_kind: None,
        })
    }

    fn current(&self) -> Option<Credentials> {
        lock(&self.value).clone()
    }

    fn calls(&self) -> Vec<&'static str> {
        lock(&self.calls).clone()
    }
}

impl AccountCredentialStore for MemoryStore {
    fn read(&self) -> BoxFuture<'_, Result<Option<Credentials>, AccountStoreError>> {
        lock(&self.calls).push("get");
        let err = lock(&self.read_err).clone();
        let val = lock(&self.value).clone();
        Box::pin(async move {
            if let Some(err) = err {
                return Err(err);
            }
            Ok(val)
        })
    }

    fn write(&self, value: &Credentials) -> BoxFuture<'_, Result<(), AccountStoreError>> {
        lock(&self.calls).push("set");
        let err = lock(&self.write_err).clone();
        let next = value.clone();
        Box::pin(async move {
            if let Some(err) = err {
                return Err(err);
            }
            *lock(&self.value) = Some(next);
            Ok(())
        })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), AccountStoreError>> {
        lock(&self.calls).push("delete");
        let err = lock(&self.clear_err).clone();
        Box::pin(async move {
            if let Some(err) = err {
                return Err(err);
            }
            *lock(&self.value) = None;
            Ok(())
        })
    }

    fn storage(&self) -> Option<&'static str> {
        self.storage_kind
    }
}

struct Harness {
    deps: AccountCommandDependencies,
    out: Arc<Mutex<Vec<Value>>>,
    err: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    fn new(store: Arc<dyn AccountCredentialStore>, home: &Path, pin_dir: &Path) -> Self {
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let out_sink = Arc::clone(&out);
        let err_sink = Arc::clone(&err);
        let log_sink = Arc::clone(&err);
        let deps = AccountCommandDependencies {
            api: "http://127.0.0.1:1".into(),
            store,
            home: home.to_path_buf(),
            pin_dir: pin_dir.to_path_buf(),
            before_keys_discarded: retire_sync_pin_on_discard(
                pin_dir.to_path_buf(),
                Arc::new(move |line: &str| {
                    lock(&log_sink).push(line.to_owned());
                }),
            ),
            device_service: None,
            refresh_token: Arc::new(|_, _| {
                Box::pin(async { RefreshOutcome::Failed("unexpected refresh".into()) })
            }),
            read_stdin: Arc::new(String::new),
            now: Arc::new(|| NOW_MS),
            output: Arc::new(move |line: &str| {
                if let Ok(parsed) = serde_json::from_str::<Value>(line) {
                    lock(&out_sink).push(parsed);
                }
            }),
            error: Arc::new(move |line: &str| {
                lock(&err_sink).push(line.to_owned());
            }),
        };
        Self { deps, out, err }
    }

    async fn run(&mut self, args: &[&str], stdin: Option<&str>) -> (i32, Vec<Value>, Vec<String>) {
        lock(&self.out).clear();
        lock(&self.err).clear();
        let text = stdin.unwrap_or_default().to_owned();
        self.deps.read_stdin = Arc::new(move || text.clone());
        let argv: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let code = run_account_verb(&self.deps, &argv).await;
        let out = std::mem::take(&mut *lock(&self.out));
        let err = std::mem::take(&mut *lock(&self.err));
        (code, out, err)
    }
}

#[tokio::test]
async fn adopt_stores_credentials_refuses_different_device_and_keeps_newer_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = MemoryStore::new(None);
    let mut h = Harness::new(
        Arc::clone(&store) as Arc<dyn AccountCredentialStore>,
        dir.path(),
        &dir.path().join("pins"),
    );

    let incoming_a = credential("dev_a", 3_600_000, "rt-dev_a");
    let (code, out, _) = h
        .run(&["adopt"], Some(&incoming_a.to_value().to_string()))
        .await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(
        out,
        vec![json!({ "adopted": true, "device": { "id": "dev_a", "name": "device dev_a" } })]
    );
    assert_eq!(
        store.current().map(|c| c.device.id),
        Some("dev_a".to_owned())
    );

    let (code, shown, _) = h.run(&["show"], None).await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(
        shown,
        vec![json!({
            "signedIn": true,
            "device": { "id": "dev_a", "name": "device dev_a" },
            "refreshState": "ok",
        })]
    );
    assert!(!shown[0].to_string().contains("rt-dev_a"));

    let incoming_desk = credential("dev_desk", 3_600_000, "rt-desk");
    let (code, refused, _) = h
        .run(&["adopt"], Some(&incoming_desk.to_value().to_string()))
        .await;
    assert_eq!(code, AccountExit::Conflict.code());
    assert_eq!(
        refused,
        vec![json!({ "conflict": { "device": { "id": "dev_a", "name": "device dev_a" } } })]
    );
    assert_eq!(
        store.current().map(|c| c.device.id),
        Some("dev_a".to_owned())
    );

    let older_same = credential("dev_a", 60_000, "rt-older");
    for args in [&["adopt"][..], &["adopt", "--replace"][..]] {
        let (code, _, _) = h.run(args, Some(&older_same.to_value().to_string())).await;
        assert_eq!(code, AccountExit::Ok.code());
        assert_eq!(
            store.current().map(|c| c.refresh_token),
            Some("rt-dev_a".to_owned())
        );
    }

    let (code, _, _) = h
        .run(
            &["adopt", "--replace"],
            Some(&incoming_desk.to_value().to_string()),
        )
        .await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(
        store.current().map(|c| c.device.id),
        Some("dev_desk".to_owned())
    );

    let (code, _, _) = h.run(&["adopt"], Some(r#"{"accessToken":"x"}"#)).await;
    assert_eq!(code, AccountExit::Invalid.code());
}

#[tokio::test]
async fn token_refreshes_expiring_credential_and_refused_refresh_ends_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = MemoryStore::new(Some(credential("dev_a", 1_000, "rt-first")));
    let mut h = Harness::new(
        Arc::clone(&store) as Arc<dyn AccountCredentialStore>,
        dir.path(),
        &dir.path().join("pins"),
    );
    let fresh_exp = iso_at(3_600_000);
    let exp_clone = fresh_exp.clone();
    h.deps.refresh_token = Arc::new(move |_api, refresh| {
        assert_eq!(refresh, "rt-first");
        let exp = exp_clone.clone();
        Box::pin(async move {
            RefreshOutcome::Refreshed {
                access_token: "at-fresh".into(),
                access_token_expires_at: exp,
                refresh_token: "rt-next".into(),
            }
        })
    });
    let (code, out, _) = h.run(&["token"], None).await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(out[0]["accessToken"], "at-fresh");
    assert_eq!(
        store.current().map(|c| c.refresh_token),
        Some("rt-next".to_owned())
    );

    for refusal_code in [
        "reauth_required",
        "unauthorized",
        "invalid_grant",
        "account_deleted",
    ] {
        *lock(&store.value) = Some(credential("dev_a", 1_000, "rt-first"));
        let code_str = refusal_code.to_owned();
        h.deps.refresh_token = Arc::new(move |_api, _refresh| {
            let c = code_str.clone();
            Box::pin(async move {
                RefreshOutcome::Refused {
                    status: if c == "account_deleted" { 410 } else { 401 },
                    code: c,
                }
            })
        });
        let (code, out, _) = h.run(&["token"], None).await;
        assert_eq!(code, AccountExit::Ended.code());
        assert_eq!(out, vec![json!({ "ended": { "code": refusal_code } })]);
        assert!(store.current().is_none());
    }
}

#[tokio::test]
async fn storage_failures_report_exact_exit_codes_without_second_read_including_logout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = MemoryStore::new(None);
    *lock(&store.read_err) = Some(AccountStoreError::KeychainLocked(
        "macOS Keychain is locked.".into(),
    ));
    let mut h = Harness::new(
        Arc::clone(&store) as Arc<dyn AccountCredentialStore>,
        dir.path(),
        &dir.path().join("pins"),
    );

    for args in [
        &["show"][..],
        &["token"][..],
        &["export"][..],
        &["forget"][..],
        &["adopt"][..],
    ] {
        lock(&store.calls).clear();
        let stdin =
            (args[0] == "adopt").then(|| credential("dev_x", 60_000, "rt").to_value().to_string());
        let (code, out, err) = h.run(args, stdin.as_deref()).await;
        assert_eq!(code, AccountExit::KeychainLocked.code(), "{args:?}");
        assert_eq!(out, vec![json!({ "keychain": "locked" })]);
        assert_eq!(store.calls(), vec!["get"]);
        assert!(err.join("\n").contains("macOS Keychain is locked"));
    }

    *lock(&store.read_err) = Some(AccountStoreError::StoreUnavailable(
        "native read timed out".into(),
    ));
    lock(&store.calls).clear();
    let logout_code = logout_account_session(&h.deps).await;
    assert_eq!(logout_code, AccountExit::StoreUnavailable.code());
    assert_eq!(
        store.calls(),
        vec!["get"],
        "#14711: logout makes one store call and clears nothing on store-unavailable"
    );
}

#[tokio::test]
async fn discarding_keys_retires_sync_pin_and_file_store_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pin_dir = dir.path().join("pins");
    for account in ["acct-1", "acct-other"] {
        pin_join(&pin_dir, account);
    }

    let file_store: Arc<dyn AccountCredentialStore> = Arc::new(FileAccountCredentialStore::new(
        dir.path(),
        "http://127.0.0.1:1",
    ));
    let mut h = Harness::new(file_store, dir.path(), &pin_dir);

    let cred1 = credential_with_account("dev-1", "acct-1");
    let (code, out, _) = h.run(&["adopt"], Some(&cred1.to_value().to_string())).await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(out[0]["storage"], "file");

    let (_, shown, _) = h.run(&["show", "--account", "acct-1"], None).await;
    assert_eq!(shown[0]["sync"], "joined");

    let (code, _, _) = h.run(&["forget", "--device", "dev-1"], None).await;
    assert_eq!(code, AccountExit::Ok.code());

    let (_, shown_1, _) = h.run(&["show", "--account", "acct-1"], None).await;
    assert_eq!(shown_1[0]["sync"], "not_joined");
    let (_, shown_other, _) = h.run(&["show", "--account", "acct-other"], None).await;
    assert_eq!(shown_other[0]["sync"], "joined");

    let (code, retired, _) = h
        .run(&["retire-sync", "--account", "acct-other"], None)
        .await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(retired, vec![json!({ "retired": true })]);
    let (_, shown_other_after, _) = h.run(&["show", "--account", "acct-other"], None).await;
    assert_eq!(shown_other_after[0]["sync"], "not_joined");
}

struct FakeHandoffClient {
    version: String,
    status: Value,
    adopted: Arc<Mutex<Vec<HeldDeviceCredential>>>,
    forgotten: Arc<Mutex<u32>>,
}

impl HandoffClient for FakeHandoffClient {
    fn service_version(&self) -> &str {
        &self.version
    }

    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        let status = self.status.clone();
        Box::pin(async move { Ok(status) })
    }

    fn adopt(
        &self,
        credential: &HeldDeviceCredential,
    ) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        lock(&self.adopted).push(credential.clone());
        let dev =
            json!({ "device": { "id": credential.device.id, "name": credential.device.name } });
        Box::pin(async move { Ok(dev) })
    }

    fn forget(&self) -> BoxFuture<'_, Result<(), LocalApiError>> {
        *lock(&self.forgotten) += 1;
        Box::pin(async { Ok(()) })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn device_service_handoff_adopts_after_clearing_marker_and_handles_dead_endpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("ds");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let endpoint = device_service_endpoint_path(&state_dir);

    let adopted = Arc::new(Mutex::new(Vec::new()));
    let forgotten = Arc::new(Mutex::new(0u32));
    let logs = Arc::new(Mutex::new(Vec::new()));

    let adopted_ref = Arc::clone(&adopted);
    let forgotten_ref = Arc::clone(&forgotten);
    let logs_ref = Arc::clone(&logs);
    let handoff = Arc::new(CliDeviceServiceHandoff {
        api: "http://127.0.0.1:9".into(),
        state_dir: state_dir.clone(),
        log: Arc::new(move |line: &str| lock(&logs_ref).push(line.to_owned())),
        connect: Arc::new(move || {
            let adopted = Arc::clone(&adopted_ref);
            let forgotten = Arc::clone(&forgotten_ref);
            Box::pin(async move {
                Ok(Box::new(FakeHandoffClient {
                    version: "0.1.3".into(),
                    status: json!({
                        "state": "connected",
                        "device": { "id": "dev-self", "name": "This Mac" },
                    }),
                    adopted,
                    forgotten,
                }) as Box<dyn HandoffClient>)
            })
        }),
        fetch_account: Arc::new(|_api, _token| Box::pin(async { Ok("acct_from_me".to_owned()) })),
    });

    handoff
        .adopt(&credential("dev-self", 3_600_000, "rt"))
        .await;
    assert!(lock(&adopted).is_empty());
    assert_eq!(handoff.machine_device().await, MachineDeviceState::Absent);

    std::fs::write(&endpoint, b"").expect("endpoint file");
    let store = MemoryStore::new(None);
    let mut h = Harness::new(
        Arc::clone(&store) as Arc<dyn AccountCredentialStore>,
        dir.path(),
        &dir.path().join("pins"),
    );
    let marker_path = signed_out_marker_path(dir.path(), &credential_id(&h.deps.api));
    write_signed_out_marker(&marker_path, Some("dev-old")).expect("write marker");
    assert!(marker_path.exists());

    h.deps.device_service = Some(Arc::clone(&handoff) as Arc<dyn DeviceServiceHandoff>);
    let incoming = credential("dev-self", 3_600_000, "rt");
    let (code, _, _) = h
        .run(&["adopt"], Some(&incoming.to_value().to_string()))
        .await;
    assert_eq!(code, AccountExit::Ok.code());
    assert!(!marker_path.exists(), "adopt deletes the signed-out marker");
    assert_eq!(lock(&adopted).len(), 1);
    assert_eq!(lock(&adopted)[0].account, "acct_from_me");
    assert_eq!(
        handoff.machine_device().await,
        MachineDeviceState::Held {
            device: CredentialDevice {
                id: "dev-self".into(),
                name: "This Mac".into(),
            }
        }
    );

    let (code, _, _) = h.run(&["forget", "--device", "dev-other"], None).await;
    let _ = code;
    let (code, _, _) = h.run(&["forget"], None).await;
    assert_eq!(code, AccountExit::Ok.code());
    assert_eq!(*lock(&forgotten), 1);

    let dead_logs = Arc::new(Mutex::new(Vec::new()));
    let dead_logs_ref = Arc::clone(&dead_logs);
    let dead_handoff = CliDeviceServiceHandoff {
        api: "http://127.0.0.1:9".into(),
        state_dir: state_dir.clone(),
        log: Arc::new(move |line: &str| lock(&dead_logs_ref).push(line.to_owned())),
        connect: Arc::new(|| {
            Box::pin(async { Err(LocalApiError::new("service_not_running", "ECONNREFUSED")) })
        }),
        fetch_account: Arc::new(|_, _| Box::pin(async { Ok("acct".into()) })),
    };
    let state = dead_handoff.machine_device().await;
    assert!(
        matches!(
            &state,
            MachineDeviceState::Unknown {
                reason,
                endpoint: Some(ep),
            } if reason.contains("dead_endpoint") && ep == &endpoint
        ),
        "unexpected state: {state:?}"
    );
    assert!(
        lock(&dead_logs).is_empty(),
        "machine_device must not log treated as not running to stderr"
    );
    clear_signed_out_marker(&marker_path);
}
