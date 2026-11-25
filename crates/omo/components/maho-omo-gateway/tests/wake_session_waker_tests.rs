//! The wake primitive, steps 1 to 5 (`thread/wake/session-waker.test.ts`, omo#13037 design 5.2, amendment A3.3):
//! resolve, refuse a child, take a place in the store, ensure the host, send one plain `open_session` frame.

use std::sync::{Arc, Mutex};

use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::wake::legacy_open::{
    WakeOpenReply, WakeOpenRequest, classify_open_frame, wake_open_frame,
};
use maho_omo_gateway::thread::wake::session_target::WakeSessionKind;
use maho_omo_gateway::thread::wake::session_waker::{
    SessionWaker, SessionWakerOptions, WakeOutcome, create_session_waker,
};
use maho_omo_gateway::thread::woken_types::{WakeStartOutcome, WokenSessionState};
use serde_json::{Value, json};

use crate::wake_support::{WakeHarness, write_session_file};

type Responder = Arc<dyn Fn(&Value, usize) -> Value + Send + Sync>;

struct WakerSetup {
    h: WakeHarness,
    frames: Arc<Mutex<Vec<Value>>>,
    ensured: Arc<Mutex<Vec<usize>>>,
    waker: SessionWaker,
}

fn opened() -> Value {
    json!({
        "success": true,
        "data": { "sessionId": "rpc-7", "attached": false, "state": { "sessionId": "rpc-7", "lifecycle": { "state": "done" } } },
    })
}

fn refusal(code: &str, error_data: Option<Value>) -> Value {
    let mut reply = json!({ "success": false, "error": code, "errorCode": code });
    if let Some(data) = error_data {
        reply["errorData"] = data;
    }
    reply
}

fn always(reply: Value) -> Responder {
    Arc::new(move |_frame: &Value, _index: usize| reply.clone())
}

fn setup(respond: Responder) -> WakerSetup {
    let h = WakeHarness::new();
    let frames: Arc<Mutex<Vec<Value>>> = Arc::default();
    let ensured: Arc<Mutex<Vec<usize>>> = Arc::default();
    let sent = frames.clone();
    let seen = frames.clone();
    let ensures = ensured.clone();
    let waker = create_session_waker(SessionWakerOptions {
        store: h.store.clone(),
        sessions_directory: h.sessions(),
        ensure_host: Arc::new(move || -> Result<(), String> {
            let count = seen.lock().expect("frames").len();
            ensures.lock().expect("ensured").push(count);
            Ok(())
        }),
        open: Arc::new(move |request: &WakeOpenRequest| -> WakeOpenReply {
            let frame = wake_open_frame(request);
            let index = {
                let mut frames = sent.lock().expect("frames");
                frames.push(frame.clone());
                frames.len() - 1
            };
            classify_open_frame(&respond(&frame, index))
        }),
    });
    WakerSetup {
        h,
        frames,
        ensured,
        waker,
    }
}

fn run(s: &WakerSetup, durable_id: &str) -> WakeOutcome {
    s.waker.ensure_session_running(durable_id).expect("wake")
}

fn frame_count(s: &WakerSetup) -> usize {
    s.frames.lock().expect("frames").len()
}

#[test]
fn given_a_session_that_ended_cleanly_when_it_is_woken_then_one_plain_open_session_frame_is_sent_after_the_host_is_ensured_and_the_row_is_left_opening()
 {
    let s = setup(always(opened()));
    let path = write_session_file(
        &s.h.sessions(),
        "s1",
        &json!({ "cwd": "/work/where-it-ran" }),
        "--tmp-project--",
    );

    assert_eq!(
        run(&s, "s1"),
        WakeOutcome::Live {
            session_id: "rpc-7".into(),
            state: json!({ "sessionId": "rpc-7", "lifecycle": { "state": "done" } }),
            generation: 1,
        }
    );
    assert_eq!(s.ensured.lock().expect("ensured").clone(), vec![0]);
    let frames = s.frames.lock().expect("frames").clone();
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0],
        json!({
            "type": "open_session",
            "sessionPath": path.to_string_lossy(),
            "cwd": "/work/where-it-ran",
            "retain_on_disconnect": true,
        })
    );
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, WokenSessionState::Opening);
    assert_eq!(rows[0].generation, 1);
}

