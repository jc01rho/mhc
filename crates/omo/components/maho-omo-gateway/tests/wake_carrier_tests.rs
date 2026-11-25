//! The carrier (`thread/wake/carrier.test.ts`, omo#13037 A3.1.3 to A3.1.8, A3.2): it lifts a woken session's gate,
//! sends the engine's wake, reads "finished" and hands the session back quietly, and follows the held rule and the
//! 30 minute rule, over a real store and an engine host that answers with the close probe's measured replies.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use maho_omo_gateway::thread::constants::WAKE_LEASE_MS;
use maho_omo_gateway::thread::intake_admission::IntakeAdmission;
use maho_omo_gateway::thread::session_wait_types::{
    QueuedWaitingOn, QueuedWaitingOnKind, SessionWaitKind,
};
use maho_omo_gateway::thread::store::SqliteGatewayStore;
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::RefusalReason;
use maho_omo_gateway::thread::wake::carrier::{
    AdmitIntake, Carrier, CarrierEvaluation, CarrierEvent, CarrierEventListener, CarrierOptions,
    CarrierWakeOutcome, CwdSwitch, MemoryFloor, MemoryProbe, Schedule, Switch, create_carrier,
};
use maho_omo_gateway::thread::wake::session_target::WakeSessionKind;
use maho_omo_gateway::thread::woken_types::{WakeTakeOverOutcome, WokenSessionState};
use serde_json::{Value, json};

use crate::wake_support::{
    FakeEngineHost, FakeEngineOptions, GIB, MIB, ManualSchedule, WakeHarness, append_line,
    background, finished, host_stamped, inert_schedule, parked_approval, record_unload_effect,
    stalled, stopped, streaming, write_session_file,
};

#[derive(Default, Clone)]
struct Knobs {
    engine: FakeEngineOptions,
    wake_limit: i64,
    same_device: Option<CwdSwitch>,
    remote_wake: Option<Switch>,
    admit_intake: Option<AdmitIntake>,
    memory_headroom: Option<MemoryProbe>,
    memory_floor: Option<MemoryFloor>,
}

struct Setup {
    h: WakeHarness,
    fake: FakeEngineHost,
    carrier: Carrier,
    events: Arc<Mutex<Vec<CarrierEvent>>>,
    schedule: Arc<ManualSchedule>,
}

fn same_device_on(on: bool) -> CwdSwitch {
    Arc::new(move |_cwd: &str| on)
}

fn switch_on(on: bool) -> Switch {
    Arc::new(move || on)
}

fn flag_switch(flag: &Arc<AtomicBool>) -> Switch {
    let flag = flag.clone();
    Arc::new(move || flag.load(Ordering::SeqCst))
}

fn admission_from(answer: &Arc<Mutex<IntakeAdmission>>) -> AdmitIntake {
    let answer = answer.clone();
    Arc::new(move |_device: &str| *answer.lock().expect("admission"))
}

fn headroom(bytes: u64) -> MemoryProbe {
    Arc::new(move || Some(bytes))
}

fn floor(bytes: u64) -> MemoryFloor {
    Arc::new(move || bytes)
}

fn host_stopping(refusing: &Arc<AtomicBool>) -> FakeEngineOptions {
    let refusing = refusing.clone();
    FakeEngineOptions {
        open_refusal: Some(Arc::new(move |_path: &str| -> Option<Value> {
            refusing.load(Ordering::SeqCst).then(|| {
                json!({ "success": false, "error": "host_stopping", "errorCode": "host_stopping" })
            })
        })),
        ..FakeEngineOptions::default()
    }
}

fn carrier_over(
    h: &WakeHarness,
    store: Arc<SqliteGatewayStore>,
    fake: &FakeEngineHost,
    knobs: &Knobs,
    schedule: Schedule,
    on_event: Option<CarrierEventListener>,
) -> Carrier {
    let wake_limit = knobs.wake_limit;
    create_carrier(CarrierOptions {
        store,
        sessions_directory: h.sessions(),
        ensure_host: Arc::new(|| -> Result<(), String> { Ok(()) }),
        connect: fake.connect(),
        wake_limit: Some(Arc::new(move || wake_limit)),
        same_device: Some(
            knobs
                .same_device
                .clone()
                .unwrap_or_else(|| same_device_on(true)),
        ),
        remote_wake: knobs.remote_wake.clone(),
        admit_intake: knobs.admit_intake.clone(),
        memory_headroom: knobs.memory_headroom.clone(),
        memory_floor: knobs.memory_floor.clone(),
        schedule,
        on_event,
    })
}

