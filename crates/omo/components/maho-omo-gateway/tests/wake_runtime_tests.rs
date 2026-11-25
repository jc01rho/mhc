//! The wake runtime and the consumer start (`thread/wake/runtime-start.test.ts`, `consumer-start.test.ts`, omo#13037,
//! #13457, #14273, #14335, #14383), over a real store and the in-process engine host. The consumer and the delivery
//! port are the test's own: the consumer itself (`consumer/consumer.ts`) is not ported.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use maho_omo_gateway::thread::consumer::delivery_consumer::{
    ConsumerStart, CreateDeliveryConsumer, DeliveryConsumer, DeliveryConsumerOptions,
};
use maho_omo_gateway::thread::consumer::port::{
    DeliveryPendingPair, DeliveryServicePort, DrainedReply, HoldsListener, InboundListener,
    PortClosed, PortRefusal, PortUnsubscribe, WakeSwitchListener,
};
use maho_omo_gateway::thread::intake_admission::IntakeAdmission;
use maho_omo_gateway::thread::wake::carrier::{CarrierWakeOutcome, MemoryProbe};
use maho_omo_gateway::thread::wake::consumer_start::{
    ConsumerStartInput, Deliveries, Log, Notify, OnConsumer, ProjectSwitch, start_delivery_consumer,
};
use maho_omo_gateway::thread::wake::pending_sweep::WakeSession;
use maho_omo_gateway::thread::wake::runtime::{
    CONSUMER_RETRY_MS, OMO_SENPI_TASK_RPC_CHILD, OnWake, RunningThreadWake, ThreadWakeLatch,
    ThreadWakeOptions, ThreadWakeStart, WakeSettings, start_thread_wake_at_session,
};
use omo_config_core::ResolvedSessionWakeSettings;
use serde_json::{Value, json};

use crate::wake_support::{
    FakeEngineHost, FakeEngineOptions, ManualSchedule, WakeHarness, record_unload_effect,
    write_session_file,
};

const ENGINE: &str = "2026.12.1";
const TARGET: &str = "dur-target";
const WAIT: Duration = Duration::from_secs(10);

type ClosedCallbacks = Mutex<Vec<PortClosed>>;
type StartAnswer = Arc<dyn Fn() -> Result<ConsumerStart, String> + Send + Sync>;
type Created = Arc<Mutex<Vec<Arc<FakeConsumer>>>>;
type SeenConsumer = Arc<Mutex<Option<Arc<dyn DeliveryConsumer>>>>;
type FreshPort = Arc<Mutex<Option<Arc<FakePort>>>>;

struct FakePort {
    admitted: AtomicBool,
    closed: AtomicBool,
    close_on_admission: AtomicBool,
    callbacks: ClosedCallbacks,
}

