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
pub(crate) struct FakeHost {
    pub(crate) sessions: Mutex<Vec<HostSession>>,
    records: Mutex<Vec<(String, Value)>>,
    pub(crate) streaming: Mutex<bool>,
    pub(crate) close_applied: Mutex<Option<bool>>,
}

impl FakeHost {
    pub(crate) fn with(ids: &[&str]) -> Arc<Self> {
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

    pub(crate) fn methods(&self) -> Vec<String> {
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
                Some("close_session") => {
                    let closed = self
                        .close_applied
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .unwrap_or(true);
                    json!({ "closed": closed })
                }
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

/// Two devices of one account: alpha (whose state dir the verbs use) and beta, which lists `b1`.
pub(crate) struct Pair {
    pub(crate) alpha: DeviceIdentity,
    pub(crate) alpha_state: PathBuf,
    pub(crate) beta_host: Arc<FakeHost>,
    alpha_relayd: Relayd,
    beta_relayd: Relayd,
    _dirs: [tempfile::TempDir; 2],
}

impl Pair {
    pub(crate) async fn start() -> Self {
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
        Self {
            alpha,
            alpha_state,
            beta_host,
            alpha_relayd,
            beta_relayd,
            _dirs: [alpha_dir, beta_dir],
        }
    }

    pub(crate) async fn stop(self) {
        self.alpha_relayd.shutdown().await;
        self.beta_relayd.shutdown().await;
    }
}

#[tokio::test]
async fn the_verbs_answer_through_two_devices() {
    let pair = Pair::start().await;
    let (alpha, beta_host) = (pair.alpha.clone(), Arc::clone(&pair.beta_host));
    let (ctx, captured) = context(&pair.alpha_state);

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

    *beta_host
        .close_applied
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(false);
    assert_eq!(
        run_remote(&ctx, &argv(&["close", "beta/b1", "--force", "--json"])).await,
        1,
        "a close that closed nothing exits 1 (#13905)"
    );
    let not_closed = captured.json();
    assert_eq!(not_closed["kind"], "error");
    assert_eq!(not_closed["code"], "not_found");
    *beta_host
        .close_applied
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;

    let stdin_message = "--first line\nzweite Zeile, 한국어\n";
    ctx.set_stdin_reader(Arc::new(move || stdin_message.to_owned()));
    assert_eq!(
        run_remote(
            &ctx,
            &argv(&["send", "beta/b1", "-", "--no-wait", "--json"])
        )
        .await,
        0
    );
    assert_eq!(captured.json()["kind"], "ok");

    ctx.set_stdin_reader(Arc::new(String::new));
    assert_eq!(
        run_remote(&ctx, &argv(&["send", "beta/b1", "-", "--no-wait"])).await,
        2,
        "empty stdin is a usage error (#13905)"
    );
    captured.take();

    let unsafe_target = "beta/s\u{001b}[31m\u{202e}x";
    assert_eq!(
        run_remote(&ctx, &argv(&["interrupt", unsafe_target, "--json"])).await,
        1
    );
    let json_err = captured.json();
    assert_eq!(json_err["kind"], "error");
    assert!(
        json_err.to_string().contains("\u{202e}")
            || json_err["message"]
                .as_str()
                .is_some_and(|msg| !msg.contains("\\u202e")),
        "--json output is not double-escaped by display escaping"
    );
    assert_eq!(
        run_remote(&ctx, &argv(&["interrupt", unsafe_target])).await,
        1
    );
    let (human_out, human_err) = captured.take();
    for line in human_out.iter().chain(human_err.iter()) {
        assert!(
            !line
                .chars()
                .any(|ch| maho_omo_remote::render::unsafe_for_display(ch, false)),
            "human output escaped unsafe chars: {line:?}"
        );
    }

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

    pair.stop().await;
}

#[tokio::test]
async fn typed_device_and_bare_session_resolution_through_relayd() {
    let pair = Pair::start().await;
    let (ctx, captured) = context(&pair.alpha_state);

    assert_eq!(run_remote(&ctx, &argv(&["devices", "--json"])).await, 0);
    let devices = captured.json();
    let beta_id = devices[1]["device"].as_str().expect("beta id").to_owned();
    let beta_prefix = &beta_id[..8];

    assert_eq!(
        run_remote(&ctx, &argv(&["sessions", "--device", "here", "--json"])).await,
        0
    );
    let here_rows = captured.json();
    assert_eq!(here_rows["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(here_rows["rows"][0]["session_id"], "a1");

    for target in [
        "here/a1".to_owned(),
        "ＢＥＴＡ/b1".to_owned(),
        format!("{beta_prefix}/b1"),
        "title b1".to_owned(),
        format!("{beta_id}/b1"),
    ] {
        assert_eq!(
            run_remote(&ctx, &argv(&["send", &target, "hi", "--json"])).await,
            0,
            "{target}"
        );
        assert_eq!(captured.json()["kind"], "ok", "{target}");
    }

    let alpha_state = pair.alpha_state.clone();
    let alpha_id = pair.alpha.id();
    pair.beta_relayd.shutdown().await;

    let (ctx, captured) = context(&alpha_state);
    assert_eq!(
        run_remote(&ctx, &argv(&["send", "beta/b1", "late", "--json"])).await,
        3
    );
    let (out, err) = captured.take();
    let refusal: Value =
        serde_json::from_str(out.last().map_or("null", String::as_str)).expect("json");
    assert_eq!(refusal["code"], "device_offline");
    assert_eq!(refusal["details"]["device"], beta_id);
    assert!(
        !err.iter().any(|line| line.starts_with("  dev_")),
        "device_offline prints no candidate list: {err:?}"
    );
    let _ = alpha_id;
    pair.alpha_relayd.shutdown().await;
}

#[tokio::test]
async fn ambiguous_devices_and_unnamed_rows_through_relayd() {
    let alpha_dir = tempfile::tempdir().expect("tempdir");
    let twin1_dir = tempfile::tempdir().expect("tempdir");
    let twin2_dir = tempfile::tempdir().expect("tempdir");
    let alpha_state = alpha_dir.path().join("ds");
    let alpha = DeviceIdentity::generate("alpha").expect("alpha");
    alpha.save(&alpha_state).expect("save alpha");
    let twin1 = DeviceIdentity::generate("twin").expect("twin1");
    let twin2 = DeviceIdentity::generate("twin").expect("twin2");

    let twin1_host = FakeHost::with(&["t1"]);
    twin1_host
        .sessions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)[0]
        .title = None;
    twin1_host
        .sessions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)[0]
        .project = Some("/work/myproj".into());
    let twin2_host = FakeHost::with(&["t2"]);

    let twin1_relayd = Relayd::start(RelaydOptions {
        state_dir: twin1_dir.path().join("ds"),
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
        identity: twin1.clone(),
        host: Arc::clone(&twin1_host) as Arc<dyn SessionHost>,
        service_version: "0.0.0-test".into(),
    })
    .await
    .expect("twin1");
    let twin2_relayd = Relayd::start(RelaydOptions {
        state_dir: twin2_dir.path().join("ds"),
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
        identity: twin2.clone(),
        host: Arc::clone(&twin2_host) as Arc<dyn SessionHost>,
        service_version: "0.0.0-test".into(),
    })
    .await
    .expect("twin2");
    let alpha_relayd = Relayd::start(RelaydOptions {
        state_dir: alpha_state.clone(),
        config: RelaydConfig {
            account: "home".into(),
            name: None,
            listen: None,
            peers: vec![
                PeerConfig {
                    name: "twin".into(),
                    address: twin1_relayd.peer_address().expect("twin1").to_string(),
                    public_key: twin1.public_key(),
                },
                PeerConfig {
                    name: "twin".into(),
                    address: twin2_relayd.peer_address().expect("twin2").to_string(),
                    public_key: twin2.public_key(),
                },
            ],
        },
        identity: alpha.clone(),
        host: FakeHost::with(&["a1"]) as Arc<dyn SessionHost>,
        service_version: "0.0.0-test".into(),
    })
    .await
    .expect("alpha");

    let (ctx, captured) = context(&alpha_state);
    assert_eq!(run_remote(&ctx, &argv(&["sessions", "--json"])).await, 0);
    let listing = captured.json();
    let rows = listing["rows"].as_array().expect("rows");
    let anon = rows
        .iter()
        .find(|row| row["session_id"] == "t1")
        .expect("t1");
    assert_eq!(anon["name"], "myproj");
    assert_eq!(anon["unnamed"], true);

    assert_eq!(
        run_remote(&ctx, &argv(&["send", "myproj", "hi", "--json"])).await,
        1
    );
    assert_eq!(captured.json()["code"], "not_found");
    captured.take();

    assert_eq!(
        run_remote(&ctx, &argv(&["sessions", "--device", "twin", "--json"])).await,
        1
    );
    let (out, err) = captured.take();
    let amb: Value = serde_json::from_str(out.last().map_or("null", String::as_str)).expect("json");
    assert_eq!(amb["code"], "ambiguous_target");
    assert!(err.iter().any(|line| line.contains(&twin1.id())), "{err:?}");
    assert!(err.iter().any(|line| line.contains(&twin2.id())), "{err:?}");

    assert_eq!(run_remote(&ctx, &argv(&["send", "twin/t1", "hi"])).await, 1);
    let (_, err) = captured.take();
    assert!(err.iter().any(|line| line.contains(&twin1.id())), "{err:?}");
    assert!(err.iter().any(|line| line.contains(&twin2.id())), "{err:?}");

    alpha_relayd.shutdown().await;
    twin1_relayd.shutdown().await;
    twin2_relayd.shutdown().await;
}