#[test]
fn given_no_session_file_for_the_id_when_it_is_woken_then_it_is_gone_and_nothing_is_written() {
    let s = setup(always(opened()));
    assert_eq!(run(&s, "missing"), WakeOutcome::Gone);
    assert_eq!(frame_count(&s), 0);
    assert!(s.ensured.lock().expect("ensured").is_empty());
    assert!(s.h.store.woken().list().expect("list").is_empty());
}

#[test]
fn given_two_session_files_carrying_one_id_when_it_is_woken_then_it_is_ambiguous_and_nothing_is_opened()
 {
    let s = setup(always(opened()));
    let first = write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-one--");
    let second = write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-two--");
    let WakeOutcome::Ambiguous { mut session_paths } = run(&s, "s1") else {
        panic!("not ambiguous");
    };
    session_paths.sort();
    let mut expected = vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
    ];
    expected.sort();
    assert_eq!(session_paths, expected);
    assert_eq!(frame_count(&s), 0);
    assert!(s.h.store.woken().list().expect("list").is_empty());
}

#[test]
fn given_a_child_a_worker_or_an_unknown_kind_when_it_is_woken_then_it_is_not_wakeable_and_nothing_is_left()
 {
    for (header, kind) in [
        (
            json!({ "spawnedBy": { "parentSessionId": "p", "rootSessionId": "p", "taskId": "t1" } }),
            WakeSessionKind::Child,
        ),
        (json!({ "kind": "worker" }), WakeSessionKind::Worker),
        (json!({ "kind": "robot" }), WakeSessionKind::Unknown),
    ] {
        let s = setup(always(opened()));
        write_session_file(&s.h.sessions(), "s1", &header, "--tmp-project--");
        assert_eq!(
            run(&s, "s1"),
            WakeOutcome::NotWakeable { session_kind: kind },
            "{header}"
        );
        assert_eq!(frame_count(&s), 0);
        assert!(s.ensured.lock().expect("ensured").is_empty());
        assert!(s.h.store.woken().list().expect("list").is_empty());
    }
}

#[test]
fn given_four_woken_sessions_past_requested_when_a_fifth_is_woken_then_it_is_opened_with_no_count_cap()
 {
    let s = setup(always(opened()));
    for id in ["a", "b", "c", "d"] {
        s.h.store
            .woken()
            .request(id, &format!("/sessions/{id}.jsonl"))
            .expect("request");
        assert!(matches!(
            s.h.store.woken().start(id).expect("start"),
            WakeStartOutcome::Started { .. }
        ));
    }
    write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
    assert!(matches!(run(&s, "s1"), WakeOutcome::Live { .. }));
    assert_eq!(s.ensured.lock().expect("ensured").len(), 1);
    let rows = s.h.store.woken().list().expect("list");
    let s1 = rows
        .iter()
        .find(|row| row.durable_id == "s1")
        .expect("the s1 row");
    assert_eq!(s1.state, WokenSessionState::Opening);
}

#[test]
fn given_the_host_refuses_the_open_for_a_holder_or_a_bad_path_when_it_is_woken_then_the_answer_is_typed_and_the_row_is_ended()
 {
    for (reply, expected) in [
        (
            refusal(
                "session_held",
                Some(json!({ "holders": [{ "pid": 4242 }] })),
            ),
            WakeOutcome::Busy {
                code: "session_held".into(),
                retry_after_ms: None,
            },
        ),
        (
            refusal(
                "session_path_in_use",
                Some(json!({ "owner": { "pid": 4242, "current": true }, "retry_after_ms": 2000 })),
            ),
            WakeOutcome::Busy {
                code: "session_path_in_use".into(),
                retry_after_ms: Some(2000),
            },
        ),
        (refusal("invalid_path", None), WakeOutcome::Gone),
    ] {
        let s = setup(always(reply));
        write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
        assert_eq!(run(&s, "s1"), expected);
        assert_eq!(frame_count(&s), 1);
        assert!(s.h.store.woken().list().expect("list").is_empty());
    }
}