impl FakePort {
    fn new(admitted: bool) -> Arc<Self> {
        Arc::new(Self {
            admitted: AtomicBool::new(admitted),
            closed: AtomicBool::new(false),
            close_on_admission: AtomicBool::new(false),
            callbacks: Mutex::default(),
        })
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl DeliveryServicePort for FakePort {
    fn serve(&self, _agent_dir: &str, _version: i64) -> Result<Value, PortRefusal> {
        Ok(json!({}))
    }

    fn answer(
        &self,
        _sender_device: &str,
        _command_id: &str,
        _reply: &Value,
    ) -> Result<(), PortRefusal> {
        Ok(())
    }

    fn held(&self, _ask_id: &str, _holds: bool) -> Result<(), PortRefusal> {
        Ok(())
    }

    fn peer_admitted(&self, _device: &str) -> Result<bool, PortRefusal> {
        if self.close_on_admission.load(Ordering::SeqCst) {
            self.close();
        }
        Ok(self.admitted.load(Ordering::SeqCst))
    }

    fn pending(&self, _pairs: &[DeliveryPendingPair]) -> Result<(), PortRefusal> {
        Ok(())
    }

    fn drained(&self, _pair: &DeliveryPendingPair) -> Result<DrainedReply, PortRefusal> {
        Ok(DrainedReply::Released)
    }

    fn on_inbound(&self, _listener: InboundListener) -> PortUnsubscribe {
        Box::new(|| {})
    }

    fn on_holds(&self, _listener: HoldsListener) -> PortUnsubscribe {
        Box::new(|| {})
    }

    fn on_wake(&self, _listener: WakeSwitchListener) -> PortUnsubscribe {
        Box::new(|| {})
    }

    fn on_closed(&self, callback: PortClosed) {
        if self.is_closed() {
            callback();
            return;
        }
        self.callbacks.lock().expect("callbacks").push(callback);
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let callbacks = std::mem::take(&mut *self.callbacks.lock().expect("callbacks"));
        for callback in callbacks {
            callback();
        }
    }
}

struct FakeConsumer {
    answer: Result<ConsumerStart, String>,
    serving: AtomicBool,
    stopped: AtomicBool,
    drained_reports: AtomicUsize,
}

impl DeliveryConsumer for FakeConsumer {
    fn start(&self) -> Result<ConsumerStart, String> {
        if self.answer == Ok(ConsumerStart::Serving) {
            self.serving.store(true, Ordering::SeqCst);
        }
        self.answer.clone()
    }

    fn serving(&self) -> bool {
        self.serving.load(Ordering::SeqCst)
    }

    fn report_drained(&self) -> Result<(), String> {
        self.drained_reports.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) {
        self.serving.store(false, Ordering::SeqCst);
        self.stopped.store(true, Ordering::SeqCst);
    }
}

fn serving_answer() -> StartAnswer {
    Arc::new(|| -> Result<ConsumerStart, String> { Ok(ConsumerStart::Serving) })
}

fn refusing(code: &str) -> StartAnswer {
    let line = format!("local API refused: {code}");
    Arc::new(move || -> Result<ConsumerStart, String> { Err(line.clone()) })
}

fn factory(answer: StartAnswer, created: &Created) -> CreateDeliveryConsumer {
    let created = created.clone();
    Arc::new(
        move |_options: DeliveryConsumerOptions| -> Arc<dyn DeliveryConsumer> {
            let consumer = Arc::new(FakeConsumer {
                answer: answer(),
                serving: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                drained_reports: AtomicUsize::new(0),
            });
            created.lock().expect("created").push(consumer.clone());
            consumer
        },
    )
}

fn port_deliveries(port: &Arc<FakePort>) -> Deliveries {
    let port = port.clone();
    Arc::new(
        move || -> Result<Option<Arc<dyn DeliveryServicePort>>, String> {
            let port: Arc<dyn DeliveryServicePort> = port.clone();
            Ok(Some(port))
        },
    )
}

fn counting(count: &Arc<AtomicUsize>) -> Notify {
    let count = count.clone();
    Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    })
}

fn consumer_input(
    h: &WakeHarness,
    deliveries: Deliveries,
    create: CreateDeliveryConsumer,
) -> ConsumerStartInput {
    let wake: WakeSession = Arc::new(|_durable_id: &str| -> Result<(), String> { Ok(()) });
    let same_device: ProjectSwitch = Arc::new(|_cwd: Option<&str>| false);
    let log: Log = Arc::new(|_line: &str| {});
    ConsumerStartInput {
        store: h.store.clone(),
        agent_dir: h.dir.path().to_string_lossy().into_owned(),
        sessions_directory: h.sessions(),
        deliveries: Some(deliveries),
        create_consumer: create,
        engine_version: Some(ENGINE.into()),
        minimum_engine: Some(ENGINE.into()),
        wake,
        chain_count_limit: Arc::new(|_cwd: &str| -> Option<u64> { None }),
        same_device,
        on_died: None,
        log,
        on_closed: None,
        on_consumer: None,
    }
}

#[test]
fn given_a_declaration_the_service_refuses_when_the_consumer_starts_then_the_port_is_closed_the_death_is_reported_and_nothing_serves()
 {
    for answer in [refusing("internal_error"), {
        let unsupported: StartAnswer =
            Arc::new(|| -> Result<ConsumerStart, String> { Ok(ConsumerStart::Unsupported) });
        unsupported
    }] {
        let h = WakeHarness::new();
        let port = FakePort::new(true);
        let created: Created = Arc::default();
        let died = Arc::new(AtomicUsize::new(0));
        let mut input = consumer_input(&h, port_deliveries(&port), factory(answer, &created));
        input.on_died = Some(counting(&died));
        assert!(start_delivery_consumer(input).expect("start").is_none());
        assert_eq!(died.load(Ordering::SeqCst), 1);
        assert!(port.is_closed());
        assert!(
            created.lock().expect("created")[0]
                .stopped
                .load(Ordering::SeqCst)
        );
    }
}

