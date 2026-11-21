//! Port of `otlp-shared-not-before.test.ts`, with the cases `otlp-http.ts` and
//! `otlp-spool-not-before.ts` gained with it (omo#12758): one 429 pauses every process of an install.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use futures::FutureExt;
use serde_json::json;
use telemetry_core::AbortSignal;
use telemetry_core::NotBefore;
use telemetry_core::OtlpFetch;
use telemetry_core::OtlpRequest;
use telemetry_core::OtlpResponse;
use telemetry_core::PostDeps;
use telemetry_core::PostOutcome;
use telemetry_core::Signal;
use telemetry_core::SleepFn;
use telemetry_core::create_spool_not_before;
use telemetry_core::post_with_retry;

use crate::otlp_support::TransportOverrides;
use crate::otlp_support::capture;
use crate::otlp_support::create_spool_root;
use crate::otlp_support::make_transport;
use crate::otlp_support::ok_fetch;
use crate::otlp_support::recording_fetch;

type Log = Arc<Mutex<Vec<String>>>;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn beside(dir: &Path) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".not-before");
    PathBuf::from(name)
}

fn memory_not_before(initial: i64) -> (NotBefore, Arc<Mutex<i64>>) {
    let state = Arc::new(Mutex::new(initial));
    let reader = Arc::clone(&state);
    let writer = Arc::clone(&state);
    let not_before = NotBefore {
        read: Arc::new(move || *reader.lock().unwrap()),
        defer: Arc::new(move |until_ms: i64| {
            let mut current = writer.lock().unwrap();
            *current = (*current).max(until_ms);
        }),
    };
    (not_before, state)
}

fn recorded_sleep(log: Log, label: &'static str) -> SleepFn {
    Arc::new(move |ms: u64, _abort: AbortSignal| {
        log.lock().unwrap().push(format!("{label}:sleep:{ms}"));
        futures::future::ready(()).boxed()
    })
}

fn logged_fetch(log: Log, label: &'static str, inner: OtlpFetch) -> OtlpFetch {
    Arc::new(move |url: String, request: OtlpRequest| {
        log.lock().unwrap().push(format!("{label}:fetch"));
        inner(url, request)
    })
}

/// Answers `first` once, then 200.
fn scripted_fetch(first: (u16, Option<&'static str>), log: Log) -> OtlpFetch {
    let calls = Arc::new(Mutex::new(0usize));
    Arc::new(move |_url: String, _request: OtlpRequest| {
        log.lock().unwrap().push("post:fetch".to_string());
        let call = {
            let mut calls = calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        let (status, retry_after) = if call == 1 { first } else { (200, None) };
        let response = OtlpResponse {
            status,
            retry_after: retry_after.map(str::to_string),
            body: Vec::new(),
        };
        async move { Ok::<OtlpResponse, String>(response) }.boxed()
    })
}

fn deps(fetch: OtlpFetch, log: Log, not_before: NotBefore) -> PostDeps {
    PostDeps {
        fetch,
        sleep: recorded_sleep(log, "post"),
        random: Arc::new(|| 0.0),
        endpoint: "https://collector.test".to_string(),
        ingest_key: "test-ingest-key".to_string(),
        abort: AbortSignal::new(),
        request_timeout_ms: 10_000,
        before_attempt: None,
        not_before: Some(not_before),
    }
}

fn accepted() -> PostOutcome {
    PostOutcome::Accepted { partial: None }
}

#[test]
fn the_shared_time_lives_beside_the_spool_and_only_moves_forward() {
    let root = create_spool_root();
    let not_before = create_spool_not_before(&root.dir);
    assert_eq!((not_before.read)(), 0);

    (not_before.defer)(5_000);
    assert_eq!((not_before.read)(), 5_000);
    (not_before.defer)(3_000);
    assert_eq!((not_before.read)(), 5_000, "the time never moves backward");
    (not_before.defer)(9_000);

    assert_eq!(std::fs::read_to_string(beside(&root.dir)).unwrap(), "9000");
    assert_eq!((create_spool_not_before(&root.dir).read)(), 9_000);
    let names: Vec<String> = std::fs::read_dir(root.dir.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["telemetry-spool.not-before".to_string()]);
}

#[test]
fn a_missing_or_unreadable_file_means_send_now() {
    let root = create_spool_root();
    let not_before = create_spool_not_before(&root.dir);
    assert_eq!((not_before.read)(), 0);

    std::fs::write(beside(&root.dir), "not a number").unwrap();
    assert_eq!((not_before.read)(), 0);

    (not_before.defer)(7_000);
    assert_eq!((not_before.read)(), 7_000);
}

#[tokio::test]
async fn a_429_pushes_the_shared_time_forward_and_the_sender_never_waits_it_out_twice() {
    let log: Log = Arc::default();
    let (not_before, shared) = memory_not_before(0);
    let deps = deps(
        scripted_fetch((429, Some("30")), Arc::clone(&log)),
        Arc::clone(&log),
        not_before,
    );

    let before = now_ms();
    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;
    let after = now_ms();

    assert_eq!(outcome, accepted());
    assert_eq!(
        *log.lock().unwrap(),
        ["post:fetch", "post:sleep:30000", "post:fetch"]
    );
    let deferred = *shared.lock().unwrap();
    assert!(
        (before + 30_000..=after + 30_000).contains(&deferred),
        "{deferred}"
    );
}

#[tokio::test]
async fn a_plain_retry_leaves_the_shared_time_alone() {
    let log: Log = Arc::default();
    let (not_before, shared) = memory_not_before(0);
    let deps = deps(
        scripted_fetch((503, None), Arc::clone(&log)),
        Arc::clone(&log),
        not_before,
    );

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, accepted());
    assert_eq!(
        *log.lock().unwrap(),
        ["post:fetch", "post:sleep:1000", "post:fetch"]
    );
    assert_eq!(*shared.lock().unwrap(), 0);
}

#[tokio::test]
async fn another_sender_waits_the_shared_time_out_before_its_first_request() {
    let log: Log = Arc::default();
    let (not_before, _shared) = memory_not_before(now_ms() + 30_000);
    let deps = deps(
        scripted_fetch((200, None), Arc::clone(&log)),
        Arc::clone(&log),
        not_before,
    );

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, accepted());
    let log = log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[1], "post:fetch");
    let waited: u64 = log[0].strip_prefix("post:sleep:").unwrap().parse().unwrap();
    assert!(waited > 25_000 && waited <= 30_000, "{waited}");
}

