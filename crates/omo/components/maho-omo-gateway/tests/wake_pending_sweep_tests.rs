//! The pending sweep (`thread/wake/pending-sweep.test.ts`, omo#13037 design 5.4 step 6, 5.6 at wake): every session
//! holding open intake rows is woken once its senders pass the admission check; a refused sender's rows settle refused,
//! and same-device rows wake nothing while the same-device switch is off.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use maho_omo_gateway::thread::intake_admission::IntakeAdmission;
use maho_omo_gateway::thread::result::refusal_mapping;
use maho_omo_gateway::thread::wake::carrier::AdmitIntake;
use maho_omo_gateway::thread::wake::pending_sweep::{
    PendingSweepOptions, SessionSwitch, run_pending_sweep,
};
use maho_omo_gateway::thread::wake::session_target::{WakeTarget, resolve_wake_target};
use serde_json::json;

use crate::wake_support::{WakeHarness, record_unload_effect, write_session_file};

fn switch(on: bool) -> SessionSwitch {
    Arc::new(move |_durable_id: &str| on)
}

fn deciding(decide: fn(&str) -> IntakeAdmission) -> AdmitIntake {
    Arc::new(move |device: &str| decide(device))
}

fn sweep(h: &WakeHarness, admit: AdmitIntake, same_device: SessionSwitch) -> Vec<String> {
    let woken: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = woken.clone();
    let sessions: PathBuf = h.sessions();
    run_pending_sweep(&PendingSweepOptions {
        store: h.store.clone(),
        admit_intake: admit,
        wake: Arc::new(move |durable_id: &str| -> Result<(), String> {
            sink.lock().expect("woken").push(durable_id.to_owned());
            Ok(())
        }),
        same_device,
        // The production reader over the sessions directory: a close is what the session's transcript records.
        closed_at: Arc::new(move |durable_id: &str| {
            match resolve_wake_target(&sessions, durable_id) {
                WakeTarget::Found { closed_at, .. } => closed_at,
                WakeTarget::Gone | WakeTarget::Ambiguous { .. } => None,
            }
        }),
    })
    .expect("sweep");
    let mut woken = woken.lock().expect("woken").clone();
    woken.sort();
    woken
}

#[test]
fn given_sessions_with_intake_rows_when_the_sweep_runs_then_an_admitted_senders_session_wakes_a_refused_senders_rows_settle_and_an_unanswered_one_waits()
 {
    let h = WakeHarness::new();
    h.intake_row("t-ok", "d-1", "dev-ok");
    h.intake_row("t-no", "d-2", "dev-no");
    h.intake_row("t-silent", "d-3", "dev-silent");
    let decide = |device: &str| match device {
        "dev-ok" => IntakeAdmission::Admitted,
        "dev-no" => IntakeAdmission::Refused,
        _ => IntakeAdmission::Unknown,
    };
    assert_eq!(sweep(&h, deciding(decide), switch(false)), ["t-ok"]);
    assert_eq!(
        h.rows("t-no"),
        vec![("refused".to_owned(), Some("sender_not_admitted".to_owned()))]
    );
    assert_eq!(h.rows("t-silent"), vec![("queued".to_owned(), None)]);
}

#[test]
fn given_a_session_whose_only_row_is_from_another_device_when_the_sweep_runs_then_it_wakes_on_admission_without_reading_its_same_device_setting()
 {
    let h = WakeHarness::new();
    h.intake_row("t-remote", "d-remote", "dev-b");
    let asked = Arc::new(Mutex::new(0_usize));
    let counted = asked.clone();
    let woken = sweep(
        &h,
        deciding(|_device: &str| IntakeAdmission::Admitted),
        Arc::new(move |_durable_id: &str| {
            *counted.lock().expect("asked") += 1;
            true
        }),
    );
    assert_eq!(woken, ["t-remote"]);
    assert_eq!(*asked.lock().expect("asked"), 0);
}

#[test]
fn given_a_send_queued_for_wake_on_a_thread_then_closed_on_request_when_the_next_sweep_admits_its_sender_then_the_thread_is_not_woken_and_the_row_is_refused_target_closed()
 {
    let h = WakeHarness::new();
    let path = write_session_file(&h.sessions(), "t-closed", &json!({}), "--tmp-project--");
    h.intake_row("t-closed", "d-before", "dev-b");
    assert!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Unknown),
            switch(false)
        )
        .is_empty()
    );
    assert_eq!(h.rows("t-closed"), vec![("queued".to_owned(), None)]);
    h.advance(60_000);
    record_unload_effect(&path, "t-closed", h.now(), "done");
    h.advance(60_000);

    assert!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Admitted),
            switch(false)
        )
        .is_empty()
    );
    assert_eq!(
        h.rows("t-closed"),
        vec![("refused".to_owned(), Some("target_closed".to_owned()))]
    );
    assert_eq!(refusal_mapping(Some("target_closed")).0, "not_resumable");
    assert!(
        h.store
            .intake_marks()
            .open_sessions()
            .expect("open sessions")
            .is_empty()
    );
}

#[test]
fn given_a_closed_thread_that_is_sent_to_after_its_close_when_the_sweep_runs_then_only_the_row_from_before_the_close_is_refused()
 {
    let h = WakeHarness::new();
    let path = write_session_file(&h.sessions(), "t-closed", &json!({}), "--tmp-project--");
    h.intake_row("t-closed", "d-before", "dev-b");
    h.advance(60_000);
    record_unload_effect(&path, "t-closed", h.now(), "done");
    h.advance(60_000);
    h.intake_row("t-closed", "d-after", "dev-b");

    assert_eq!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Admitted),
            switch(false)
        ),
        ["t-closed"]
    );
    assert_eq!(
        h.rows("t-closed"),
        vec![
            ("refused".to_owned(), Some("target_closed".to_owned())),
            ("queued".to_owned(), None),
        ]
    );
}

#[test]
fn given_a_close_the_holder_refused_when_the_sweep_runs_then_the_queued_send_still_wakes_the_thread()
 {
    let h = WakeHarness::new();
    let path = write_session_file(&h.sessions(), "t-open", &json!({}), "--tmp-project--");
    h.intake_row("t-open", "d-1", "dev-b");
    h.advance(60_000);
    record_unload_effect(&path, "t-open", h.now(), "failed");

    assert_eq!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Admitted),
            switch(false)
        ),
        ["t-open"]
    );
    assert_eq!(h.rows("t-open"), vec![("queued".to_owned(), None)]);
}

#[test]
fn given_a_session_with_same_device_rows_only_when_the_sweep_runs_then_it_is_woken_only_with_the_same_device_switch_on()
 {
    let h = WakeHarness::new();
    h.queue_row("t", "d-local");
    assert!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Admitted),
            switch(false)
        )
        .is_empty()
    );
    assert_eq!(
        sweep(
            &h,
            deciding(|_device: &str| IntakeAdmission::Admitted),
            switch(true)
        ),
        ["t"]
    );
}