#[test]
fn given_a_transient_failure_when_it_is_woken_then_the_row_goes_back_to_requested_with_one_attempt_and_a_next_attempt_time()
 {
    let s = setup(always(refusal("host_stopping", None)));
    write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
    let outcome = run(&s, "s1");
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, WokenSessionState::Requested);
    assert_eq!(rows[0].attempts, 1);
    assert_eq!(rows[0].lease_owner, None);
    let next = rows[0].next_attempt_at.expect("a next attempt");
    assert!(next > s.h.now());
    assert_eq!(
        outcome,
        WakeOutcome::Deferred {
            attempts: 1,
            next_attempt_at: next,
        }
    );
    assert_eq!(frame_count(&s), 1);

    assert_eq!(
        run(&s, "s1"),
        WakeOutcome::NotYet {
            next_attempt_at: next
        }
    );
    assert_eq!(frame_count(&s), 1);
    assert_eq!(s.ensured.lock().expect("ensured").len(), 1);
}

#[test]
fn given_a_host_that_keeps_failing_for_a_transient_reason_when_it_is_woken_at_each_next_attempt_time_then_the_third_failure_deletes_the_row()
 {
    let s = setup(always(refusal("host_stopping", None)));
    write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let outcome = run(&s, "s1");
        if let WakeOutcome::Deferred {
            next_attempt_at, ..
        } = outcome
        {
            s.h.set_now(next_attempt_at);
            kinds.push("deferred");
        } else if let WakeOutcome::Failed { attempts, .. } = outcome {
            assert_eq!(attempts, 3);
            kinds.push("failed");
        } else {
            panic!("unexpected {outcome:?}");
        }
    }
    assert_eq!(kinds, ["deferred", "deferred", "failed"]);
    assert_eq!(frame_count(&s), 3);
    assert!(s.h.store.woken().list().expect("list").is_empty());
}

#[test]
fn given_a_host_that_fails_twice_then_opens_when_it_is_woken_at_each_next_attempt_time_then_the_third_call_answers_live()
 {
    let s = setup(Arc::new(|_frame: &Value, index: usize| {
        if index < 2 {
            refusal("host_stopping", None)
        } else {
            opened()
        }
    }));
    write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
    for _ in 0..2 {
        let WakeOutcome::Deferred {
            next_attempt_at, ..
        } = run(&s, "s1")
        else {
            panic!("expected deferred");
        };
        s.h.set_now(next_attempt_at);
    }
    assert!(matches!(run(&s, "s1"), WakeOutcome::Live { .. }));
    assert_eq!(frame_count(&s), 3);
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows[0].state, WokenSessionState::Opening);
    assert_eq!(rows[0].attempts, 2);
}

#[test]
fn given_another_carrier_already_started_the_row_when_it_is_woken_here_then_it_is_busy_and_the_other_carriers_row_is_untouched()
 {
    let s = setup(always(opened()));
    write_session_file(&s.h.sessions(), "s1", &json!({}), "--tmp-project--");
    let other = s.h.other_store();
    other
        .woken()
        .request("s1", "/sessions/s1.jsonl")
        .expect("request");
    assert!(matches!(
        other.woken().start("s1").expect("start"),
        WakeStartOutcome::Started { .. }
    ));

    assert_eq!(
        run(&s, "s1"),
        WakeOutcome::Busy {
            code: "wake_in_progress".into(),
            retry_after_ms: None,
        }
    );
    assert_eq!(frame_count(&s), 0);
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows[0].state, WokenSessionState::Opening);
    assert_eq!(rows[0].generation, 1);
    assert_eq!(
        rows[0].lease_owner,
        Some(other.identity().expect("identity"))
    );
}