#[tokio::test]
async fn the_wait_for_the_shared_time_is_capped_at_one_minute() {
    let log: Log = Arc::default();
    let (not_before, _shared) = memory_not_before(now_ms() + 600_000);
    let deps = deps(
        scripted_fetch((200, None), Arc::clone(&log)),
        Arc::clone(&log),
        not_before,
    );

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, accepted());
    assert_eq!(*log.lock().unwrap(), ["post:sleep:60000", "post:fetch"]);
}

#[tokio::test]
async fn a_429_seen_by_one_process_pauses_the_installs_other_processes() {
    let root = create_spool_root();
    let log: Log = Arc::default();
    let throttling = recording_fetch(Arc::new(|_url: &str, call: usize| OtlpResponse {
        status: if call == 1 { 429 } else { 200 },
        retry_after: if call == 1 {
            Some("30".to_string())
        } else {
            None
        },
        body: Vec::new(),
    }));
    let throttled = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(logged_fetch(
                Arc::clone(&log),
                "throttled",
                throttling.fetch.clone(),
            )),
            sleep: Some(recorded_sleep(Arc::clone(&log), "throttled")),
            ..TransportOverrides::default()
        },
    );
    capture(&throttled, "feature_used", json!({ "feature": "a" }));
    throttled.flush().await;

    let other_recorder = ok_fetch();
    let other = make_transport(
        &root.dir,
        TransportOverrides {
            fetch: Some(logged_fetch(
                Arc::clone(&log),
                "other",
                other_recorder.fetch.clone(),
            )),
            sleep: Some(recorded_sleep(Arc::clone(&log), "other")),
            ..TransportOverrides::default()
        },
    );
    capture(&other, "feature_used", json!({ "feature": "b" }));
    other.flush().await;

    let log = log.lock().unwrap().clone();
    let first_other_fetch = log
        .iter()
        .position(|entry| entry == "other:fetch")
        .expect("the other process sent its batch");
    let waited: Vec<u64> = log[..first_other_fetch]
        .iter()
        .filter_map(|entry| entry.strip_prefix("other:sleep:"))
        .map(|ms| ms.parse().unwrap())
        .collect();
    assert_eq!(waited.len(), 1, "{log:?}");
    assert!(waited[0] > 25_000 && waited[0] <= 30_000, "{log:?}");
}