#[test]
fn given_the_serving_connection_ends_while_the_startup_pending_sweep_runs_when_the_consumer_finishes_starting_then_no_stale_serving_consumer_is_returned()
 {
    let h = WakeHarness::new();
    write_session_file(&h.sessions(), TARGET, &json!({}), "--tmp-project--");
    h.intake_row(TARGET, "d-early", "device-b");
    let port = FakePort::new(true);
    port.close_on_admission.store(true, Ordering::SeqCst);
    let created: Created = Arc::default();
    let serving = start_delivery_consumer(consumer_input(
        &h,
        port_deliveries(&port),
        factory(serving_answer(), &created),
    ))
    .expect("start");
    assert!(serving.is_none());
    assert!(port.is_closed());
}

#[test]
fn given_a_session_whose_queued_row_waits_from_another_device_when_the_consumer_starts_and_its_pending_sweep_wakes_it_then_the_consumer_already_reads_serving()
 {
    let h = WakeHarness::new();
    write_session_file(&h.sessions(), TARGET, &json!({}), "--tmp-project--");
    h.intake_row(TARGET, "d-early", "device-b");
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let seen: SeenConsumer = Arc::default();
    let order: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut input = consumer_input(
        &h,
        port_deliveries(&port),
        factory(serving_answer(), &created),
    );
    let on_consumer: OnConsumer = {
        let seen = seen.clone();
        Arc::new(move |consumer: Arc<dyn DeliveryConsumer>| {
            *seen.lock().expect("seen") = Some(consumer);
        })
    };
    let wake: WakeSession = {
        let seen = seen.clone();
        let order = order.clone();
        Arc::new(move |durable_id: &str| -> Result<(), String> {
            let serving = seen
                .lock()
                .expect("seen")
                .as_ref()
                .is_some_and(|consumer| consumer.serving());
            let mut order = order.lock().expect("order");
            order.push(if serving { "serving" } else { "off" }.to_owned());
            order.push(format!("wake:{durable_id}"));
            Ok(())
        })
    };
    input.on_consumer = Some(on_consumer);
    input.wake = wake;
    let started = start_delivery_consumer(input)
        .expect("start")
        .expect("a serving consumer");
    assert_eq!(
        order.lock().expect("order").clone(),
        vec!["serving".to_owned(), format!("wake:{TARGET}")]
    );
    started.close();
    assert!(port.is_closed());
}

#[test]
fn given_a_startup_pending_sweep_that_fails_after_the_consumer_was_acknowledged_when_the_start_fails_then_the_consumer_is_stopped_and_reported_dead()
 {
    let h = WakeHarness::new();
    h.queue_row("t-local", "d-local");
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let died = Arc::new(AtomicUsize::new(0));
    let mut input = consumer_input(
        &h,
        port_deliveries(&port),
        factory(serving_answer(), &created),
    );
    let same_device: ProjectSwitch = Arc::new(|_cwd: Option<&str>| true);
    let wake: WakeSession =
        Arc::new(|_durable_id: &str| -> Result<(), String> { Err("the wake failed".to_owned()) });
    input.same_device = same_device;
    input.wake = wake;
    input.on_died = Some(counting(&died));
    let error = start_delivery_consumer(input)
        .err()
        .expect("the start fails");
    assert!(error.contains("the wake failed"), "{error}");
    assert_eq!(died.load(Ordering::SeqCst), 1);
    assert!(
        created.lock().expect("created")[0]
            .stopped
            .load(Ordering::SeqCst)
    );
    assert!(port.is_closed());
}

