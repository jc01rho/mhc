//! `lifecycle/host-session-end.ts`, `host-session-end-ownership.test.ts` and
//! `host-session-cancel-driver.ts` (omo#11765, omo#11783).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::*;
use crate::lifecycle::host_session_cancel_driver::drive_host_session_cancel;
use crate::lifecycle::{
    HostGenerationView, HostSessionCancelSink, HostSessionCloseError, HostSessionCloseRequest,
    HostSessionCloser, HostSessionEnd, HostSessionEndVia, HostSessionLiveness, HostSessionProbe,
    HostSessionRetryPolicy, LifecycleContext, LifecycleDeps, clear_host_session_end,
    default_host_session_retry_policy, end_host_session_child, resolve_context,
};
use crate::state::{HostSessionIdentity, ResidencyState::*, RunnerKind, TaskStatus::*};

const WAIT: Duration = Duration::from_secs(10);

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Probe(Mutex<HostSessionLiveness>);

impl HostSessionProbe for Probe {
    fn daemon_alive(&self, _host_session: &HostSessionIdentity) -> bool {
        true
    }
    fn session_live(&self, host_session: &HostSessionIdentity) -> bool {
        self.session_liveness(host_session) == HostSessionLiveness::Live
    }
    fn session_liveness(&self, _host_session: &HostSessionIdentity) -> HostSessionLiveness {
        *lock(&self.0)
    }
    fn refresh(&self, _socket: Option<&str>) {}
}

struct View {
    generation: bool,
    parked: bool,
}

impl HostGenerationView for View {
    fn generation_present(&self, _host_session: &HostSessionIdentity) -> bool {
        self.generation
    }
    fn parked_entry_present(&self, _host_session: &HostSessionIdentity) -> bool {
        self.parked
    }
}

/// Answers each close from the script in order (`true` confirms and ends the listing, `false`
/// refuses); past the script every close refuses.
struct Closes {
    script: Mutex<Vec<bool>>,
    opens: Mutex<usize>,
}

struct Fixture {
    temp: TempStore,
    context: LifecycleContext,
    closes: Arc<Closes>,
}

fn identity() -> HostSessionIdentity {
    HostSessionIdentity {
        socket: "/tmp/host.sock".to_string(),
        routing_id: "rpc-1".to_string(),
        session_path: "/tmp/host-session-end.jsonl".to_string(),
        instance_id: "inst-1".to_string(),
        daemon_pid: Some(4242),
        effect_seq: None,
        adopted_entry_id: None,
    }
}

fn fixture(
    liveness: HostSessionLiveness,
    view: Option<View>,
    script: &[bool],
    edit: impl FnOnce(&mut LifecycleDeps),
) -> Fixture {
    let temp = temp_store();
    let probe = Arc::new(Probe(Mutex::new(liveness)));
    let closes = Arc::new(Closes {
        script: Mutex::new(script.iter().rev().copied().collect()),
        opens: Mutex::new(0),
    });
    let closer: HostSessionCloser = {
        let closes = Arc::clone(&closes);
        let probe = Arc::clone(&probe);
        Arc::new(move |_request: &HostSessionCloseRequest| {
            *lock(&closes.opens) += 1;
            if lock(&closes.script).pop().unwrap_or(false) {
                *lock(&probe.0) = HostSessionLiveness::Gone;
                Ok(())
            } else {
                Err(HostSessionCloseError {
                    message: "refused".to_string(),
                })
            }
        })
    };
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.host_session_probe = Some(probe);
    deps.host_session_close = Some(closer);
    deps.host_generation_view = view.map(|view| Arc::new(view) as Arc<dyn HostGenerationView>);
    edit(&mut deps);
    Fixture {
        temp,
        context: resolve_context(deps),
        closes,
    }
}

fn seed(fixture: &Fixture, task_id: &str) -> crate::state::TaskRecord {
    seed_record(
        &fixture.temp.store,
        Seed {
            task_id,
            status: Some(Running),
            residency_state: Some(RpcDetached),
            execution_mode: Some("process"),
            runner_kind: Some(RunnerKind::HostSession),
            host_session: Some(identity()),
            run_epoch: Some(1),
            ..Seed::default()
        },
    )
}