fn setup(knobs: Knobs) -> Setup {
    let h = WakeHarness::new();
    let fake = FakeEngineHost::new(knobs.engine.clone());
    let schedule = ManualSchedule::new();
    let events: Arc<Mutex<Vec<CarrierEvent>>> = Arc::default();
    let sink = events.clone();
    let listener: CarrierEventListener = Arc::new(move |event: &CarrierEvent| {
        sink.lock().expect("events").push(event.clone());
    });
    let carrier = carrier_over(
        &h,
        h.store.clone(),
        &fake,
        &knobs,
        schedule.schedule(),
        Some(listener),
    );
    // The carrier's start sweep reads an empty table and arms nothing.
    schedule.run_immediate();
    Setup {
        h,
        fake,
        carrier,
        events,
        schedule,
    }
}

fn successor(s: &Setup, knobs: &Knobs) -> Carrier {
    carrier_over(
        &s.h,
        s.h.other_store(),
        &s.fake,
        knobs,
        inert_schedule(),
        None,
    )
}

fn wake(carrier: &Carrier, durable_id: &str) -> CarrierWakeOutcome {
    carrier.wake(durable_id).expect("wake")
}

fn evaluate(carrier: &Carrier, durable_id: &str) -> CarrierEvaluation {
    carrier.evaluate(durable_id).expect("evaluate")
}

fn woken(s: &Setup) -> Vec<(String, WokenSessionState)> {
    s.h.store
        .woken()
        .list()
        .expect("list")
        .into_iter()
        .map(|row| (row.durable_id, row.state))
        .collect()
}

fn is_open(outcome: &CarrierWakeOutcome) -> bool {
    matches!(outcome, CarrierWakeOutcome::Open { .. })
}

fn interrupts(fake: &FakeEngineHost) -> usize {
    fake.frames("release_session")
        .iter()
        .filter(|frame| frame.get("interrupt") == Some(&json!(true)))
        .count()
}

#[test]
fn given_a_session_that_ended_cleanly_when_it_is_woken_and_found_finished_then_the_gate_lifts_the_wake_is_sent_and_a_quiet_release_ends_the_row()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");

    assert!(is_open(&wake(&s.carrier, "s1")));
    assert_eq!(woken(&s), vec![("s1".to_owned(), WokenSessionState::Open)]);
    assert_eq!(s.fake.frames("wake").len(), 1);
    let opens = s.fake.frames("open_session");
    let mut keys: Vec<&String> = opens[0]
        .as_object()
        .map(|frame| frame.keys().collect())
        .unwrap_or_default();
    keys.sort();
    assert_eq!(keys, ["cwd", "retain_on_disconnect", "sessionPath", "type"]);

    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
    let releases = s.fake.frames("release_session");
    assert_eq!(releases[0]["reason"], json!("takeover"));
    assert!(releases[0].get("interrupt").is_none());
    assert!(releases[0].get("force").is_none());
    assert_eq!(s.fake.frames("detach_connection").len(), 1);
    assert!(woken(&s).is_empty());
}

#[test]
fn given_rows_queued_for_a_session_that_was_then_closed_on_request_when_it_is_woken_then_nothing_opens_the_rows_are_refused_target_closed_and_a_send_after_the_close_opens_it()
 {
    let s = setup(Knobs {
        remote_wake: Some(switch_on(true)),
        admit_intake: Some(admission_from(&Arc::new(Mutex::new(
            IntakeAdmission::Admitted,
        )))),
        ..Knobs::default()
    });
    let file = s.h.path("s1");
    s.h.intake_row("s1", "d-before", "dev-b");
    s.h.advance(60_000);
    record_unload_effect(&file, "s1", s.h.now(), "done");
    s.h.advance(60_000);

    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::NothingQueued);
    assert!(s.fake.frames("open_session").is_empty());
    assert_eq!(
        s.h.rows("s1"),
        vec![("refused".to_owned(), Some("target_closed".to_owned())); 2]
    );

    s.h.intake_row("s1", "d-after", "dev-b");
    assert!(is_open(&wake(&s.carrier, "s1")));
}

#[test]
fn given_a_streaming_turn_when_the_carrier_looks_then_it_sends_no_release_and_keeps_the_row_and_releases_once_the_turn_settled()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, finished());
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, streaming());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    assert!(s.fake.frames("release_session").is_empty());
    assert_eq!(woken(&s).len(), 1);

    s.fake.set_snapshot(&path, finished());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
}

#[test]
fn given_a_finished_looking_session_with_a_queued_delivery_when_the_release_is_refused_for_it_then_the_carrier_keeps_it_without_an_interrupt_and_releases_once_the_delivery_ran()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_held_deliveries(&path, &["d-queued"]);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    assert_eq!(interrupts(&s.fake), 0);
    assert_eq!(woken(&s).len(), 1);

    s.fake.set_held_deliveries(&path, &[]);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
}

