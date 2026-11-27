//! The process's thread wake: the carrier and the delivery consumer beside it (omo#13037 A3.1.3).
//!
//! Upstream counterpart: `thread/wake/runtime.ts`. It starts at most once per process and agent directory, on the
//! first session start of a `tui` or `rpc` engine, never in a task child or under CI, and only on an engine at the wake
//! minimum (A8.1). The consumer is started again after every death (#13457), warning once per change of failure; a
//! serving consumer answers a woken drain's admission at once (#14335); a wake that settled a closed session's rows
//! reports the drained marks they leave (#14273, #14383).
//!
//! Not ported here: the production carrier connector (`carrier-connection.ts`), the engine host ensure
//! (`host-ensure.ts`), the device service's delivery port (`consumer/device-service-port.ts`) and the same-device wake
//! of the local transport (`gateway/same-device-wake.ts`). A host passes them as seams; the native thread component
//! does not start the wake yet.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use omo_config_core::ResolvedSessionWakeSettings;

use super::carrier::{
    AdmitIntake, CancelScheduled, Carrier, CarrierError, CarrierErrorListener, CarrierEvent,
    CarrierEventListener, CarrierOptions, CarrierWakeOutcome, CwdSwitch, EnsureHost, MemoryFloor,
    MemoryProbe, Schedule, ScheduledRun, Switch, WakeLimit, create_carrier,
};
use super::carrier_connection::CarrierConnect;
use super::consumer_start::{
    ConsumerStartInput, Deliveries, Log, Notify, OnConsumer, ProjectSwitch, ServingConsumer,
    admission_of, start_delivery_consumer,
};
use super::engine_minimum::{MIN_WAKE_ENGINE_VERSION, engine_drops_retained_record};
use super::memory_headroom::{read_default_host_memory_headroom, read_default_wake_memory_floor};
use super::pending_sweep::WakeSession;
use crate::thread::consumer::delivery_consumer::{
    ChainCountLimit, CreateDeliveryConsumer, DeliveryConsumer,
};
use crate::thread::consumer::port::DeliveryServicePort;
use crate::thread::intake_admission::IntakeAdmission;
use crate::thread::store::{GatewayStoreOptions, SqliteGatewayStore, create_gateway_store};

/// How long a dead delivery consumer waits before it is started again (omo#13457).
pub const CONSUMER_RETRY_MS: u64 = 20_000;
pub const OMO_SENPI_TASK_RPC_CHILD: &str = "OMO_SENPI_TASK_RPC_CHILD";

/// `session_wake` as the machine's omo configuration says it now, for a session's cwd; `None` reads the caller's
/// workspace root.
pub type WakeSettings = Arc<dyn Fn(Option<&str>) -> ResolvedSessionWakeSettings + Send + Sync>;
pub type OnWake = Arc<dyn Fn(&str, &CarrierWakeOutcome) + Send + Sync>;
pub type ThreadWakeLatch = Mutex<HashMap<PathBuf, Arc<RunningThreadWake>>>;

/// Every field but `connect` is a seam a production start leaves empty.
pub struct ThreadWakeOptions {
    pub env: BTreeMap<String, String>,
    pub latch: Option<Arc<ThreadWakeLatch>>,
    /// The store the carrier and the consumer share; absent, the running wake opens its own under the agent dir.
    pub store: Option<Arc<SqliteGatewayStore>>,
    pub connect: CarrierConnect,
    pub ensure_host: Option<EnsureHost>,
    pub deliveries: Option<Deliveries>,
    pub create_consumer: Option<CreateDeliveryConsumer>,
    pub engine_version: Option<String>,
    pub minimum_engine: Option<String>,
    pub chain_count_limit: Option<ChainCountLimit>,
    pub memory_headroom: Option<MemoryProbe>,
    pub schedule: Option<Schedule>,
    pub on_wake: Option<OnWake>,
}

pub struct ThreadWakeStart {
    pub options: ThreadWakeOptions,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub settings: WakeSettings,
    pub engine_version: Option<String>,
    pub warn: Log,
}

#[derive(Default)]
struct ConsumerSlot {
    serving: Option<Arc<ServingConsumer>>,
    consumer: Option<Arc<dyn DeliveryConsumer>>,
    /// The port the last consumer attempt runs on: while it is live the carrier's admission ask is answered on it.
    live_port: Option<Arc<dyn DeliveryServicePort>>,
    failure: Option<String>,
    retry: Option<CancelScheduled>,
    stopped: bool,
    starting: bool,
    ready: bool,
}

