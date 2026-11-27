//! The one delivery consumer of an engine process (omo#13037 design 5.4).
//!
//! Upstream counterpart: `thread/wake/consumer-start.ts`. It declares this agent directory to the device service, then
//! wakes every session whose intake rows wait on an admitted sender (the pending sweep, 5.4 step 6). On an engine below
//! the wake minimum (A8.1) it opens no connection and declares nothing. A start that fails closes its port, reports
//! itself dead and answers `None`: the caller starts it again later, so a device whose service comes up after this
//! engine keeps its remote wake.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::engine_minimum::{MIN_WAKE_ENGINE_VERSION, engine_drops_retained_record};
use super::pending_sweep::{PendingSweepOptions, WakeSession, run_pending_sweep};
use super::session_target::{WakeTarget, resolve_wake_target};
use crate::thread::consumer::delivery_consumer::{
    ChainCountLimit, ConsumerStart, CreateDeliveryConsumer, DeliveryConsumer,
    DeliveryConsumerOptions,
};
use crate::thread::consumer::port::DeliveryServicePort;
use crate::thread::intake_admission::IntakeAdmission;
use crate::thread::store::SqliteGatewayStore;

/// The device service's delivery port on its local API; `Ok(None)` when there is nothing to serve on.
pub type Deliveries =
    Arc<dyn Fn() -> Result<Option<Arc<dyn DeliveryServicePort>>, String> + Send + Sync>;
/// `session_wake.same_device` for a session's own cwd; `None` reads the caller's workspace root.
pub type ProjectSwitch = Arc<dyn Fn(Option<&str>) -> bool + Send + Sync>;
pub type Notify = Arc<dyn Fn() + Send + Sync>;
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;
pub type OnConsumer = Arc<dyn Fn(Arc<dyn DeliveryConsumer>) + Send + Sync>;

pub struct ConsumerStartInput {
    pub store: Arc<SqliteGatewayStore>,
    pub agent_dir: String,
    pub sessions_directory: PathBuf,
    pub deliveries: Option<Deliveries>,
    pub create_consumer: CreateDeliveryConsumer,
    pub engine_version: Option<String>,
    /// `None` reads the shipped minimum (`MIN_WAKE_ENGINE_VERSION`).
    pub minimum_engine: Option<String>,
    pub wake: WakeSession,
    pub chain_count_limit: ChainCountLimit,
    pub same_device: ProjectSwitch,
    /// The consumer had a port and died (its start failed, or its serving connection ended): it may be started again.
    pub on_died: Option<Notify>,
    pub log: Log,
    /// The serving connection ended: the consumer stopped and serves nothing until a new start.
    pub on_closed: Option<Notify>,
    /// The consumer, as soon as it exists: it may wake a session before its start has returned.
    pub on_consumer: Option<OnConsumer>,
}

pub struct ServingConsumer {
    pub consumer: Arc<dyn DeliveryConsumer>,
    port: Arc<dyn DeliveryServicePort>,
}

impl ServingConsumer {
    /// The device service's answer to whether a device is admitted now (5.6), for a woken session's drain.
    pub fn admit_intake(&self, device: &str) -> IntakeAdmission {
        admission_of(&*self.port, device)
    }

    pub fn close(&self) {
        self.consumer.stop();
        self.port.close();
    }
}

pub(crate) fn admission_of(port: &dyn DeliveryServicePort, device: &str) -> IntakeAdmission {
    match port.peer_admitted(device) {
        Ok(true) => IntakeAdmission::Admitted,
        Ok(false) => IntakeAdmission::Refused,
        Err(_) => IntakeAdmission::Unknown,
    }
}

fn notify(callback: &Option<Notify>) {
    if let Some(callback) = callback {
        callback();
    }
}