#[test]
fn given_a_queued_delivery_that_never_runs_when_the_release_is_still_refused_after_two_minutes_then_the_carrier_releases_with_interrupt()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_held_deliveries(&path, &["d-stuck"]);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    s.h.advance(2 * 60_000 - 1);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    assert_eq!(interrupts(&s.fake), 0);
    s.h.advance(1);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
    let releases = s.fake.frames("release_session");
    let last = releases.last().expect("a release");
    assert_eq!(last["reason"], json!("takeover"));
    assert_eq!(last["interrupt"], json!(true));
    assert!(woken(&s).is_empty());
}

#[test]
fn given_a_refused_delivery_then_a_working_session_then_a_new_refusal_when_two_minutes_pass_from_the_first_then_the_carrier_still_keeps_it()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_held_deliveries(&path, &["d-1"]);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    s.h.advance(90_000);
    s.fake.set_snapshot(&path, streaming());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    s.fake.set_snapshot(&path, finished());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    s.h.advance(60_000);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    assert_eq!(interrupts(&s.fake), 0);
}

#[test]
fn given_background_work_when_the_carrier_looks_then_it_never_sends_the_release() {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, background());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Kept);
    assert!(s.fake.frames("release_session").is_empty());
}

#[test]
fn given_a_session_whose_only_wait_is_stalled_when_the_carrier_looks_then_it_counts_as_finished() {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, stalled());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
}

#[test]
fn given_a_persons_client_attached_meanwhile_when_the_carrier_looks_then_it_hands_the_session_over_and_resets_the_wake_count()
 {
    let s = setup(Knobs {
        wake_limit: 5,
        ..Knobs::default()
    });
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    assert_eq!(s.h.store.woken().wake_count("s1").expect("count"), 1);
    s.fake.set_others(&path, 1);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::HandedOver);
    assert_eq!(s.fake.frames("release_attachment").len(), 1);
    assert!(woken(&s).is_empty());
    assert_eq!(s.h.store.woken().wake_count("s1").expect("count"), 0);
}

#[test]
fn given_a_host_that_answers_the_release_as_an_unknown_command_when_the_carrier_closes_then_the_close_failed_and_the_row_ends()
 {
    let s = setup(Knobs {
        engine: FakeEngineOptions {
            release_unknown: true,
            ..FakeEngineOptions::default()
        },
        ..Knobs::default()
    });
    s.h.path("s1");
    wake(&s.carrier, "s1");
    assert_eq!(
        evaluate(&s.carrier, "s1"),
        CarrierEvaluation::CloseFailed {
            code: "unknown_command".into()
        }
    );
    assert!(woken(&s).is_empty());
}

#[test]
fn given_a_lifecycle_event_on_the_carriers_connection_when_the_session_settles_then_the_carrier_looks_again_by_itself()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, streaming());
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, finished());

    s.fake.emit_lifecycle(&path);
    assert_eq!(s.schedule.run_immediate(), 1);

    assert_eq!(
        s.events.lock().expect("events").clone(),
        vec![CarrierEvent {
            durable_id: "s1".into(),
            outcome: CarrierEvaluation::Released,
        }]
    );
    assert!(woken(&s).is_empty());
}

#[test]
fn given_an_approval_that_arose_after_admission_when_thirty_minutes_pass_then_the_state_is_read_again_and_close_session_ends_it()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, parked_approval());
    assert_eq!(
        evaluate(&s.carrier, "s1"),
        CarrierEvaluation::Waiting { since: s.h.now() }
    );
    s.h.advance(30 * 60_000 - 1);
    assert!(matches!(
        evaluate(&s.carrier, "s1"),
        CarrierEvaluation::Waiting { .. }
    ));
    assert!(s.fake.frames("close_session").is_empty());
    s.h.advance(1);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::ClosedWaiting);
    assert_eq!(s.fake.frames("close_session").len(), 1);
    assert!(woken(&s).is_empty());
}

#[test]
fn given_a_wait_that_ended_before_thirty_minutes_when_the_carrier_reads_no_wait_then_the_first_seen_time_is_cleared()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, parked_approval());
    evaluate(&s.carrier, "s1");
    s.fake.set_snapshot(&path, streaming());
    evaluate(&s.carrier, "s1");
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows[0].first_waiting_at, None);
    s.h.advance(31 * 60_000);
    s.fake.set_snapshot(&path, parked_approval());
    assert_eq!(
        evaluate(&s.carrier, "s1"),
        CarrierEvaluation::Waiting { since: s.h.now() }
    );
}