impl ConsumerSlot {
    fn settling(&self) -> bool {
        if self.stopped {
            return false;
        }
        self.starting || !self.ready
    }
}

#[derive(Default)]
struct ConsumerShared {
    slot: Mutex<ConsumerSlot>,
    settled: Condvar,
}

impl ConsumerShared {
    fn lock(&self) -> MutexGuard<'_, ConsumerSlot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn carrier_admission(&self, device: &str) -> IntakeAdmission {
        let port = self.lock().live_port.clone();
        port.map_or(IntakeAdmission::Unknown, |port| {
            admission_of(&*port, device)
        })
    }

    fn serving_now(&self) -> bool {
        let consumer = self.lock().consumer.clone();
        consumer.is_some_and(|consumer| consumer.serving())
    }

    /// Tells the service which intake marks are drained now; a report that fails is retried at the next drain.
    fn report_drained(&self, warn: &Log) {
        let serving = self.lock().serving.clone();
        if let Some(serving) = serving
            && let Err(error) = serving.consumer.report_drained()
        {
            warn(&format!("thread wake: drained marks not reported: {error}"));
        }
    }

    /// One warning per change of state: a repeating failure is silent, a new one warns again.
    fn report_failure(&self, warn: &Log, line: &str) {
        {
            let mut slot = self.lock();
            if slot.failure.as_deref() == Some(line) {
                return;
            }
            slot.failure = Some(line.to_owned());
        }
        warn(line);
    }
}

struct RuntimeInner {
    carrier: Carrier,
    shared: Arc<ConsumerShared>,
    store: Arc<SqliteGatewayStore>,
    agent_dir: String,
    sessions_directory: PathBuf,
    settings: WakeSettings,
    warn: Log,
    deliveries: Option<Deliveries>,
    create_consumer: Option<CreateDeliveryConsumer>,
    engine_version: Option<String>,
    minimum_engine: Option<String>,
    chain_count_limit: ChainCountLimit,
    schedule: Schedule,
    on_wake: Option<OnWake>,
}

pub struct RunningThreadWake {
    inner: Arc<RuntimeInner>,
}

fn process_latch() -> &'static ThreadWakeLatch {
    static LATCH: OnceLock<ThreadWakeLatch> = OnceLock::new();
    LATCH.get_or_init(ThreadWakeLatch::default)
}

fn latch_lock(latch: &ThreadWakeLatch) -> MutexGuard<'_, HashMap<PathBuf, Arc<RunningThreadWake>>> {
    latch.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_ci(env: &BTreeMap<String, String>) -> bool {
    match env.get("CI").map(|value| value.trim().to_ascii_lowercase()) {
        None => false,
        Some(value) => !matches!(value.as_str(), "" | "0" | "false"),
    }
}

fn no_host_ensure() -> EnsureHost {
    Arc::new(|| -> Result<(), String> { Ok(()) })
}

fn no_chain_limit() -> ChainCountLimit {
    Arc::new(|_cwd: &str| -> Option<u64> { None })
}

fn default_memory_headroom() -> MemoryProbe {
    Arc::new(read_default_host_memory_headroom)
}

fn default_memory_floor() -> MemoryFloor {
    Arc::new(read_default_wake_memory_floor)
}

/// The production schedule: `run` on its own thread after `delay_ms`, unless cancelled first. The thread never keeps
/// the process alive.
pub fn thread_schedule() -> Schedule {
    Arc::new(|run: ScheduledRun, delay_ms: u64| -> CancelScheduled {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let _ = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            if !flag.load(Ordering::SeqCst) {
                run();
            }
        });
        Box::new(move || cancelled.store(true, Ordering::SeqCst))
    })
}

/// `session_wake` read from the machine's omo configuration at each call, for the session's cwd when one is named and
/// the caller's workspace root otherwise.
pub fn wake_settings_reader(env: BTreeMap<String, String>, cwd: PathBuf) -> WakeSettings {
    Arc::new(move |at: Option<&str>| {
        let cwd = at.map_or_else(|| cwd.to_string_lossy().into_owned(), str::to_owned);
        let loaded = maho_omo_config_resolution::load_senpi_omo_config(
            omo_config_core::LoadOmoConfigOptions {
                cwd: Some(cwd),
                env: Some(env.clone()),
                ..Default::default()
            },
        );
        omo_config_core::resolve_session_wake_settings(&loaded.config)
    })
}