#[test]
fn given_two_local_sessions_whose_projects_disagree_about_same_device_when_the_consumer_sweeps_their_rows_then_only_the_allowed_one_is_woken()
 {
    let h = WakeHarness::new();
    write_session_file(
        &h.sessions(),
        "t-on",
        &json!({ "cwd": "/work/on" }),
        "--project-on--",
    );
    write_session_file(
        &h.sessions(),
        "t-off",
        &json!({ "cwd": "/work/off" }),
        "--project-off--",
    );
    h.queue_row("t-on", "d-on");
    h.queue_row("t-off", "d-off");
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let woken: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut input = consumer_input(
        &h,
        port_deliveries(&port),
        factory(serving_answer(), &created),
    );
    let same_device: ProjectSwitch = Arc::new(|cwd: Option<&str>| cwd == Some("/work/on"));
    let wake: WakeSession = {
        let woken = woken.clone();
        Arc::new(move |durable_id: &str| -> Result<(), String> {
            woken.lock().expect("woken").push(durable_id.to_owned());
            Ok(())
        })
    };
    input.same_device = same_device;
    input.wake = wake;
    let started = start_delivery_consumer(input)
        .expect("start")
        .expect("a serving consumer");
    assert_eq!(
        woken.lock().expect("woken").clone(),
        vec!["t-on".to_owned()]
    );
    started.close();
}

struct Running {
    h: WakeHarness,
    fake: FakeEngineHost,
    schedule: Arc<ManualSchedule>,
    running: Arc<RunningThreadWake>,
    warnings: Arc<Mutex<Vec<String>>>,
    outcomes: Arc<Mutex<Vec<String>>>,
}

fn settings_on() -> WakeSettings {
    Arc::new(|_cwd: Option<&str>| ResolvedSessionWakeSettings {
        same_device: true,
        wake_limit: 0,
    })
}

fn no_memory_reading() -> MemoryProbe {
    Arc::new(|| -> Option<u64> { None })
}

fn wake_start(
    h: &WakeHarness,
    fake: &FakeEngineHost,
    schedule: &Arc<ManualSchedule>,
    env: BTreeMap<String, String>,
    agent_dir: PathBuf,
    engine: &str,
    latch: &Arc<ThreadWakeLatch>,
) -> ThreadWakeStart {
    ThreadWakeStart {
        options: ThreadWakeOptions {
            env,
            latch: Some(latch.clone()),
            store: Some(h.store.clone()),
            connect: fake.connect(),
            ensure_host: None,
            deliveries: None,
            create_consumer: None,
            engine_version: Some(engine.into()),
            minimum_engine: Some(ENGINE.into()),
            chain_count_limit: None,
            memory_headroom: Some(no_memory_reading()),
            schedule: Some(schedule.schedule()),
            on_wake: None,
        },
        agent_dir,
        sessions_directory: h.sessions(),
        settings: settings_on(),
        engine_version: Some(engine.into()),
        warn: Arc::new(|_line: &str| {}),
    }
}

fn start_runtime(
    h: WakeHarness,
    fake: FakeEngineHost,
    deliveries: Option<Deliveries>,
    create: Option<CreateDeliveryConsumer>,
) -> Running {
    let schedule = ManualSchedule::new();
    let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
    let outcomes: Arc<Mutex<Vec<String>>> = Arc::default();
    let latch: Arc<ThreadWakeLatch> = Arc::default();
    let mut start = wake_start(
        &h,
        &fake,
        &schedule,
        BTreeMap::new(),
        h.dir.path().to_path_buf(),
        ENGINE,
        &latch,
    );
    let warn: Log = {
        let warnings = warnings.clone();
        Arc::new(move |line: &str| warnings.lock().expect("warnings").push(line.to_owned()))
    };
    let on_wake: OnWake = {
        let outcomes = outcomes.clone();
        Arc::new(move |_durable_id: &str, outcome: &CarrierWakeOutcome| {
            outcomes
                .lock()
                .expect("outcomes")
                .push(outcome.kind().to_owned());
        })
    };
    start.warn = warn;
    start.options.on_wake = Some(on_wake);
    start.options.deliveries = deliveries;
    start.options.create_consumer = create;
    let running = start_thread_wake_at_session("tui", start).expect("the wake starts");
    Running {
        h,
        fake,
        schedule,
        running,
        warnings,
        outcomes,
    }
}

