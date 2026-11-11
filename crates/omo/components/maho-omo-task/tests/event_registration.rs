use crate::support;
use maho_ext_api::*;
use maho_omo_task::{
    dag_runtime::{DagRuntimeLifecycle, wire_dag_lifecycle},
    event_bridge::{stale_stop_release_session, wire_stale_stop_release, wire_task_usage_guidance},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

fn agent_start(preview: bool) -> ExtensionEvent {
    ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: "work".into(),
        trigger: "prompt".into(),
        preview,
        images: None,
        system_prompt: String::new(),
        system_prompt_options: Default::default(),
    })
}

/// #12343: only a real turn re-checks a stale run Stop, and it re-checks its own session.
#[test]
fn only_a_real_turn_re_checks_a_stale_stop_for_its_own_session() {
    let context = support::context();
    assert_eq!(
        stale_stop_release_session(&agent_start(true), &context),
        None
    );
    assert_eq!(
        stale_stop_release_session(&agent_start(false), &context).as_deref(),
        Some("session")
    );
}

/// #12343: a release that never settles (a child resume port that never answers) does not hold
/// the turn, and a release that fails is reported instead of failing the turn.
#[test]
fn a_stale_stop_release_never_holds_the_turn_and_a_failed_release_is_reported() {
    let mut api = support::api();
    let (entered, entries) = mpsc::channel::<String>();
    let (unblock, blocked) = mpsc::channel::<()>();
    let blocked = Mutex::new(Some(blocked));
    let calls = AtomicUsize::new(0);
    let (reported, reports) = mpsc::channel::<String>();
    wire_stale_stop_release(
        &mut api,
        Arc::new(move |session: &str, _: &ExtensionContext| {
            let _ = entered.send(session.to_string());
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                if let Some(blocked) = blocked.lock().expect("blocked").take() {
                    let _ = blocked.recv();
                }
                return Ok(());
            }
            Err("resume port down".into())
        }),
        Arc::new(move |line: &str| {
            let _ = reported.send(line.to_string());
        }),
    );
    let handler = api.registered.handlers[&EventKind::BeforeAgentStart][0].clone();

    // The turns start on a current_thread host runtime of their own, so a handler that waited on
    // the release (asynchronously or not) fails the breaker below instead of hanging the test.
    let (started, turns) = mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let context = support::context();
        for _ in 0..2 {
            let mut event = agent_start(false);
            let result = runtime.block_on(handler(&mut event, &context));
            let _ = started.send(matches!(result, Ok(EventResult::None)));
        }
    });

    // Circuit breaker only: each turn starts at once.
    let wait = Duration::from_secs(10);
    assert_eq!(
        turns.recv_timeout(wait),
        Ok(true),
        "the stale release held the turn"
    );
    assert_eq!(
        turns.recv_timeout(wait),
        Ok(true),
        "the stale release held the turn"
    );
    assert_eq!(entries.recv_timeout(wait).as_deref(), Ok("session"));
    assert_eq!(entries.recv_timeout(wait).as_deref(), Ok("session"));
    let report = reports
        .recv_timeout(wait)
        .expect("the failed release is reported");
    assert!(report.contains("resume port down"), "{report}");
    unblock.send(()).expect("settle the blocked release");
}
struct Runtime(Arc<Mutex<Vec<&'static str>>>);
impl DagRuntimeLifecycle for Runtime {
    fn attach(&self) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("events").push("attach");
        Ok(())
    }
    fn detach(&self) {
        self.0.lock().expect("events").push("detach");
    }
    fn pause_for_shutdown(&self) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("events").push("pause");
        Ok(())
    }
    fn dispose(&self) {
        self.0.lock().expect("events").push("dispose");
    }
}
#[tokio::test]
async fn dag_shutdown_pauses_before_task_teardown_then_disposes() {
    let mut api = support::api();
    let events = Arc::new(Mutex::new(Vec::new()));
    let task = events.clone();
    wire_dag_lifecycle(&mut api, Arc::new(Runtime(events.clone())), move |api| {
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let task = task.clone();
                Box::pin(async move {
                    task.lock().expect("events").push("task");
                    Ok(EventResult::None)
                })
            }),
        );
    });
    let context = support::context();
    let mut event = ExtensionEvent::SessionShutdown(SessionShutdownEvent {
        reason: SessionReason::Quit,
        target_session_file: None,
        signal: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {
        handler(&mut event, &context).await.expect("shutdown");
    }
    assert_eq!(
        *events.lock().expect("events"),
        ["pause", "task", "dispose"]
    );
}
#[tokio::test]
async fn usage_guidance_is_hidden_and_delivered_once_per_session() {
    let mut api = support::api();
    wire_task_usage_guidance(&mut api, Arc::new(|| true));
    let context = support::context();
    let mut event = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
        prompt: "work".into(),
        trigger: "prompt".into(),
        preview: false,
        images: None,
        system_prompt: String::new(),
        system_prompt_options: Default::default(),
    });
    let handler = &api.registered.handlers[&EventKind::BeforeAgentStart][0];
    let EventResult::BeforeAgentStart(result) = handler(&mut event, &context).await.expect("event")
    else {
        panic!("guidance");
    };
    let message = result.message.expect("message");
    assert_eq!(message.custom_type, "senpi-task.usage");
    assert!(!message.display);
    assert!(matches!(
        handler(&mut event, &context).await.expect("repeat"),
        EventResult::None
    ));
}
#[tokio::test]
async fn dag_start_attaches_after_task_recovery_and_switch_detaches_after_task_capture() {
    let mut api = support::api();
    let events = Arc::new(Mutex::new(Vec::new()));
    let task = events.clone();
    wire_dag_lifecycle(&mut api, Arc::new(Runtime(events.clone())), move |api| {
        for kind in [EventKind::SessionStart, EventKind::SessionBeforeSwitch] {
            let task = task.clone();
            api.on(
                kind,
                Arc::new(move |_, _| {
                    let task = task.clone();
                    Box::pin(async move {
                        task.lock().expect("events").push("task");
                        Ok(EventResult::None)
                    })
                }),
            );
        }
    });
    let context = support::context();
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent {
        sizes_only: false,
        reason: SessionReason::New,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionStart] {
        handler(&mut start, &context).await.expect("start");
    }
    let mut switch = ExtensionEvent::SessionBeforeSwitch {
        reason: SessionReason::New,
        target_session_file: None,
    };
    for handler in &api.registered.handlers[&EventKind::SessionBeforeSwitch] {
        handler(&mut switch, &context).await.expect("switch");
    }
    assert_eq!(
        *events.lock().expect("events"),
        ["task", "attach", "task", "detach"]
    );
}
#[tokio::test]
async fn disabled_usage_guidance_does_not_deliver() {
    let mut api = support::api();
    wire_task_usage_guidance(&mut api, Arc::new(|| false));
    let context = support::context();
    let mut event = ExtensionEvent::AgentStart;
    assert!(matches!(
        api.registered.handlers[&EventKind::BeforeAgentStart][0](&mut event, &context)
            .await
            .expect("event"),
        EventResult::None
    ));
}