/// Starts the process's carrier for this agent directory on the first session start of a `tui` or `rpc` engine, never
/// in a task child or under CI, and at most once per process. Below the wake minimum no carrier starts, so a send to a
/// session that is not running answers as it did before the wake existed. Answers the running wake of the agent
/// directory.
pub fn start_thread_wake_at_session(
    mode: &str,
    input: ThreadWakeStart,
) -> Option<Arc<RunningThreadWake>> {
    if mode != "tui" && mode != "rpc" {
        return None;
    }
    let env = &input.options.env;
    if is_ci(env) || env.get(OMO_SENPI_TASK_RPC_CHILD).map(String::as_str) == Some("1") {
        return None;
    }
    if !input.agent_dir.is_absolute() {
        return None;
    }
    let version = input
        .options
        .engine_version
        .as_deref()
        .or(input.engine_version.as_deref());
    let minimum = input
        .options
        .minimum_engine
        .as_deref()
        .or(MIN_WAKE_ENGINE_VERSION);
    if !engine_drops_retained_record(version, minimum) {
        return None;
    }
    let held = input.options.latch.clone();
    let latch: &ThreadWakeLatch = match &held {
        Some(latch) => latch,
        None => process_latch(),
    };
    let mut running = latch_lock(latch);
    if let Some(existing) = running.get(&input.agent_dir) {
        return Some(existing.clone());
    }
    let agent_dir = input.agent_dir.clone();
    let started = start_running(input)?;
    running.insert(agent_dir, started.clone());
    Some(started)
}

/// The receiving drain's admission check (5.6), answered by this process's serving consumer.
pub fn process_intake_admission(
    latch: Option<&ThreadWakeLatch>,
    agent_dir: &std::path::Path,
    device: &str,
) -> IntakeAdmission {
    let latch = match latch {
        Some(latch) => latch,
        None => process_latch(),
    };
    let running = latch_lock(latch).get(agent_dir).cloned();
    running.map_or(IntakeAdmission::Unknown, |running| {
        running.admit_intake(device)
    })
}

pub struct ProcessSameDeviceWakeInput {
    pub latch: Option<Arc<ThreadWakeLatch>>,
    pub store: Arc<SqliteGatewayStore>,
    pub agent_dir: PathBuf,
    pub sessions_directory: PathBuf,
    pub settings: WakeSettings,
}

/// The local transport's same-device wake: the process's running carrier takes the wake when present.
pub fn process_same_device_wake(
    input: ProcessSameDeviceWakeInput,
) -> crate::thread::same_device_wake::SameDeviceWake {
    let latch = input.latch;
    let agent_dir = input.agent_dir;
    let settings = input.settings;
    let enabled: CwdSwitch = Arc::new(move |cwd: &str| settings(Some(cwd)).same_device);
    let carrier_lookup: crate::thread::same_device_wake::SameDeviceCarrierLookup =
        Arc::new(move || {
            let running = match &latch {
                Some(latch) => latch_lock(latch).get(&agent_dir).cloned(),
                None => latch_lock(process_latch()).get(&agent_dir).cloned(),
            }?;
            let wake: crate::thread::same_device_wake::SameDeviceCarrier =
                Arc::new(move |durable_id: &str| {
                    let running = running.clone();
                    let durable_id = durable_id.to_owned();
                    std::thread::spawn(move || {
                        let _ = running.wake(&durable_id);
                    });
                    Ok(())
                });
            Some(wake)
        });
    crate::thread::same_device_wake::SameDeviceWake::with_carrier_lookup(
        input.store,
        input.sessions_directory,
        enabled,
        carrier_lookup,
    )
}