fn warning_count(r: &Running) -> usize {
    r.warnings.lock().expect("warnings").len()
}

fn engine_host() -> FakeEngineHost {
    FakeEngineHost::new(FakeEngineOptions::default())
}

#[test]
fn given_an_engine_at_the_wake_minimum_and_an_empty_wake_table_when_the_wake_starts_then_no_sweep_timer_is_armed()
 {
    let r = start_runtime(WakeHarness::new(), engine_host(), None, None);
    assert_eq!(r.schedule.run_immediate(), 2);
    r.running.wait_ready();
    assert!(r.schedule.delays().is_empty());
    r.running.stop();
}

#[test]
fn given_a_mode_ci_a_task_child_a_relative_directory_or_an_old_engine_then_no_wake_starts_and_a_second_start_answers_the_first()
 {
    let h = WakeHarness::new();
    let fake = engine_host();
    let schedule = ManualSchedule::new();
    let latch: Arc<ThreadWakeLatch> = Arc::default();
    let agent_dir = h.dir.path().to_path_buf();
    let env = |key: &str, value: &str| BTreeMap::from([(key.to_owned(), value.to_owned())]);
    let refused = [
        (
            "print",
            wake_start(
                &h,
                &fake,
                &schedule,
                BTreeMap::new(),
                agent_dir.clone(),
                ENGINE,
                &latch,
            ),
        ),
        (
            "tui",
            wake_start(
                &h,
                &fake,
                &schedule,
                env("CI", "true"),
                agent_dir.clone(),
                ENGINE,
                &latch,
            ),
        ),
        (
            "rpc",
            wake_start(
                &h,
                &fake,
                &schedule,
                env(OMO_SENPI_TASK_RPC_CHILD, "1"),
                agent_dir.clone(),
                ENGINE,
                &latch,
            ),
        ),
        (
            "tui",
            wake_start(
                &h,
                &fake,
                &schedule,
                BTreeMap::new(),
                PathBuf::from("relative/agent"),
                ENGINE,
                &latch,
            ),
        ),
        (
            "tui",
            wake_start(
                &h,
                &fake,
                &schedule,
                BTreeMap::new(),
                agent_dir.clone(),
                "2026.1.1",
                &latch,
            ),
        ),
    ];
    for (mode, start) in refused {
        assert!(
            start_thread_wake_at_session(mode, start).is_none(),
            "{mode}"
        );
    }
    assert!(schedule.delays().is_empty());

    let first = start_thread_wake_at_session(
        "rpc",
        wake_start(
            &h,
            &fake,
            &schedule,
            env("CI", "false"),
            agent_dir.clone(),
            ENGINE,
            &latch,
        ),
    )
    .expect("the wake starts");
    let second = start_thread_wake_at_session(
        "tui",
        wake_start(
            &h,
            &fake,
            &schedule,
            BTreeMap::new(),
            agent_dir,
            ENGINE,
            &latch,
        ),
    )
    .expect("the running wake");
    assert!(Arc::ptr_eq(&first, &second));
    first.stop();
}

#[test]
fn given_a_failure_whose_kind_changes_without_a_recovery_when_each_retry_fails_then_each_new_failure_is_warned_once()
 {
    let calls = Arc::new(AtomicUsize::new(0));
    let port = FakePort::new(true);
    let deliveries: Deliveries = {
        let calls = calls.clone();
        let port = port.clone();
        Arc::new(
            move || -> Result<Option<Arc<dyn DeliveryServicePort>>, String> {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err("the seam is broken".to_owned());
                }
                let port: Arc<dyn DeliveryServicePort> = port.clone();
                Ok(Some(port))
            },
        )
    };
    let created: Created = Arc::default();
    let r = start_runtime(
        WakeHarness::new(),
        engine_host(),
        Some(deliveries),
        Some(factory(refusing("internal_error"), &created)),
    );
    r.schedule.run_immediate();
    r.running.wait_ready();
    assert_eq!(warning_count(&r), 1);
    assert_eq!(r.schedule.delays(), vec![CONSUMER_RETRY_MS]);

    r.schedule.run_queued();
    assert_eq!(warning_count(&r), 2);
    assert_eq!(r.schedule.delays(), vec![CONSUMER_RETRY_MS]);
    r.schedule.run_queued();
    assert_eq!(warning_count(&r), 2);
    r.running.stop();
}