pub fn start_delivery_consumer(
    input: ConsumerStartInput,
) -> Result<Option<ServingConsumer>, String> {
    let Some(deliveries) = input.deliveries.clone() else {
        return Ok(None);
    };
    let minimum = input.minimum_engine.as_deref().or(MIN_WAKE_ENGINE_VERSION);
    if !engine_drops_retained_record(input.engine_version.as_deref(), minimum) {
        return Ok(None);
    }
    let Some(port) = deliveries()? else {
        return Ok(None);
    };
    let consumer = (input.create_consumer)(DeliveryConsumerOptions {
        store: input.store.clone(),
        port: port.clone(),
        agent_dir: input.agent_dir.clone(),
        sessions_directory: input.sessions_directory.clone(),
        chain_count_limit: input.chain_count_limit.clone(),
        wake: input.wake.clone(),
        on_error: Some({
            let log = input.log.clone();
            Arc::new(move |error: String| log(&format!("thread wake: {error}")))
        }),
    });
    if let Some(on_consumer) = &input.on_consumer {
        on_consumer(consumer.clone());
    }
    let started = match consumer.start() {
        Ok(started) => started,
        Err(error) => {
            consumer.stop();
            port.close();
            notify(&input.on_died);
            (input.log)(&format!(
                "thread wake: the delivery consumer did not start: {error}"
            ));
            return Ok(None);
        }
    };
    if started != ConsumerStart::Serving {
        consumer.stop();
        port.close();
        notify(&input.on_died);
        (input.log)(&format!(
            "thread wake: the device service serves no deliveries to this engine ({})",
            started.as_str()
        ));
        return Ok(None);
    }
    let ended = Arc::new(AtomicBool::new(false));
    let died = Arc::new(AtomicBool::new(false));
    let die: Notify = {
        let died = died.clone();
        let consumer = consumer.clone();
        let on_died = input.on_died.clone();
        Arc::new(move || {
            if died.swap(true, Ordering::SeqCst) {
                return;
            }
            consumer.stop();
            notify(&on_died);
        })
    };
    {
        let ended = ended.clone();
        let die = die.clone();
        let on_closed = input.on_closed.clone();
        port.on_closed(Box::new(move || {
            ended.store(true, Ordering::SeqCst);
            die();
            notify(&on_closed);
        }));
    }
    let admitting = port.clone();
    let sessions = input.sessions_directory.clone();
    let closes = input.sessions_directory.clone();
    let same_device = input.same_device.clone();
    let swept = run_pending_sweep(&PendingSweepOptions {
        store: input.store.clone(),
        admit_intake: Arc::new(move |device: &str| admission_of(&*admitting, device)),
        wake: input.wake.clone(),
        same_device: Arc::new(move |durable_id: &str| {
            match resolve_wake_target(&sessions, durable_id) {
                WakeTarget::Found { cwd, .. } => same_device(Some(cwd.as_str())),
                WakeTarget::Gone | WakeTarget::Ambiguous { .. } => same_device(None),
            }
        }),
        closed_at: Arc::new(move |durable_id: &str| {
            match resolve_wake_target(&closes, durable_id) {
                WakeTarget::Found { closed_at, .. } => closed_at,
                WakeTarget::Gone | WakeTarget::Ambiguous { .. } => None,
            }
        }),
    });
    if let Err(error) = swept {
        // The sweep failed after the consumer was already acknowledged: it is stopped and reported dead here, or its
        // subscriptions would outlive a start its caller sees as failed.
        ended.store(true, Ordering::SeqCst);
        die();
        port.close();
        return Err(error.to_string());
    }
    // The rows the sweep settled leave their marks drained: the service is told now, so a settled mark is not handed
    // back at the next start (omo#14273). A report that fails is retried at the next drain.
    if let Err(error) = consumer.report_drained() {
        (input.log)(&format!(
            "thread wake: drained marks not reported after the sweep: {error}"
        ));
    }
    if ended.load(Ordering::SeqCst) {
        return Ok(None);
    }
    Ok(Some(ServingConsumer { consumer, port }))
}