fn start_running(input: ThreadWakeStart) -> Option<Arc<RunningThreadWake>> {
    let ThreadWakeStart {
        options,
        agent_dir,
        sessions_directory,
        settings,
        engine_version,
        warn,
    } = input;
    let store = match options.store.clone() {
        Some(store) => store,
        None => match create_gateway_store(GatewayStoreOptions::new(
            agent_dir.to_string_lossy().into_owned(),
        )) {
            Ok(store) => store,
            Err(error) => {
                warn(&format!(
                    "thread wake: the gateway store did not open: {}",
                    error.message()
                ));
                return None;
            }
        },
    };
    let shared = Arc::new(ConsumerShared::default());
    let schedule = options.schedule.clone().unwrap_or_else(thread_schedule);
    let wake_limit: WakeLimit = {
        let settings = settings.clone();
        Arc::new(move || i64::try_from(settings(None).wake_limit).unwrap_or(i64::MAX))
    };
    let same_device: CwdSwitch = {
        let settings = settings.clone();
        Arc::new(move |cwd: &str| settings(Some(cwd)).same_device)
    };
    // Owner ruling 2026-11-14: a device whose service serves this engine's consumer wakes; no switch has to be told on.
    let remote_wake: Switch = {
        let shared = shared.clone();
        Arc::new(move || shared.serving_now())
    };
    let admit_intake: AdmitIntake = {
        let shared = shared.clone();
        Arc::new(move |device: &str| shared.carrier_admission(device))
    };
    let on_event: CarrierEventListener = {
        let shared = shared.clone();
        let warn = warn.clone();
        Arc::new(move |event: &CarrierEvent| {
            if event.outcome.ends_carry() {
                shared.report_drained(&warn);
            }
        })
    };
    let on_error: CarrierErrorListener = {
        let warn = warn.clone();
        Arc::new(move |error: &CarrierError| {
            warn(&format!(
                "thread wake: the carrier's sweep failed and retries at its next tick: {error}"
            ));
        })
    };
    let carrier = create_carrier(CarrierOptions {
        store: store.clone(),
        sessions_directory: sessions_directory.clone(),
        ensure_host: options.ensure_host.clone().unwrap_or_else(no_host_ensure),
        connect: options.connect.clone(),
        wake_limit: Some(wake_limit),
        same_device: Some(same_device),
        remote_wake: Some(remote_wake),
        admit_intake: Some(admit_intake),
        memory_headroom: Some(
            options
                .memory_headroom
                .clone()
                .unwrap_or_else(default_memory_headroom),
        ),
        memory_floor: Some(default_memory_floor()),
        schedule: schedule.clone(),
        on_event: Some(on_event),
        on_error: Some(on_error),
    });
    let inner = Arc::new(RuntimeInner {
        carrier,
        shared,
        store,
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        sessions_directory,
        settings,
        warn,
        deliveries: options.deliveries.clone(),
        create_consumer: options.create_consumer.clone(),
        engine_version: options.engine_version.clone().or(engine_version),
        minimum_engine: options.minimum_engine.clone(),
        chain_count_limit: options
            .chain_count_limit
            .clone()
            .unwrap_or_else(no_chain_limit),
        schedule,
        on_wake: options.on_wake.clone(),
    });
    inner.schedule_consumer_start(0);
    Some(Arc::new(RunningThreadWake { inner }))
}

impl RuntimeInner {
    fn wake(&self, durable_id: &str) -> Result<(), CarrierError> {
        // Every caller (a send, the consumer, the pending sweep) answers its sender without waiting on this wake, so the
        // failure is reported here; the requested row stays and the carrier's sweep tries it again.
        let outcome = self.carrier.wake(durable_id).inspect_err(|error| {
            (self.warn)(&format!(
                "thread wake: {durable_id} was not woken and is retried by the sweep: {error}"
            ));
        })?;
        // The rows a close settled during this wake leave drained marks no carry ending will report (omo#14273
        // follow-up, #14383).
        if outcome == CarrierWakeOutcome::NothingQueued {
            self.shared.report_drained(&self.warn);
        }
        if let CarrierWakeOutcome::Failed { attempts } = outcome {
            (self.warn)(&format!(
                "thread wake: {durable_id} was not started after {attempts} attempts"
            ));
        }
        if let Some(on_wake) = &self.on_wake {
            on_wake(durable_id, &outcome);
        }
        Ok(())
    }