#[test]
fn given_a_question_its_owner_left_unanswered_when_the_wake_opens_it_then_nothing_is_lifted_the_wait_is_stored_and_the_row_ends()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, parked_approval());
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::Waiting);
    assert!(s.fake.frames("wake").is_empty());
    assert!(s.fake.frames("close_session").is_empty());
    let wait =
        s.h.store
            .session_wait()
            .read("s1")
            .expect("read")
            .expect("a stored wait");
    assert_eq!(wait.wait, SessionWaitKind::Waiting);
    assert!(woken(&s).is_empty());
}

#[test]
fn given_a_stop_left_on_the_session_when_it_is_woken_then_the_hold_is_stored_a_quiet_release_ends_it_and_it_is_not_woken_again()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, stopped());
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::Held);
    assert!(s.fake.frames("wake").is_empty());
    let releases = s.fake.frames("release_session");
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0]["reason"], json!("takeover"));
    let wait =
        s.h.store
            .session_wait()
            .read("s1")
            .expect("read")
            .expect("a stored hold");
    assert_eq!(wait.wait, SessionWaitKind::Held);
    assert_eq!(wait.hold_kinds, vec!["session_hold".to_owned()]);
    assert!(woken(&s).is_empty());

    let opens = s.fake.frames("open_session").len();
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::Held);
    assert_eq!(s.fake.frames("open_session").len(), opens);
}

#[test]
fn given_a_held_state_whose_file_grew_when_it_is_woken_then_one_new_probe_opens_it_and_a_released_hold_lets_it_run()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, stopped());
    wake(&s.carrier, "s1");
    append_line(&path, &json!({ "type": "custom", "customType": "resume" }));
    s.fake.set_snapshot(&path, finished());
    assert!(is_open(&wake(&s.carrier, "s1")));
    assert_eq!(s.h.store.session_wait().read("s1").expect("read"), None);
}

#[test]
fn given_a_session_the_carrier_handed_back_held_unattended_when_a_later_wake_comes_then_the_stored_hold_does_not_block_it()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    assert!(is_open(&wake(&s.carrier, "s1")));
    s.fake.set_snapshot(&path, host_stamped());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Held);
    let wait =
        s.h.store
            .session_wait()
            .read("s1")
            .expect("read")
            .expect("a stored hold");
    assert_eq!(wait.hold_kinds, vec!["unattended".to_owned()]);

    s.fake.set_snapshot(&path, finished());
    assert!(is_open(&wake(&s.carrier, "s1")));
    assert_eq!(s.fake.frames("open_session").len(), 2);
    assert_eq!(s.fake.frames("wake").len(), 2);
}

#[test]
fn given_a_hold_that_appears_after_the_lift_with_a_delivery_held_as_a_notice_when_the_carrier_looks_then_it_releases_with_interrupt()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    s.fake.set_snapshot(&path, stopped());
    s.fake.set_held_deliveries(&path, &["d-held"]);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Held);
    let releases = s.fake.frames("release_session");
    let last = releases.last().expect("a release");
    assert_eq!(last["reason"], json!("takeover"));
    assert_eq!(last["interrupt"], json!(true));
    assert!(woken(&s).is_empty());
}

#[test]
fn given_a_session_a_host_restart_stamped_and_still_holds_when_it_is_woken_then_the_carrier_releases_only_its_own_attachment()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, host_stamped());
    s.fake.set_others(&path, 1);
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::Held);
    assert_eq!(s.fake.frames("release_attachment").len(), 1);
    assert!(s.fake.frames("close_session").is_empty());
    assert!(woken(&s).is_empty());
}

#[test]
fn given_four_held_sessions_woken_in_turn_when_a_fifth_is_woken_then_it_still_opens() {
    let s = setup(Knobs::default());
    for id in ["a", "b", "c", "d"] {
        let path = s.h.path(id);
        s.fake.set_snapshot(&path, stopped());
        assert_eq!(wake(&s.carrier, id), CarrierWakeOutcome::Held);
    }
    s.h.path("e");
    assert!(is_open(&wake(&s.carrier, "e")));
}

#[test]
fn given_a_row_another_carrier_took_over_after_the_lease_ran_out_when_the_first_carrier_looks_then_it_only_releases_its_own_attachment()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");
    wake(&s.carrier, "s1");
    s.h.advance(WAKE_LEASE_MS + 1);
    assert!(matches!(
        s.h.other_store()
            .woken()
            .take_over("s1")
            .expect("take over"),
        WakeTakeOverOutcome::TakenOver { .. }
    ));
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::LostLease);
    assert_eq!(s.fake.frames("release_attachment").len(), 1);
    assert!(s.fake.frames("release_session").is_empty());
    let rows = s.h.store.woken().list().expect("list");
    assert_eq!(rows[0].generation, 2);
}