#[test]
fn given_a_service_that_stays_refused_then_recovers_and_fails_again_when_the_retries_fire_then_one_warning_is_written_per_stretch_of_failure()
 {
    let healthy = Arc::new(AtomicBool::new(false));
    let good = FakePort::new(true);
    let bad = FakePort::new(true);
    let deliveries: Deliveries = {
        let healthy = healthy.clone();
        let good = good.clone();
        let bad = bad.clone();
        Arc::new(
            move || -> Result<Option<Arc<dyn DeliveryServicePort>>, String> {
                let port: Arc<dyn DeliveryServicePort> = if healthy.load(Ordering::SeqCst) {
                    good.clone()
                } else {
                    bad.clone()
                };
                Ok(Some(port))
            },
        )
    };
    let answer: StartAnswer = {
        let healthy = healthy.clone();
        Arc::new(move || -> Result<ConsumerStart, String> {
            if healthy.load(Ordering::SeqCst) {
                Ok(ConsumerStart::Serving)
            } else {
                Err("local API refused: forbidden".to_owned())
            }
        })
    };
    let created: Created = Arc::default();
    let r = start_runtime(
        WakeHarness::new(),
        engine_host(),
        Some(deliveries),
        Some(factory(answer, &created)),
    );
    r.schedule.run_immediate();
    r.running.wait_ready();
    assert_eq!(warning_count(&r), 1);
    for _ in 0..2 {
        assert_eq!(r.schedule.run_queued(), 1);
        assert_eq!(warning_count(&r), 1);
    }
    healthy.store(true, Ordering::SeqCst);
    assert_eq!(r.schedule.run_queued(), 1);
    assert_eq!(
        r.running.admit_intake("device-b"),
        IntakeAdmission::Admitted
    );
    assert_eq!(warning_count(&r), 1);

    healthy.store(false, Ordering::SeqCst);
    good.close();
    assert_eq!(r.schedule.run_queued(), 1);
    assert_eq!(warning_count(&r), 2);
    r.running.stop();
}

#[test]
fn given_a_retry_that_opens_a_fresh_service_when_stop_runs_before_it_settles_then_the_consumer_that_retry_produces_is_closed()
 {
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let first = FakePort::new(true);
    let fresh: FreshPort = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let deliveries: Deliveries = {
        let first = first.clone();
        let fresh = fresh.clone();
        let calls = calls.clone();
        let entered = Mutex::new(entered_tx);
        Arc::new(
            move || -> Result<Option<Arc<dyn DeliveryServicePort>>, String> {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let port: Arc<dyn DeliveryServicePort> = first.clone();
                    return Ok(Some(port));
                }
                entered
                    .lock()
                    .expect("entered")
                    .send(())
                    .map_err(|error| error.to_string())?;
                release_rx
                    .lock()
                    .expect("release")
                    .recv_timeout(WAIT)
                    .map_err(|error| error.to_string())?;
                let opened = FakePort::new(true);
                *fresh.lock().expect("fresh") = Some(opened.clone());
                let port: Arc<dyn DeliveryServicePort> = opened;
                Ok(Some(port))
            },
        )
    };
    let created: Created = Arc::default();
    let r = start_runtime(
        WakeHarness::new(),
        engine_host(),
        Some(deliveries),
        Some(factory(serving_answer(), &created)),
    );
    r.schedule.run_immediate();
    r.running.wait_ready();
    first.close();
    assert_eq!(r.schedule.delays(), vec![CONSUMER_RETRY_MS]);

    let schedule = r.schedule.clone();
    let retry = std::thread::spawn(move || schedule.run_queued());
    entered_rx
        .recv_timeout(WAIT)
        .expect("the retry opens its port");
    let running = r.running.clone();
    let stopper = std::thread::spawn(move || running.stop());
    release_tx.send(()).expect("release the retry");
    assert_eq!(retry.join().expect("the retry thread"), 1);
    stopper.join().expect("the stopping thread");

    let opened = fresh
        .lock()
        .expect("fresh")
        .clone()
        .expect("the retry's fresh port");
    assert!(opened.is_closed());
}