    fn admit_intake(&self, device: &str) -> IntakeAdmission {
        // A consumer the service has acknowledged answers on its port at once (omo#14335): its start is not over until
        // the startup pending sweep has woken every parked session, and a woken session's drain asks this question,
        // so waiting for the start would wait on itself.
        if self.shared.serving_now() {
            return self.shared.carrier_admission(device);
        }
        let serving = {
            let mut slot = self.shared.lock();
            while slot.settling() {
                slot = self
                    .shared
                    .settled
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            slot.serving.clone()
        };
        serving.map_or(IntakeAdmission::Unknown, |serving| {
            serving.admit_intake(device)
        })
    }

    fn schedule_consumer_start(self: &Arc<Self>, delay_ms: u64) {
        let previous = {
            let mut slot = self.shared.lock();
            if slot.stopped {
                return;
            }
            slot.retry.take()
        };
        if let Some(previous) = previous {
            previous();
        }
        let weak = Arc::downgrade(self);
        let cancel = (self.schedule)(
            Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.shared.lock().retry = None;
                    inner.start_consumer();
                }
            }),
            delay_ms,
        );
        self.shared.lock().retry = Some(cancel);
    }

    fn start_consumer(self: &Arc<Self>) {
        {
            let mut slot = self.shared.lock();
            if slot.stopped {
                slot.ready = true;
                drop(slot);
                self.shared.settled.notify_all();
                return;
            }
            slot.starting = true;
        }
        let attempt = match (&self.deliveries, &self.create_consumer) {
            (Some(deliveries), Some(create_consumer)) => {
                start_delivery_consumer(self.consumer_input(deliveries, create_consumer))
            }
            _ => Ok(None),
        };
        let attempt = match attempt {
            Ok(attempt) => attempt.map(Arc::new),
            Err(error) => {
                // A rejected start is a death like the others: reported once and retried.
                self.shared.report_failure(
                    &self.warn,
                    &format!("thread wake: the delivery consumer did not start: {error}"),
                );
                self.schedule_consumer_start(CONSUMER_RETRY_MS);
                None
            }
        };
        {
            let mut slot = self.shared.lock();
            slot.consumer = attempt.as_ref().map(|serving| serving.consumer.clone());
            if attempt.is_some() {
                slot.failure = None;
            }
            slot.serving = attempt;
            slot.starting = false;
            slot.ready = true;
        }
        self.shared.settled.notify_all();
    }

    fn consumer_input(
        self: &Arc<Self>,
        deliveries: &Deliveries,
        create_consumer: &CreateDeliveryConsumer,
    ) -> ConsumerStartInput {
        let live: Deliveries = {
            let shared = self.shared.clone();
            let deliveries = deliveries.clone();
            Arc::new(
                move || -> Result<Option<Arc<dyn DeliveryServicePort>>, String> {
                    let port = deliveries()?;
                    shared.lock().live_port = port.clone();
                    Ok(port)
                },
            )
        };
        let on_consumer: OnConsumer = {
            let shared = self.shared.clone();
            Arc::new(move |consumer: Arc<dyn DeliveryConsumer>| {
                shared.lock().consumer = Some(consumer);
            })
        };
        let on_died: Notify = {
            let weak = Arc::downgrade(self);
            Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.schedule_consumer_start(CONSUMER_RETRY_MS);
                }
            })
        };
        let on_closed: Notify = {
            let shared = self.shared.clone();
            Arc::new(move || {
                shared.lock().serving = None;
            })
        };
        let wake: WakeSession = {
            let weak = Arc::downgrade(self);
            Arc::new(move |durable_id: &str| -> Result<(), String> {
                match weak.upgrade() {
                    Some(inner) => inner.wake(durable_id).map_err(|error| error.to_string()),
                    None => Ok(()),
                }
            })
        };
        let log: Log = {
            let shared = self.shared.clone();
            let warn = self.warn.clone();
            Arc::new(move |line: &str| shared.report_failure(&warn, line))
        };
        let same_device: ProjectSwitch = {
            let settings = self.settings.clone();
            Arc::new(move |cwd: Option<&str>| settings(cwd).same_device)
        };
        ConsumerStartInput {
            store: self.store.clone(),
            agent_dir: self.agent_dir.clone(),
            sessions_directory: self.sessions_directory.clone(),
            deliveries: Some(live),
            create_consumer: create_consumer.clone(),
            engine_version: self.engine_version.clone(),
            minimum_engine: self.minimum_engine.clone(),
            wake,
            chain_count_limit: self.chain_count_limit.clone(),
            same_device,
            on_died: Some(on_died),
            log,
            on_closed: Some(on_closed),
            on_consumer: Some(on_consumer),
        }
    }

    fn stop(&self) {
        let retry = {
            let mut slot = self.shared.lock();
            slot.stopped = true;
            slot.retry.take()
        };
        if let Some(cancel) = retry {
            cancel();
        }
        // An in-flight start settles `serving` only after this wait, so its consumer is closed too.
        let serving = {
            let mut slot = self.shared.lock();
            while slot.starting {
                slot = self
                    .shared
                    .settled
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            slot.serving.take()
        };
        self.shared.settled.notify_all();
        if let Some(serving) = serving {
            serving.close();
        }
        self.carrier.stop();
    }
}

impl RunningThreadWake {
    pub fn wake(&self, durable_id: &str) -> Result<(), CarrierError> {
        self.inner.wake(durable_id)
    }

    /// Asked by a woken session's drain about a row from another device; `unknown` while no consumer serves.
    pub fn admit_intake(&self, device: &str) -> IntakeAdmission {
        self.inner.admit_intake(device)
    }

    /// Returns once the consumer's first start has settled, or the wake stopped.
    pub fn wait_ready(&self) {
        let mut slot = self.inner.shared.lock();
        while slot.settling() {
            slot = self
                .inner
                .shared
                .settled
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn stop(&self) {
        self.inner.stop();
    }
}