#[test]
fn given_a_carrier_that_died_after_the_open_when_another_carrier_sweeps_past_the_lease_then_it_takes_the_row_over_and_ends_the_finished_session()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");
    wake(&s.carrier, "s1");
    s.carrier.stop();
    s.h.advance(WAKE_LEASE_MS + 1);
    successor(&s, &Knobs::default()).sweep().expect("sweep");
    assert!(woken(&s).is_empty());
    assert_eq!(s.fake.frames("release_session").len(), 1);
    let cwds: Vec<Value> = s
        .fake
        .frames("open_session")
        .iter()
        .map(|frame| frame["cwd"].clone())
        .collect();
    assert_eq!(cwds, vec![json!("/work/project"), json!("/work/project")]);
}

#[test]
fn given_a_table_holding_a_row_whose_lease_ran_out_when_a_successor_starts_with_no_wake_of_its_own_then_it_takes_the_row_over_at_its_start()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");
    wake(&s.carrier, "s1");
    s.carrier.stop();
    s.h.advance(WAKE_LEASE_MS + 1);
    let schedule = ManualSchedule::new();
    let events: Arc<Mutex<Vec<CarrierEvent>>> = Arc::default();
    let sink = events.clone();
    let _successor = carrier_over(
        &s.h,
        s.h.other_store(),
        &s.fake,
        &Knobs::default(),
        schedule.schedule(),
        Some(Arc::new(move |event: &CarrierEvent| {
            sink.lock().expect("events").push(event.clone());
        })),
    );
    assert_eq!(schedule.run_immediate(), 1);
    assert_eq!(
        events.lock().expect("events").clone(),
        vec![CarrierEvent {
            durable_id: "s1".into(),
            outcome: CarrierEvaluation::Released,
        }]
    );
    assert!(woken(&s).is_empty());
    assert_eq!(s.fake.frames("release_session").len(), 1);
}

#[test]
fn given_a_carrier_that_died_after_the_open_when_the_session_is_gone_or_ambiguous_at_the_takeover_then_the_row_ends_and_nothing_is_opened()
 {
    for ambiguous in [false, true] {
        let s = setup(Knobs::default());
        let file = s.h.path("s1");
        wake(&s.carrier, "s1");
        s.carrier.stop();
        if ambiguous {
            write_session_file(&s.h.sessions(), "s1", &json!({}), "--other-project--");
        } else {
            std::fs::remove_file(&file).expect("remove");
        }
        s.h.advance(WAKE_LEASE_MS + 1);
        successor(&s, &Knobs::default()).sweep().expect("sweep");
        assert!(woken(&s).is_empty(), "ambiguous: {ambiguous}");
        assert_eq!(
            s.fake.frames("open_session").len(),
            1,
            "ambiguous: {ambiguous}"
        );
    }
}

#[test]
fn given_a_session_kind_this_build_does_not_know_when_it_is_woken_then_it_is_refused_not_wakeable_as_unknown()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");
    write_session_file(
        &s.h.sessions(),
        "s1",
        &json!({ "kind": "robot" }),
        "--tmp-project--",
    );
    assert_eq!(
        wake(&s.carrier, "s1"),
        CarrierWakeOutcome::NotWakeable {
            session_kind: WakeSessionKind::Unknown
        }
    );
    assert!(s.fake.frames("open_session").is_empty());
}

#[test]
fn given_a_carrier_that_died_after_the_open_when_the_session_file_now_names_a_child_or_a_worker_then_the_takeover_ends_the_row()
 {
    for header in [
        json!({ "spawnedBy": { "session_id": "parent-s" } }),
        json!({ "kind": "worker" }),
    ] {
        let s = setup(Knobs::default());
        s.h.path("s1");
        wake(&s.carrier, "s1");
        s.carrier.stop();
        write_session_file(&s.h.sessions(), "s1", &header, "--tmp-project--");
        s.h.advance(WAKE_LEASE_MS + 1);
        successor(&s, &Knobs::default()).sweep().expect("sweep");
        assert!(woken(&s).is_empty(), "{header}");
        assert_eq!(s.fake.frames("open_session").len(), 1, "{header}");
    }
}

