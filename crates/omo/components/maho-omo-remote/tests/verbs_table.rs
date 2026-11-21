use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use maho_omo_remote::{RemoteContext, run_account_command, run_remote};
use mhc_relayd::session_host::BoxFuture;
use mhc_relayd::{
    DeviceIdentity, HostCommandOutcome, HostSession, PeerConfig, Relayd, RelaydConfig,
    RelaydOptions, SessionHost,
};
use serde_json::{Value, json};

#[derive(Default)]
struct FakeHost {
    sessions: Mutex<Vec<HostSession>>,
    records: Mutex<Vec<(String, Value)>>,
    streaming: Mutex<bool>,
}

impl FakeHost {
    fn with(ids: &[&str]) -> Arc<Self> {
        let host = Self::default();
        *host.sessions.lock().unwrap_or_else(PoisonError::into_inner) = ids
            .iter()
            .map(|id| HostSession {
                session_id: (*id).to_owned(),
                durable_id: Some(format!("durable-{id}")),
                title: Some(format!("title {id}")),
                project: Some("/work".into()),
                path: None,
                state: "idle".into(),
                updated_ms: None,
                kind: Some("interactive".into()),
                model: None,
            })
            .collect();
        Arc::new(host)
    }

    fn methods(&self) -> Vec<String> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, record)| record["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

impl SessionHost for FakeHost {
    fn list_sessions<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
    ) -> BoxFuture<'a, Result<Vec<HostSession>, String>> {
        Box::pin(async move {
            Ok(self
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        })
    }

    fn command<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
        session_id: &'a str,
        record: &'a Value,
    ) -> BoxFuture<'a, HostCommandOutcome> {
        Box::pin(async move {
            let listed = self
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|session| session.is_named(session_id));
            if !listed {
                return HostCommandOutcome::NotListed;
            }
            self.records
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((session_id.to_owned(), record.clone()));
            let streaming = *self
                .streaming
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            HostCommandOutcome::Applied(match record["type"].as_str() {
                Some("get_state") => json!({ "state": { "isStreaming": streaming } }),
                Some("abort") => json!({ "aborted": streaming }),
                Some("close_session") => json!({ "closed": true }),
                _ => json!({ "disposition": "started" }),
            })
        })
    }
}

struct Captured {
    out: Arc<Mutex<Vec<String>>>,
    err: Arc<Mutex<Vec<String>>>,
}

impl Captured {
    fn take(&self) -> (Vec<String>, Vec<String>) {
        (
            std::mem::take(&mut *self.out.lock().unwrap_or_else(PoisonError::into_inner)),
            std::mem::take(&mut *self.err.lock().unwrap_or_else(PoisonError::into_inner)),
        )
    }

    fn json(&self) -> Value {
        let (out, _) = self.take();
        serde_json::from_str(out.last().map_or("null", String::as_str)).expect("one JSON line")
    }
}

fn context(state_dir: &Path) -> (RemoteContext, Captured) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let err = Arc::new(Mutex::new(Vec::new()));
    let (out_sink, err_sink) = (Arc::clone(&out), Arc::clone(&err));
    let context = RemoteContext {
        state_dir: state_dir.to_path_buf(),
        agent_dir: state_dir.join("agent"),
        version: "2026.11.21-test".into(),
        mhc_bin: None,
        output: Arc::new(move |line: &str| {
            out_sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line.to_owned());
        }),
        error: Arc::new(move |line: &str| {
            err_sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line.to_owned());
        }),
        now: Arc::new(|| 10_000_000),
        cold_wake: None,
        request_timeout: Duration::from_secs(20),
    };
    (context, Captured { out, err })
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[tokio::test]
async fn without_a_device_service_every_verb_is_unavailable_and_usage_comes_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ctx, captured) = context(&dir.path().join("none"));
    assert_eq!(run_remote(&ctx, &argv(&["status"])).await, 3);
    let (_, err) = captured.take();
    assert!(err[0].contains("not running"), "{err:?}");
    assert_eq!(
        run_remote(&ctx, &argv(&["sessions", "--state", "asleep"])).await,
        2
    );
    assert_eq!(run_remote(&ctx, &argv(&["teleport"])).await, 2);
    assert_eq!(run_remote(&ctx, &argv(&[])).await, 2);
    assert_eq!(run_remote(&ctx, &argv(&["help"])).await, 0);
    assert_eq!(run_remote(&ctx, &argv(&["send", "--help"])).await, 0);
    assert_eq!(run_remote(&ctx, &argv(&["attach", "box/s1"])).await, 3);
    assert_eq!(run_remote(&ctx, &argv(&["open", "box"])).await, 3);
    assert_eq!(
        run_account_command(&ctx, "whoami", &[]).await,
        3,
        "no key yet"
    );
    assert_eq!(
        run_account_command(&ctx, "login", &argv(&["--bogus"])).await,
        2
    );
}