#[test]
fn given_a_delivery_from_another_device_queued_before_a_close_on_request_when_the_session_is_woken_then_it_answers_nothing_queued_and_its_drained_mark_is_reported()
 {
    let h = WakeHarness::new();
    let file = write_session_file(&h.sessions(), "dur-closed", &json!({}), "--tmp-project--");
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let r = start_runtime(
        h,
        engine_host(),
        Some(port_deliveries(&port)),
        Some(factory(serving_answer(), &created)),
    );
    r.schedule.run_immediate();
    r.running.wait_ready();
    let consumer = created.lock().expect("created")[0].clone();
    assert_eq!(consumer.drained_reports.load(Ordering::SeqCst), 1);

    r.h.intake_row("dur-closed", "d-1", "device-b");
    r.h.advance(60_000);
    record_unload_effect(&file, "dur-closed", r.h.now(), "done");
    r.h.advance(60_000);
    r.running.wake("dur-closed").expect("wake");

    assert_eq!(
        r.outcomes.lock().expect("outcomes").last().cloned(),
        Some("nothing_queued".to_owned())
    );
    assert_eq!(consumer.drained_reports.load(Ordering::SeqCst), 2);
    assert_eq!(
        r.h.rows("dur-closed"),
        vec![("refused".to_owned(), Some("target_closed".to_owned()))]
    );
    r.running.stop();
}

#[test]
fn given_a_parked_session_the_startup_sweep_wakes_when_its_drain_asks_the_admission_then_the_serving_consumer_answers_at_once()
 {
    let h = WakeHarness::new();
    h.file("parked");
    h.intake_row("parked", "d-1", "dev-b");
    let answers: Arc<Mutex<Vec<IntakeAdmission>>> = Arc::default();
    let cell: Arc<OnceLock<Arc<RunningThreadWake>>> = Arc::default();
    let fake = FakeEngineHost::new(FakeEngineOptions {
        wake_pass: Some({
            let cell = cell.clone();
            let answers = answers.clone();
            Arc::new(move |_path: &str| -> Vec<String> {
                if let Some(running) = cell.get() {
                    answers
                        .lock()
                        .expect("answers")
                        .push(running.admit_intake("dev-b"));
                }
                vec!["d-1".to_owned()]
            })
        }),
        ..FakeEngineOptions::default()
    });
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let r = start_runtime(
        h,
        fake,
        Some(port_deliveries(&port)),
        Some(factory(serving_answer(), &created)),
    );
    assert!(cell.set(r.running.clone()).is_ok());
    r.schedule.run_immediate();
    r.running.wait_ready();

    assert_eq!(
        answers.lock().expect("answers").clone(),
        vec![IntakeAdmission::Admitted]
    );
    assert_eq!(r.fake.frames("wake").len(), 1);
    assert_eq!(
        r.outcomes.lock().expect("outcomes").clone(),
        vec!["open".to_owned()]
    );
    r.running.stop();
}

#[test]
fn given_a_local_session_the_startup_sweep_woke_when_its_carry_ends_then_the_drained_marks_are_reported_again()
 {
    let h = WakeHarness::new();
    h.path("s1");
    let port = FakePort::new(true);
    let created: Created = Arc::default();
    let r = start_runtime(
        h,
        engine_host(),
        Some(port_deliveries(&port)),
        Some(factory(serving_answer(), &created)),
    );
    r.schedule.run_immediate();
    r.running.wait_ready();
    let consumer = created.lock().expect("created")[0].clone();
    assert_eq!(
        r.outcomes.lock().expect("outcomes").clone(),
        vec!["open".to_owned()]
    );
    assert_eq!(consumer.drained_reports.load(Ordering::SeqCst), 1);

    assert_eq!(r.schedule.run_queued(), 1);
    assert_eq!(consumer.drained_reports.load(Ordering::SeqCst), 2);
    assert!(r.h.store.woken().list().expect("list").is_empty());
    r.running.stop();
}