fn opens(fixture: &Fixture) -> usize {
    *lock(&fixture.closes.opens)
}

#[test]
fn a_confirmed_close_is_the_end_and_answers_again_until_the_destruction_clears_it() {
    let fixture = fixture(HostSessionLiveness::Live, None, &[true], |_| {});
    let record = seed(&fixture, "st_0000e001");

    let first = end_host_session_child(&fixture.context, &record);
    let again = end_host_session_child(&fixture.context, &record);
    clear_host_session_end(&fixture.context, &record.task_id);
    let after_clear = end_host_session_child(&fixture.context, &record);

    assert_eq!(
        first,
        HostSessionEnd::Ended(HostSessionEndVia::CloseSession)
    );
    assert_eq!(again, first);
    assert_eq!(
        after_clear,
        HostSessionEnd::Ended(HostSessionEndVia::NotListed)
    );
    assert_eq!(opens(&fixture), 1);
}

#[test]
fn a_refused_close_is_not_an_end_and_the_next_pass_asks_again() {
    let fixture = fixture(HostSessionLiveness::Live, None, &[false, true], |_| {});
    let record = seed(&fixture, "st_0000e002");

    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::NotEnded("close_refused")
    );
    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::Ended(HostSessionEndVia::CloseSession)
    );
    assert_eq!(opens(&fixture), 2);
}

#[test]
fn not_listed_is_not_an_end_while_the_records_generation_still_runs() {
    let fixture = fixture(
        HostSessionLiveness::Gone,
        Some(View {
            generation: true,
            parked: false,
        }),
        &[],
        |_| {},
    );
    let record = seed(&fixture, "st_0000e003");

    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::NotEnded("generation_cannot_cancel")
    );
    assert_eq!(opens(&fixture), 0);
}

#[test]
fn a_session_parked_for_the_successor_is_closed_there() {
    let fixture = fixture(
        HostSessionLiveness::Gone,
        Some(View {
            generation: false,
            parked: true,
        }),
        &[true],
        |_| {},
    );
    let record = seed(&fixture, "st_0000e004");

    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::Ended(HostSessionEndVia::CloseSession)
    );
    assert_eq!(opens(&fixture), 1);
}

#[test]
fn a_gone_generation_with_no_parked_entry_proves_the_end_and_records_it() {
    let fixture = fixture(
        HostSessionLiveness::Gone,
        Some(View {
            generation: false,
            parked: false,
        }),
        &[],
        |_| {},
    );
    let record = seed(&fixture, "st_0000e005");

    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::Ended(HostSessionEndVia::GenerationExited)
    );
    assert_eq!(opens(&fixture), 0);
    assert!(
        read_events(&fixture.temp.store, &record.task_id)
            .iter()
            .any(|event| event == "host_session_closed")
    );
}

/// A closer that holds every close until the test releases it, announcing each one it holds.
fn held_closer(
    entered: Sender<()>,
    release: Receiver<()>,
    opens: Arc<Mutex<usize>>,
) -> HostSessionCloser {
    let entered = Mutex::new(entered);
    let release = Mutex::new(release);
    Arc::new(
        move |_request: &HostSessionCloseRequest| -> Result<(), HostSessionCloseError> {
            *lock(&opens) += 1;
            lock(&entered)
                .send(())
                .expect("the test watches every close");
            lock(&release)
                .recv_timeout(WAIT)
                .expect("the test releases the held close");
            Ok(())
        },
    )
}

#[test]
fn an_earlier_runs_end_in_flight_is_never_joined_or_doubled_by_a_later_run() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let held_opens = Arc::new(Mutex::new(0));
    let closer = held_closer(entered_tx, release_rx, Arc::clone(&held_opens));
    let fixture = fixture(HostSessionLiveness::Live, None, &[], |deps| {
        deps.host_session_close = Some(closer);
    });
    let run_n = seed(&fixture, "st_0000e006");
    let mut run_n1 = run_n.clone();
    run_n1.notification.run_epoch += 1;
    let context = fixture.context.clone();
    let held = run_n.clone();
    let first = std::thread::spawn(move || end_host_session_child(&context, &held));
    entered.recv_timeout(WAIT).expect("run N's close started");

    let later = end_host_session_child(&fixture.context, &run_n1);
    release.send(()).expect("release run N's close");

    assert_eq!(later, HostSessionEnd::NotEnded("earlier_run_in_flight"));
    assert_eq!(
        first.join().expect("run N's pass"),
        HostSessionEnd::Ended(HostSessionEndVia::CloseSession)
    );
    assert_eq!(*lock(&held_opens), 1);
}