#[test]
fn given_carrier_a_carrying_an_open_session_whose_last_row_was_settled_when_carrier_b_is_asked_to_wake_it_then_b_answers_busy_and_a_still_closes_it()
 {
    let s = setup(Knobs::default());
    s.h.path("s1");
    assert!(is_open(&wake(&s.carrier, "s1")));
    let before = s.h.store.woken().list().expect("list");
    for row in s.h.store.list(Some("s1"), None).expect("rows") {
        s.h.store
            .refuse_queued(
                s.h.store.now(),
                &row.delivery_id,
                RefusalReason::SenderNotAdmitted,
            )
            .expect("refuse");
    }
    assert_eq!(
        wake(&successor(&s, &Knobs::default()), "s1"),
        CarrierWakeOutcome::Busy {
            code: "wake_in_progress".into()
        }
    );
    assert_eq!(s.h.store.woken().list().expect("list"), before);
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
    assert_eq!(s.fake.frames("release_session").len(), 1);
}

#[test]
fn given_a_remote_row_deferred_by_a_failed_open_when_the_sweep_retries_it_and_the_senders_admission_answers_unknown_then_nothing_opens_until_it_is_admitted()
 {
    let refusing = Arc::new(AtomicBool::new(true));
    let admission = Arc::new(Mutex::new(IntakeAdmission::Admitted));
    let s = setup(Knobs {
        engine: host_stopping(&refusing),
        remote_wake: Some(switch_on(true)),
        admit_intake: Some(admission_from(&admission)),
        ..Knobs::default()
    });
    s.h.file("t");
    s.h.intake_row("t", "d-remote", "dev-b");
    assert!(matches!(
        wake(&s.carrier, "t"),
        CarrierWakeOutcome::Deferred { .. }
    ));
    assert_eq!(s.fake.frames("open_session").len(), 1);

    refusing.store(false, Ordering::SeqCst);
    *admission.lock().expect("admission") = IntakeAdmission::Unknown;
    let due = s.h.store.woken().list().expect("list")[0]
        .next_attempt_at
        .expect("a next attempt");
    s.h.set_now(due + 1);
    s.carrier.sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 1);
    assert_eq!(
        woken(&s),
        vec![("t".to_owned(), WokenSessionState::Requested)]
    );

    *admission.lock().expect("admission") = IntakeAdmission::Admitted;
    s.carrier.sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 2);
    assert_eq!(s.fake.frames("wake").len(), 1);
}

#[test]
fn given_a_carrier_that_died_after_opening_for_a_remote_row_when_its_successor_finds_the_sender_refused_then_nothing_is_reopened_and_the_row_ends()
 {
    let admission = Arc::new(Mutex::new(IntakeAdmission::Admitted));
    let knobs = Knobs {
        remote_wake: Some(switch_on(true)),
        admit_intake: Some(admission_from(&admission)),
        ..Knobs::default()
    };
    let s = setup(knobs.clone());
    s.h.file("t");
    s.h.intake_row("t", "d-remote", "dev-b");
    assert!(is_open(&wake(&s.carrier, "t")));
    s.carrier.stop();
    *admission.lock().expect("admission") = IntakeAdmission::Refused;
    s.h.advance(WAKE_LEASE_MS + 1);
    successor(&s, &knobs).sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 1);
    assert!(woken(&s).is_empty());

    *admission.lock().expect("admission") = IntakeAdmission::Admitted;
    assert!(is_open(&wake(&successor(&s, &knobs), "t")));
    assert_eq!(s.fake.frames("open_session").len(), 2);
}

#[test]
fn given_a_same_device_wake_deferred_while_the_switch_was_on_when_the_switch_is_off_at_the_sweep_then_nothing_opens_until_it_is_on_again()
 {
    let refusing = Arc::new(AtomicBool::new(true));
    let on = Arc::new(AtomicBool::new(true));
    let flag = on.clone();
    let s = setup(Knobs {
        engine: host_stopping(&refusing),
        same_device: Some(Arc::new(move |_cwd: &str| flag.load(Ordering::SeqCst))),
        ..Knobs::default()
    });
    s.h.path("t");
    assert!(matches!(
        wake(&s.carrier, "t"),
        CarrierWakeOutcome::Deferred { .. }
    ));
    refusing.store(false, Ordering::SeqCst);
    on.store(false, Ordering::SeqCst);
    let due = s.h.store.woken().list().expect("list")[0]
        .next_attempt_at
        .expect("a next attempt");
    s.h.set_now(due + 1);
    s.carrier.sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 1);
    assert_eq!(
        woken(&s),
        vec![("t".to_owned(), WokenSessionState::Requested)]
    );
    on.store(true, Ordering::SeqCst);
    s.carrier.sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 2);
    assert_eq!(s.fake.frames("wake").len(), 1);
}