#[tokio::test]
async fn the_verbs_answer_through_two_devices() {
    let alpha_dir = tempfile::tempdir().expect("tempdir");
    let beta_dir = tempfile::tempdir().expect("tempdir");
    let alpha_state = alpha_dir.path().join("ds");
    let beta_state = beta_dir.path().join("ds");
    let alpha = DeviceIdentity::generate("alpha").expect("identity");
    alpha.save(&alpha_state).expect("save alpha");
    let beta = DeviceIdentity::generate("beta").expect("identity");
    let beta_host = FakeHost::with(&["b1"]);
    let beta_relayd = Relayd::start(RelaydOptions {
        state_dir: beta_state,
        config: RelaydConfig {
            account: "home".into(),
            name: None,
            listen: Some("127.0.0.1:0".into()),
            peers: vec![PeerConfig {
                name: "alpha".into(),
                address: "127.0.0.1:9".into(),
                public_key: alpha.public_key(),
            }],
        },
        identity: beta.clone(),
        host: Arc::clone(&beta_host) as Arc<dyn SessionHost>,
        service_version: "0.0.0-test".into(),
    })
    .await
    .expect("beta");
    let beta_address = beta_relayd
        .peer_address()
        .expect("beta listens")
        .to_string();
    let alpha_relayd = Relayd::start(RelaydOptions {
        state_dir: alpha_state.clone(),
        config: RelaydConfig {
            account: "home".into(),
            name: None,
            listen: None,
            peers: vec![PeerConfig {
                name: "beta".into(),
                address: beta_address,
                public_key: beta.public_key(),
            }],
        },
        identity: alpha.clone(),
        host: FakeHost::with(&["a1"]) as Arc<dyn SessionHost>,
        service_version: "0.0.0-test".into(),
    })
    .await
    .expect("alpha");
    let (ctx, captured) = context(&alpha_state);

    assert_eq!(run_remote(&ctx, &argv(&["status", "--json"])).await, 0);
    let status = captured.json();
    assert_eq!(status["state"], "connected");
    assert_eq!(status["device"]["id"], alpha.id());
    assert_eq!(status["service_version"], "0.0.0-test");

    assert_eq!(run_remote(&ctx, &argv(&["devices", "--json"])).await, 0);
    let devices = captured.json();
    assert_eq!(devices[0]["this_device"], true);
    assert_eq!(devices[1]["label"], "beta");
    assert_eq!(devices[1]["online"], true);
    assert_eq!(run_remote(&ctx, &argv(&["devices"])).await, 0);
    let (lines, _) = captured.take();
    assert!(lines[0].starts_with("DEVICE"));
    assert!(lines[1].contains("this device"));
    assert!(lines[2].contains("online"));

    assert_eq!(run_remote(&ctx, &argv(&["sessions", "--json"])).await, 0);
    let listing = captured.json();
    assert_eq!(listing["complete"], true);
    assert_eq!(listing["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        run_remote(&ctx, &argv(&["sessions", "--device", "beta"])).await,
        0
    );
    let (lines, _) = captured.take();
    assert!(
        lines.iter().any(|line| line.starts_with("beta/b1")),
        "{lines:?}"
    );
    assert_eq!(
        run_remote(&ctx, &argv(&["sessions", "--device", "gamma"])).await,
        2
    );
    captured.take();

    assert_eq!(
        run_remote(&ctx, &argv(&["send", "beta/b1", "hello", "--json"])).await,
        0
    );
    let sent = captured.json();
    assert_eq!(sent["kind"], "ok");
    assert_eq!(sent["state"], "engine_applied");
    assert_eq!(
        run_remote(&ctx, &argv(&["send", "durable-b1", "again"])).await,
        0
    );
    let (lines, _) = captured.take();
    assert_eq!(lines, vec!["Sent to beta/b1: turn started.".to_owned()]);
    assert_eq!(
        run_remote(&ctx, &argv(&["model", "beta/b1", "openai/gpt-5"])).await,
        0
    );
    assert_eq!(run_remote(&ctx, &argv(&["interrupt", "beta/b1"])).await, 0);
    let (lines, _) = captured.take();
    assert_eq!(
        lines.last().map(String::as_str),
        Some("Nothing was running on beta/b1.")
    );

    *beta_host
        .streaming
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = true;
    assert_eq!(
        run_remote(&ctx, &argv(&["close", "beta/b1", "--json"])).await,
        1
    );
    let refused = captured.json();
    assert_eq!(refused["details"]["reason"], "turn_active");
    assert_eq!(
        run_remote(&ctx, &argv(&["close", "beta/b1", "--force"])).await,
        0
    );
    captured.take();
    assert_eq!(
        beta_host.methods(),
        vec![
            "prompt",
            "prompt",
            "set_model",
            "abort",
            "get_state",
            "close_session"
        ]
    );

    beta_host
        .sessions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    assert_eq!(
        run_remote(&ctx, &argv(&["send", "beta/b1", "late"])).await,
        1
    );
    let (_, err) = captured.take();
    assert!(err[0].contains("lists no such session"), "{err:?}");

    assert_eq!(
        run_remote(&ctx, &argv(&["control", "off", "--json"])).await,
        0
    );
    assert_eq!(captured.json()["attached"], false);
    assert_eq!(
        run_remote(&ctx, &argv(&["wake", "default", "--json"])).await,
        0
    );
    let wake = captured.json();
    assert_eq!(wake["stored"], Value::Null);
    assert_eq!(wake["overridden"], true);
    assert_eq!(run_remote(&ctx, &argv(&["disable"])).await, 0);
    let (lines, _) = captured.take();
    assert_eq!(
        lines,
        vec!["Remote sessions are off for this device.".to_owned()]
    );
    assert_eq!(run_remote(&ctx, &argv(&["devices", "--json"])).await, 0);
    assert_eq!(
        captured.json()[0]["online"],
        false,
        "this device shows offline while off"
    );

    assert_eq!(
        run_account_command(&ctx, "whoami", &argv(&["--json"])).await,
        0
    );
    let whoami = captured.json();
    assert_eq!(whoami["device"]["id"], alpha.id());
    assert_eq!(whoami["service"], "disabled");
    assert_eq!(
        run_account_command(&ctx, "logout", &argv(&["--json"])).await,
        0
    );
    assert_eq!(
        captured.json(),
        json!({ "signed_out": true, "key_removed": false })
    );
    assert_eq!(run_remote(&ctx, &argv(&["status", "--json"])).await, 0);
    assert_eq!(captured.json()["state"], "signed_out");
    assert_eq!(
        run_account_command(&ctx, "login", &argv(&["--json"])).await,
        0
    );
    let login = captured.json();
    assert_eq!(login["created"], false, "logout kept the key");
    assert_eq!(login["device"]["id"], alpha.id());
    assert_eq!(run_remote(&ctx, &argv(&["status", "--json"])).await, 0);
    assert_eq!(
        captured.json()["state"],
        "connected",
        "login took the device back"
    );

    alpha_relayd.shutdown().await;
    beta_relayd.shutdown().await;
}
