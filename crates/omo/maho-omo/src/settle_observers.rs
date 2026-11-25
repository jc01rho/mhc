//! Port of `omo-senpi/src/extension/settle-observers.ts` (omo v5.1.52).
//!
//! A settle subscription owns observation, not the work it observes. Expiry or retirement stops
//! new phases and ends the wait; it never terminalizes a reflection or signals its child.
//!
//! PERF-DIVERGENCE: upstream keeps observing the abandoned promise after the race is lost. A Rust
//! handler future borrows the event and context, so it cannot be detached; it is dropped at its next
//! await point instead. Work that must outlive the observer (durable writes) is expected to be
//! spawned by the handler itself and retried from durable state on the next settle, as upstream says.
//! The native `ExtensionHandlerOptions` has no `hook_label`, so the label is fixed to `onSettled`
//! unless the handler names a phase through the observer.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use maho_ai::utils::abort::{AbortController, AbortReason};
use maho_ext_api::{
    ComponentLogger, EventResult, ExtensionContext, ExtensionEvent, ExtensionFailure,
    ExtensionHandler, with_settle_observer,
};
pub use maho_ext_api::{SettleObserver, current_settle_observer};

pub const SETTLE_OBSERVER_BUDGET: Duration = Duration::from_millis(2_000);
const DEFAULT_HOOK_LABEL: &str = "onSettled";
const RETIRED: &str = "Settle observer retired";
const EXHAUSTED: &str = "Settle observer budget exhausted";
const SUPERSEDED: &str = "Settle hook superseded by a newer settle";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum InFlightHookKey {
    Named(String),
    Unnamed(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct InFlightKey {
    session_id: Option<String>,
    hook: InFlightHookKey,
}

type Pending = Arc<Mutex<BTreeMap<u64, (AbortController, Option<String>)>>>;
type InFlight = Arc<Mutex<BTreeMap<InFlightKey, (u64, AbortController)>>>;
struct CancellationOwner(Arc<()>);

struct ObserverSlotGuard {
    pending: Pending,
    in_flight: InFlight,
    id: u64,
    key: InFlightKey,
}

impl Drop for ObserverSlotGuard {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if in_flight
            .get(&self.key)
            .is_some_and(|(current_id, _)| *current_id == self.id)
        {
            in_flight.remove(&self.key);
        }
    }
}

#[derive(Clone)]
pub struct SettleObservers {
    pending: Pending,
    in_flight: InFlight,
    cancellation_owner: Arc<()>,
    next: Arc<Mutex<u64>>,
    next_slot: Arc<AtomicU64>,
    logger: Arc<dyn ComponentLogger>,
    budget: Duration,
}

fn session_id_of(context: &ExtensionContext) -> Option<String> {
    Some(context.session_manager.session_id().to_owned())
}

impl SettleObservers {
    pub fn new(logger: Arc<dyn ComponentLogger>) -> Self {
        Self::with_budget(logger, SETTLE_OBSERVER_BUDGET)
    }

    pub fn with_budget(logger: Arc<dyn ComponentLogger>, budget: Duration) -> Self {
        Self {
            pending: Arc::default(),
            in_flight: Arc::default(),
            cancellation_owner: Arc::new(()),
            next: Arc::default(),
            next_slot: Arc::default(),
            logger,
            budget,
        }
    }

    /// Aborts every observer owned by `session_id`, or all of them when it is unknown.
    pub fn retire(&self, session_id: Option<&str>) {
        let controllers: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|(_, owner)| session_id.is_none() || owner.as_deref() == session_id)
            .map(|(controller, _)| controller.clone())
            .collect();
        // Abort invokes listeners synchronously: never hold the pending guard across callbacks.
        for controller in controllers {
            controller.abort(Some(self.cancellation_reason("AbortError", RETIRED)));
        }
    }

    fn cancellation_reason(&self, name: &str, message: &str) -> AbortReason {
        AbortReason::new(name, message)
            .with_payload(CancellationOwner(self.cancellation_owner.clone()))
    }

    pub fn is_cancellation(&self, failure: &ExtensionFailure) -> bool {
        failure
            .abort_reason
            .as_deref()
            .and_then(AbortReason::payload::<CancellationOwner>)
            .is_some_and(|owner| Arc::ptr_eq(&owner.0, &self.cancellation_owner))
    }

    pub fn cancellation_failure(reason: &AbortReason) -> ExtensionFailure {
        ExtensionFailure::from_abort_reason(reason.clone())
    }

    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Wraps one `agent_settled` handler in a bounded observation.
    pub fn wrap(
        &self,
        component: &str,
        hook_label: Option<&str>,
        handler: ExtensionHandler,
    ) -> ExtensionHandler {
        let this = self.clone();
        let component = component.to_owned();
        let (label, hook_key) = match hook_label {
            Some(explicit) => (
                explicit.to_owned(),
                InFlightHookKey::Named(explicit.to_owned()),
            ),
            None => {
                let slot = self.next_slot.fetch_add(1, Ordering::Relaxed);
                (
                    DEFAULT_HOOK_LABEL.to_owned(),
                    InFlightHookKey::Unnamed(slot),
                )
            }
        };
        Arc::new(
            move |event: &mut ExtensionEvent, context: &ExtensionContext| {
                let this = this.clone();
                let component = component.clone();
                let label = label.clone();
                let hook_key = hook_key.clone();
                let handler = handler.clone();
                let session_id = session_id_of(context);
                Box::pin(async move {
                    this.observe(
                        &component, label, hook_key, handler, event, context, session_id,
                    )
                    .await
                })
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn observe(
        &self,
        component: &str,
        label: String,
        hook_key: InFlightHookKey,
        handler: ExtensionHandler,
        event: &mut ExtensionEvent,
        context: &ExtensionContext,
        session_id: Option<String>,
    ) -> Result<EventResult, ExtensionFailure> {
        let controller = AbortController::new();
        let signal = controller.signal();
        let observer = SettleObserver::new(signal.clone(), label.clone());
        let hook = observer.clone();
        let id = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            *next
        };
        let key = InFlightKey {
            session_id: session_id.clone(),
            hook: hook_key,
        };
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, (controller.clone(), session_id.clone()));
        let superseded = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), (id, controller.clone()));
        let slot_guard = ObserverSlotGuard {
            pending: Arc::clone(&self.pending),
            in_flight: Arc::clone(&self.in_flight),
            id,
            key,
        };
        // Abort after releasing the map lock; listeners fire synchronously.
        if let Some((_, prev)) = superseded.filter(|(_, prev)| !prev.signal().aborted()) {
            prev.abort(Some(self.cancellation_reason("AbortError", SUPERSEDED)));
            self.logger.warn(
                "extension_handler_superseded",
                Some(&serde_json::json!({
                    "event": "extension_handler_superseded",
                    "extension": "omo.js",
                    "component": component,
                    "hook": label,
                    "handlerEvent": "agent_settled",
                    "stage": "superseded",
                    "sessionId": session_id,
                })),
            );
        }
        let started = Instant::now();
        let outcome = {
            // Invoke inside the scope: a handler may read the observer before its first await.
            let work = with_settle_observer(observer, async { handler(event, context).await });
            tokio::select! {
                biased;
                () = signal.cancelled() => None,
                () = tokio::time::sleep(self.budget) => {
                    let current = hook.hook_label();
                    self.logger.warn(
                        "extension_handler_stalled",
                        Some(&serde_json::json!({
                            "event": "extension_handler_stalled", "extension": "omo.js",
                            "component": component, "hook": current, "handlerEvent": "agent_settled",
                            "stage": "timeout", "durationMs": started.elapsed().as_millis() as u64,
                            "sessionId": session_id,
                        })),
                    );
                    let controller = self.pending.lock().unwrap_or_else(PoisonError::into_inner)
                        .get(&id).map(|(controller, _)| controller.clone());
                    if let Some(controller) = controller {
                        controller.abort(Some(self.cancellation_reason("TimeoutError", EXHAUSTED)));
                    }
                    None
                }
                result = work => Some(result),
            }
        };
        drop(slot_guard);
        match outcome {
            Some(Err(err)) => {
                if signal.aborted() || self.is_cancellation(&err) {
                    Ok(EventResult::None)
                } else {
                    Err(err)
                }
            }
            Some(result) => result,
            None => Ok(EventResult::None),
        }
    }
}