#[test]
fn every_entrant_of_one_run_gets_the_pass_in_flight() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let held_opens = Arc::new(Mutex::new(0));
    let closer = held_closer(entered_tx, release_rx, Arc::clone(&held_opens));
    let fixture = fixture(HostSessionLiveness::Live, None, &[], |deps| {
        deps.host_session_close = Some(closer);
    });
    let record = seed(&fixture, "st_0000e007");
    let first_context = fixture.context.clone();
    let first_record = record.clone();
    let first = std::thread::spawn(move || end_host_session_child(&first_context, &first_record));
    entered
        .recv_timeout(WAIT)
        .expect("the first pass's close started");
    let second_context = fixture.context.clone();
    let second_record = record.clone();
    let second =
        std::thread::spawn(move || end_host_session_child(&second_context, &second_record));

    release.send(()).expect("release the close");

    let ended = HostSessionEnd::Ended(HostSessionEndVia::CloseSession);
    assert_eq!(first.join().expect("first entrant"), ended);
    assert_eq!(second.join().expect("second entrant"), ended);
    assert_eq!(*lock(&held_opens), 1);
}

struct Sink(Mutex<Sender<&'static str>>);

impl HostSessionCancelSink for Sink {
    fn ended(&self) {
        lock(&self.0)
            .send("ended")
            .expect("the test watches the sink");
    }
    fn settle(&self) {
        lock(&self.0)
            .send("settled")
            .expect("the test watches the sink");
    }
}

fn instant_retries(deps: &mut LifecycleDeps) {
    deps.host_retry = Some(HostSessionRetryPolicy {
        deferred_retry_backoff_ms: vec![1, 1, 1],
        ..default_host_session_retry_policy()
    });
    deps.host_retry_wait = Some(Arc::new(|_: u64| {}));
}

fn sink_events(events: &Receiver<&'static str>) -> Vec<&'static str> {
    let mut seen = Vec::new();
    while seen.last() != Some(&"settled") {
        seen.push(events.recv_timeout(WAIT).expect("the driver settles"));
    }
    seen
}

#[test]
fn the_cancel_driver_finishes_the_cancel_on_the_pass_that_confirms_the_end() {
    let fixture = fixture(
        HostSessionLiveness::Live,
        None,
        &[false, false, true],
        instant_retries,
    );
    let record = seed(&fixture, "st_0000e008");
    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::NotEnded("close_refused")
    );
    let (events_tx, events) = mpsc::channel();

    drive_host_session_cancel(
        fixture.context.clone(),
        record,
        Arc::new(Sink(Mutex::new(events_tx))),
    );

    assert_eq!(sink_events(&events), vec!["ended", "settled"]);
    assert_eq!(opens(&fixture), 3);
}

#[test]
fn the_cancel_driver_only_settles_once_the_task_moved_to_another_run() {
    let fixture = fixture(HostSessionLiveness::Live, None, &[false], instant_retries);
    let record = seed(&fixture, "st_0000e009");
    assert_eq!(
        end_host_session_child(&fixture.context, &record),
        HostSessionEnd::NotEnded("close_refused")
    );
    let _ = fixture
        .temp
        .store
        .mutate(&record.task_id, |fresh| {
            let mut moved = fresh.clone();
            moved.notification.run_epoch += 1;
            moved
        })
        .expect("a later run");
    let (events_tx, events) = mpsc::channel();

    drive_host_session_cancel(
        fixture.context.clone(),
        record,
        Arc::new(Sink(Mutex::new(events_tx))),
    );

    assert_eq!(sink_events(&events), vec!["settled"]);
    assert_eq!(opens(&fixture), 1);
}
