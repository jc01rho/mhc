use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use maho_ext_api::*;
use maho_omo_gateway::thread::adapter::{
    EndpointLiveness, GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions,
    create_gateway_thread_component_with_alerts,
};
use maho_omo_gateway::thread::engine::{GatewayResolution, GatewayTarget};
use maho_omo_gateway::thread::run_alerts::{
    OperatorAlertsTestSeams, RunAlertsLogger, WebhookFetch, WebhookResponse,
};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{AdmissionLedger, Provenance};
use maho_omo_gateway::thread::wake::session_target::parse_iso_timestamp_ms;
use regex::Regex;
use serde_json::{Value, json};

const SEAT: &str = "dur-seat";
const DUTY: &str = "dur-duty";
const WEBHOOK_ENV: &str = "OMO_TEST_ALERT_WEBHOOK";
const WEBHOOK_URL: &str = "https://hooks.example.invalid/services/SECRET-PATH";
const SENTINELS: [&str; 3] = ["PROMPT-SENTINEL", "TOOL-SENTINEL", "OUTPUT-SENTINEL"];

struct TestSession {
    session_id: String,
}

impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }

    fn get_leaf_id(&self) -> Option<String> {
        None
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }

    fn get_session_name(&self) -> Option<String> {
        Some("coordinator".into())
    }
}

struct TestRegistry;

impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }

    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }

    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }

    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }

    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestUi;

impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn extension_context(durable_id: &str) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: "/tmp".into(),
        agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession {
            session_id: durable_id.to_owned(),
        }),
        model_registry: Arc::new(TestRegistry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

struct LogSink {
    lines: Mutex<Vec<String>>,
    condvar: Condvar,
}

impl LogSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lines: Mutex::new(Vec::new()),
            condvar: Condvar::new(),
        })
    }

    fn push(&self, line: &str) {
        let mut guard = self
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.push(line.to_owned());
        self.condvar.notify_all();
    }

    fn wait_for(&self, pattern: &str, label: &str) {
        let re = Regex::new(pattern).expect("valid regex");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut guard = self
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !guard.iter().any(|line| re.is_match(line)) {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for {label}: {guard:?}");
            let (next, _) = self
                .condvar
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next;
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Posted {
    url: String,
    body: Value,
}

struct SeatHarness {
    _root: tempfile::TempDir,
    store: Arc<dyn GatewayStoreApi>,
    component: GatewayThreadComponent,
    api: ExtensionApi,
    clock: Arc<AtomicI64>,
    log: Arc<LogSink>,
    posted: Arc<Mutex<Vec<Posted>>>,
}

impl SeatHarness {
    fn new(fetch: WebhookFetch, config_layer: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let root_path =
            std::fs::canonicalize(root.path()).unwrap_or_else(|_| root.path().to_path_buf());
        let agent_dir = root_path.join("agent");
        let home = root_path.join("home");
        let workspace = home.join("project");
        std::fs::create_dir_all(home.join(".omo")).expect("mkdir home .omo");
        std::fs::create_dir_all(workspace.join(".omo")).expect("mkdir workspace .omo");
        let alerts = json!({
            "operator_alerts": {
                "error_streak": {
                    "threshold": 10,
                    "realert_minutes": 60,
                    "threads": [DUTY],
                    "webhook_url_env": WEBHOOK_ENV,
                }
            }
        });
        let is_project = config_layer == "project";
        std::fs::write(
            home.join(".omo").join("omo.jsonc"),
            if is_project {
                "{}".to_owned()
            } else {
                alerts.to_string()
            },
        )
        .expect("write user config");
        std::fs::write(
            workspace.join(".omo").join("omo.json"),
            if is_project {
                alerts.to_string()
            } else {
                "{}".to_owned()
            },
        )
        .expect("write project config");

        let store = create_gateway_store(GatewayStoreOptions::new(
            agent_dir.to_string_lossy().into_owned(),
        ))
        .expect("create store");
        let log = LogSink::new();
        let start_clock =
            parse_iso_timestamp_ms("2026-11-19T22:00:00.000Z").expect("start timestamp");
        let clock = Arc::new(AtomicI64::new(start_clock));
        let posted: Arc<Mutex<Vec<Posted>>> = Arc::new(Mutex::new(Vec::new()));

        let mut env = BTreeMap::new();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env.insert(WEBHOOK_ENV.into(), WEBHOOK_URL.into());

        let clock_for_seams = clock.clone();
        let posted_for_seams = posted.clone();
        let log_info = log.clone();
        let log_warn = log.clone();
        let seams = OperatorAlertsTestSeams {
            env: Some(env),
            now: Some(Arc::new(move || clock_for_seams.load(Ordering::SeqCst))),
            device: Some(Arc::new(|| Box::pin(async { Some("device-test".into()) }))),
            delay: Some(Arc::new(|_| Box::pin(async {}))),
            fetch: Some(Arc::new(move |url, body_str| {
                let posted = posted_for_seams.clone();
                let fetch = fetch.clone();
                Box::pin(async move {
                    let parsed: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);
                    posted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(Posted {
                            url: url.clone(),
                            body: parsed,
                        });
                    fetch(url, body_str).await
                })
            })),
            log: Some(RunAlertsLogger {
                info: Arc::new(move |line| log_info.push(line)),
                warn: Arc::new(move |line| log_warn.push(line)),
            }),
        };

        let workspace_str = workspace.to_string_lossy().into_owned();
        let component = create_gateway_thread_component_with_alerts(
            Arc::new(GatewayThreadComponentOptions {
                store: store.clone(),
                runtime: SessionRuntimePort {
                    phase: Arc::new(|| RuntimePhase::Idle),
                    admission_gate: Arc::new(|| SessionAdmissionGate {
                        can_admit: false,
                        hold_reason: Some("test".into()),
                        editor_revision: 0,
                        turn_epoch: 0,
                    }),
                    admit_external_message: Arc::new(|_| Err("unused".into())),
                    list_admitted_deliveries: Arc::new(AdmissionLedger::default),
                },
                durable_id: SEAT.into(),
                session_path: Arc::new(|| None),
                caller_session_id: Arc::new(|| SEAT.into()),
                caller_name: Some(Arc::new(|| Some("coordinator".into()))),
                caller_provenance: Arc::new(|| Provenance {
                    workspace: "unclassified".into(),
                    project: "project".into(),
                    lane: None,
                    session: None,
                }),
                resolve: Arc::new(|target, _all_scope| {
                    let target = target.to_owned();
                    Box::pin(async move {
                        GatewayResolution::Ok(GatewayTarget {
                            durable_id: target,
                            endpoint: None,
                            liveness: EndpointLiveness::Dead,
                        })
                    })
                }),
                endpoints: GatewayEndpointPort::inert(),
                locate: Arc::new(|_| Box::pin(async { None })),
                notify: None,
                log: None,
                host: None,
                caller_workspace_root: Arc::new(move || workspace_str.clone()),
                sessions_directory: None,
                remote: None,
            }),
            Some(seams),
        );

        let mut api = ExtensionApi::new(
            LoadedExtension::new("thread", workspace, SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        component.register(&mut api);

        Self {
            _root: root,
            store,
            component,
            api,
            clock,
            log,
            posted,
        }
    }

    fn dispatch_event(&self, kind: EventKind, mut event: ExtensionEvent, durable_id: &str) {
        let ctx = extension_context(durable_id);
        if let Some(handlers) = self.api.registered.handlers.get(&kind).cloned() {
            for handler in handlers {
                block_on(handler(&mut event, &ctx)).expect("handler dispatch");
            }
        }
    }

    fn end_run(&self, assistant: Value) {
        self.clock.fetch_add(15_000, Ordering::SeqCst);
        let user_msg: AgentMessage = serde_json::from_value(json!({
            "role": "user",
            "content": [{ "type": "text", "text": "PROMPT-SENTINEL" }],
            "timestamp": 0,
        }))
        .expect("user message");
        let tool_msg: AgentMessage = serde_json::from_value(json!({
            "role": "toolResult",
            "toolCallId": "c1",
            "toolName": "read",
            "content": [{ "type": "text", "text": "TOOL-SENTINEL" }],
            "isError": false,
            "timestamp": 0,
        }))
        .expect("tool result message");
        let assistant_msg: AgentMessage =
            serde_json::from_value(assistant).expect("assistant message");

        self.dispatch_event(
            EventKind::AgentEnd,
            ExtensionEvent::AgentEnd {
                messages: vec![user_msg, tool_msg, assistant_msg],
                aborted: None,
                will_retry: None,
                abort_source: None,
            },
            SEAT,
        );
        self.dispatch_event(EventKind::AgentSettled, ExtensionEvent::AgentSettled, SEAT);
    }

    fn failed_run(&self) {
        self.end_run(json!({
            "role": "assistant",
            "content": [],
            "api": "faux",
            "provider": "anthropic",
            "model": "claude-x",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "error",
            "errorMessage": "rate_limit_error: This request would exceed your account's rate limit OUTPUT-SENTINEL",
            "timestamp": 0,
        }));
    }

    fn normal_run(&self) {
        self.end_run(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "done" }],
            "api": "faux",
            "provider": "anthropic",
            "model": "claude-x",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
            },
            "stopReason": "stop",
            "timestamp": 0,
        }));
    }

    fn shutdown(&self) {
        self.dispatch_event(
            EventKind::SessionShutdown,
            ExtensionEvent::SessionShutdown(SessionShutdownEvent::default()),
            SEAT,
        );
    }

    fn deliveries_to(&self, target: &str) -> Vec<(String, String)> {
        let mut rows = self
            .store
            .list(Some(target), None)
            .expect("list deliveries");
        rows.sort_by_key(|row| row.seq);
        rows.into_iter().map(|row| (row.sender, row.body)).collect()
    }

    fn send_as_peer(&self, peer: &str, text: &str) -> Value {
        let key = format!("call-{peer}-{text}");
        block_on((self.component.send_as)(
            peer.to_owned(),
            json!({
                "thread": SEAT,
                "message": text,
                "all_scope": true,
                "idempotency_key": key,
            }),
        ))
    }

    fn posted_list(&self) -> Vec<Posted> {
        self.posted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn ok_fetch() -> WebhookFetch {
    Arc::new(|_, _| Box::pin(async { Ok(WebhookResponse { status: 200 }) }))
}

#[test]
fn given_a_seat_whose_runs_end_with_errors_when_10th_settles_25_more_follow_and_normal_stop_comes_then_duty_thread_and_webhook_get_one_alert_and_one_recovered_notice_with_metadata_only()
 {
    let seat = SeatHarness::new(ok_fetch(), "user");
    for _ in 1..=10 {
        seat.failed_run();
    }
    seat.log.wait_for(
        r"error_streak for dur-seat sent to the webhook",
        "first webhook alert",
    );
    seat.log.wait_for(
        r"error_streak for dur-seat sent to thread dur-duty",
        "first thread alert",
    );

    for _ in 1..=25 {
        seat.failed_run();
    }
    seat.normal_run();
    seat.log.wait_for(
        r"error_streak_recovered for dur-seat sent to the webhook",
        "recovered webhook notice",
    );
    seat.log.wait_for(
        r"error_streak_recovered for dur-seat sent to thread dur-duty",
        "recovered thread notice",
    );

    let to_duty = seat.deliveries_to(DUTY);
    assert_eq!(to_duty.len(), 2);
    assert_eq!(to_duty[0].0, format!("session:{SEAT}"));
    assert_eq!(to_duty[1].0, format!("session:{SEAT}"));
    let line0: Value =
        serde_json::from_str(to_duty[0].1.lines().nth(1).unwrap_or("{}")).expect("json 0");
    let line1: Value =
        serde_json::from_str(to_duty[1].1.lines().nth(1).unwrap_or("{}")).expect("json 1");
    assert_eq!(line0["event"], "error_streak");
    assert_eq!(line0["count"], 10);
    assert_eq!(line0["device"], "device-test");
    assert_eq!(line1["event"], "error_streak_recovered");
    assert_eq!(line1["count"], 35);
    assert_eq!(line1["device"], "device-test");

    let posted = seat.posted_list();
    assert_eq!(posted.len(), 2);
    assert_eq!(posted[0].url, WEBHOOK_URL);
    assert_eq!(posted[0].body["event"], "error_streak");
    assert_eq!(posted[0].body["count"], 10);
    assert_eq!(posted[1].url, WEBHOOK_URL);
    assert_eq!(posted[1].body["event"], "error_streak_recovered");
    assert_eq!(posted[1].body["count"], 35);
    assert_eq!(posted[0].body["session_id"], SEAT);
    assert_eq!(posted[0].body["name"], "coordinator");
    assert_eq!(posted[0].body["classes"], json!({ "rate_limit": 10 }));
    assert_eq!(posted[0].body["provider"], "anthropic");
    assert_eq!(posted[0].body["model"], "claude-x");
    assert_eq!(
        posted[0].body["first_at"],
        json!(parse_iso_timestamp_ms("2026-11-19T22:00:15.000Z").expect("ts"))
    );
    assert_eq!(
        posted[0].body["last_at"],
        json!(parse_iso_timestamp_ms("2026-11-19T22:02:30.000Z").expect("ts"))
    );
    assert!(posted[0].body["text"].is_string());

    let mut everything: Vec<String> = to_duty.into_iter().map(|(_, body)| body).collect();
    everything.extend(posted.into_iter().map(|p| p.body.to_string()));
    for sentinel in SENTINELS {
        assert!(
            !everything.iter().any(|text| text.contains(sentinel)),
            "leaked sentinel {sentinel}"
        );
    }
}

#[test]
fn given_a_seat_whose_own_inbox_holds_128_undelivered_messages_when_its_10th_error_run_settles_then_alert_still_reaches_duty_thread()
 {
    let seat = SeatHarness::new(ok_fetch(), "user");
    for index in 0..16 {
        let peer = format!("dur-peer-{index}");
        for message in 1..=8 {
            let res = seat.send_as_peer(&peer, &format!("work {message}"));
            assert_eq!(res["kind"], "ok", "{res}");
        }
    }
    assert_eq!(seat.deliveries_to(SEAT).len(), 128);

    for _ in 1..=10 {
        seat.failed_run();
    }
    seat.log.wait_for(
        r"error_streak for dur-seat sent to thread dur-duty",
        "alert to duty thread",
    );
    assert_eq!(seat.deliveries_to(DUTY).len(), 1);
}

#[test]
fn given_a_webhook_that_answers_500_once_and_then_holds_its_answer_when_alert_fires_then_settle_never_waits_for_it_post_is_retried_once_and_url_is_never_logged()
 {
    let calls = Arc::new(AtomicUsize::new(0));
    let retried_pair = Arc::new((Mutex::new(false), Condvar::new()));
    let held_pair: Arc<(Mutex<Option<WebhookResponse>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));

    let calls_fetch = calls.clone();
    let retried_fetch = retried_pair.clone();
    let held_fetch = held_pair.clone();
    let fetch: WebhookFetch = Arc::new(move |_, _| {
        let count = calls_fetch.fetch_add(1, Ordering::SeqCst) + 1;
        let retried = retried_fetch.clone();
        let held = held_fetch.clone();
        Box::pin(async move {
            if count == 1 {
                return Ok(WebhookResponse { status: 500 });
            }
            {
                let (lock, cvar) = &*retried;
                let mut seen = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *seen = true;
                cvar.notify_all();
            }
            let (lock, cvar) = &*held;
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while guard.is_none() {
                let now = Instant::now();
                assert!(now < deadline, "timed out waiting for held webhook release");
                let (next, _) = cvar
                    .wait_timeout(guard, deadline - now)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard = next;
            }
            Ok(guard.clone().expect("released response"))
        })
    });

    let seat = SeatHarness::new(fetch, "user");
    for _ in 1..=10 {
        seat.failed_run();
    }

    {
        let (lock, cvar) = &*retried_pair;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*seen {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for retried webhook POST");
            let (next, _) = cvar
                .wait_timeout(seen, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            seen = next;
        }
    }

    seat.failed_run();

    {
        let (lock, cvar) = &*held_pair;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(WebhookResponse { status: 200 });
        cvar.notify_all();
    }

    seat.log.wait_for(
        r"error_streak for dur-seat sent to the webhook",
        "the webhook alert",
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        !seat
            .log
            .snapshot()
            .iter()
            .any(|line| line.contains("SECRET-PATH"))
    );
}

#[test]
fn given_operator_alerts_set_only_in_workspace_omo_json_when_10th_error_settles_then_alert_is_logged_but_no_thread_or_webhook_receives_it()
 {
    let seat = SeatHarness::new(ok_fetch(), "project");
    for _ in 1..=10 {
        seat.failed_run();
    }
    seat.log
        .wait_for(r"^omo alert: .*ended 10 runs in a row", "logged alert");
    seat.log.wait_for(
        r"error_streak for dur-seat: \d+ of \d+ target\(s\) reached",
        "dispatched alert",
    );
    assert!(seat.deliveries_to(DUTY).is_empty());
    assert!(seat.posted_list().is_empty());
}

#[test]
fn given_a_seat_that_ended_its_session_nine_errors_into_a_streak_when_same_session_id_fails_ten_more_times_then_alert_streak_starts_at_first_run_after_shutdown()
 {
    let seat = SeatHarness::new(ok_fetch(), "user");
    for _ in 1..=9 {
        seat.failed_run();
    }
    seat.shutdown();

    for _ in 1..=10 {
        seat.failed_run();
    }
    seat.log.wait_for(
        r"error_streak for dur-seat sent to the webhook",
        "error-streak alert after shutdown",
    );

    let posted = seat.posted_list();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].body["event"], "error_streak");
    assert_eq!(posted[0].body["count"], 10);
    assert_eq!(
        posted[0].body["first_at"],
        json!(parse_iso_timestamp_ms("2026-11-19T22:02:30.000Z").expect("ts"))
    );
}