#[test]
fn given_a_session_holding_a_row_of_each_kind_when_the_switches_read_each_way_then_an_open_happens_only_when_one_allows_it()
 {
    for (remote, same, opens) in [
        (false, false, false),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ] {
        let s = setup(Knobs {
            same_device: Some(same_device_on(same)),
            remote_wake: Some(switch_on(remote)),
            ..Knobs::default()
        });
        s.h.path("t");
        s.h.intake_row("t", "d-remote", "dev-b");
        let outcome = wake(&s.carrier, "t");
        if opens {
            assert!(is_open(&outcome), "{remote} {same}: {outcome:?}");
        } else {
            assert_eq!(outcome, CarrierWakeOutcome::SwitchedOff, "{remote} {same}");
        }
        assert_eq!(
            s.fake.frames("open_session").len(),
            usize::from(opens),
            "{remote} {same}"
        );
    }
}

#[test]
fn given_a_session_whose_only_row_is_from_another_device_when_the_consumer_was_told_off_then_the_row_waits_and_it_opens_when_told_on()
 {
    let told = Arc::new(AtomicBool::new(false));
    let s = setup(Knobs {
        same_device: Some(same_device_on(false)),
        remote_wake: Some(flag_switch(&told)),
        ..Knobs::default()
    });
    s.h.intake_row("t", "d-remote", "dev-b");
    s.h.file("t");
    assert_eq!(wake(&s.carrier, "t"), CarrierWakeOutcome::SwitchedOff);
    assert_eq!(s.h.rows("t"), vec![("queued".to_owned(), None)]);
    assert_eq!(
        woken(&s),
        vec![("t".to_owned(), WokenSessionState::Requested)]
    );
    told.store(true, Ordering::SeqCst);
    s.carrier.sweep().expect("sweep");
    assert_eq!(s.fake.frames("open_session").len(), 1);
}

#[test]
fn given_a_wake_that_is_told_off_while_the_session_is_being_opened_when_the_open_finishes_then_the_row_is_carried()
 {
    let told = Arc::new(AtomicBool::new(true));
    let turned_off = told.clone();
    let s = setup(Knobs {
        engine: FakeEngineOptions {
            open_refusal: Some(Arc::new(move |_path: &str| -> Option<Value> {
                turned_off.store(false, Ordering::SeqCst);
                None
            })),
            ..FakeEngineOptions::default()
        },
        same_device: Some(same_device_on(false)),
        remote_wake: Some(flag_switch(&told)),
        ..Knobs::default()
    });
    s.h.path("t");
    s.h.intake_row("t", "d-remote", "dev-b");
    assert!(is_open(&wake(&s.carrier, "t")));
    assert_eq!(s.fake.frames("wake").len(), 1);
}

#[test]
fn given_a_session_with_no_queued_row_when_the_sweep_finds_its_requested_record_then_nothing_opens_and_the_record_ends()
 {
    let s = setup(Knobs::default());
    let file = s.h.file("t");
    s.h.store
        .woken()
        .request("t", &file.to_string_lossy())
        .expect("request");
    s.carrier.sweep().expect("sweep");
    assert!(s.fake.frames("open_session").is_empty());
    assert!(woken(&s).is_empty());
}

#[test]
fn given_the_default_wake_limit_when_a_session_is_woken_many_times_then_nothing_is_refused() {
    let s = setup(Knobs::default());
    s.h.path("s1");
    for _ in 0..6 {
        assert!(is_open(&wake(&s.carrier, "s1")));
        assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
    }
}

#[test]
fn given_the_wake_limit_at_two_when_the_third_consecutive_wake_comes_then_it_is_refused_and_nothing_opens()
 {
    let s = setup(Knobs {
        wake_limit: 2,
        ..Knobs::default()
    });
    s.h.path("s1");
    for _ in 0..2 {
        wake(&s.carrier, "s1");
        evaluate(&s.carrier, "s1");
    }
    let opens = s.fake.frames("open_session").len();
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::WakeLimitReached);
    assert_eq!(s.fake.frames("open_session").len(), opens);
}

#[test]
fn given_the_wake_limit_at_two_when_a_persons_input_moved_the_input_revision_between_wakes_then_the_count_starts_again()
 {
    let s = setup(Knobs {
        wake_limit: 2,
        ..Knobs::default()
    });
    let path = s.h.path("s1");
    wake(&s.carrier, "s1");
    evaluate(&s.carrier, "s1");
    let mut moved = finished();
    moved["lifecycle"]["humanInputRevision"] = json!(7);
    s.fake.set_snapshot(&path, moved);
    wake(&s.carrier, "s1");
    evaluate(&s.carrier, "s1");
    assert!(is_open(&wake(&s.carrier, "s1")));
    evaluate(&s.carrier, "s1");
    assert_eq!(wake(&s.carrier, "s1"), CarrierWakeOutcome::WakeLimitReached);
}

#[test]
fn given_nine_sessions_a_host_restart_parked_when_the_carrier_sweeps_with_plenty_of_memory_then_all_nine_are_opened_and_woken()
 {
    let s = setup(Knobs {
        memory_headroom: Some(headroom(64 * GIB)),
        ..Knobs::default()
    });
    let ids: Vec<String> = (1..=9).map(|index| format!("parked-{index}")).collect();
    for id in &ids {
        let path = s.h.path(id);
        s.h.store
            .woken()
            .request(id, &path.to_string_lossy())
            .expect("request");
    }
    s.carrier.sweep().expect("sweep");
    let mut rows = woken(&s);
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let expected: Vec<(String, WokenSessionState)> = ids
        .iter()
        .map(|id| (id.clone(), WokenSessionState::Open))
        .collect();
    assert_eq!(rows, expected);
    assert_eq!(s.fake.frames("open_session").len(), 9);
    assert_eq!(s.fake.frames("wake").len(), 9);
}

#[test]
fn given_host_memory_headroom_below_the_floor_when_a_session_is_woken_then_the_wake_is_deferred_with_the_numbers_and_opens_once_the_probe_reports_enough()
 {
    let available = Arc::new(AtomicU64::new(512 * MIB));
    let probe = available.clone();
    let s = setup(Knobs {
        memory_headroom: Some(Arc::new(move || Some(probe.load(Ordering::SeqCst)))),
        ..Knobs::default()
    });
    s.h.path("s1");
    assert_eq!(
        wake(&s.carrier, "s1"),
        CarrierWakeOutcome::HostMemoryPressure {
            available_bytes: 512 * MIB,
            floor_bytes: GIB,
        }
    );
    assert!(s.fake.frames("open_session").is_empty());
    assert_eq!(
        woken(&s),
        vec![("s1".to_owned(), WokenSessionState::Requested)]
    );
    assert_eq!(
        s.h.store
            .session_wait()
            .waiting_on("s1")
            .expect("waiting on"),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Wake
        })
    );

    available.store(8 * GIB, Ordering::SeqCst);
    s.carrier.sweep().expect("sweep");
    assert_eq!(woken(&s), vec![("s1".to_owned(), WokenSessionState::Open)]);
    assert_eq!(s.fake.frames("open_session").len(), 1);
    assert_eq!(s.fake.frames("wake").len(), 1);
}

#[test]
fn given_a_768_mib_cgroup_whose_floor_is_one_wakes_cost_and_500_mib_of_room_when_a_session_is_woken_then_it_opens()
 {
    let s = setup(Knobs {
        memory_headroom: Some(headroom(500 * MIB)),
        memory_floor: Some(floor(409 * MIB)),
        ..Knobs::default()
    });
    let path = s.h.path("s1");
    s.h.store
        .woken()
        .request("s1", &path.to_string_lossy())
        .expect("request");
    s.carrier.sweep().expect("sweep");
    assert_eq!(woken(&s), vec![("s1".to_owned(), WokenSessionState::Open)]);
    assert_eq!(s.fake.frames("wake").len(), 1);
}

#[test]
fn given_room_below_that_floor_when_a_session_is_woken_then_the_deferral_names_the_floor_it_was_measured_against()
 {
    let s = setup(Knobs {
        memory_headroom: Some(headroom(300 * MIB)),
        memory_floor: Some(floor(409 * MIB)),
        ..Knobs::default()
    });
    s.h.path("s1");
    assert_eq!(
        wake(&s.carrier, "s1"),
        CarrierWakeOutcome::HostMemoryPressure {
            available_bytes: 300 * MIB,
            floor_bytes: 409 * MIB,
        }
    );
}

#[test]
fn given_a_woken_session_when_the_wake_arms_the_timer_then_one_renewal_tick_is_scheduled_and_it_rearms_while_the_session_is_carried()
 {
    let s = setup(Knobs::default());
    let path = s.h.path("s1");
    s.fake.set_snapshot(&path, streaming());
    assert!(is_open(&wake(&s.carrier, "s1")));
    assert_eq!(s.schedule.delays(), vec![20_000]);
    assert_eq!(s.schedule.run_queued(), 1);
    assert_eq!(s.schedule.delays(), vec![20_000]);
    s.fake.set_snapshot(&path, finished());
    assert_eq!(evaluate(&s.carrier, "s1"), CarrierEvaluation::Released);
    assert_eq!(s.schedule.run_queued(), 1);
    assert!(s.schedule.delays().is_empty());
}
