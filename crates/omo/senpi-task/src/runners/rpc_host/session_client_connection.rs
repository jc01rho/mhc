//! Engine-connection-local task-parent wire and the owner of the connection an open dials
//! (`session-client-connection.ts`).
use super::{
    host_command_dispatch::{HostRpcClient, dispatch_host_command},
    session_client_opening::BindClient,
    task_parent_wire::{HostRunHoldClient, create_host_task_parent_wire},
};
use crate::{host::HostError, runners::task_parent_control::TaskParentWire};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
#[derive(Clone)]
pub struct SessionClientConnection {
    pub routing_id: String,
    client: Arc<dyn HostRpcClient>,
    run_hold: Arc<dyn HostRunHoldClient>,
    parent_wire: Arc<OnceLock<Arc<TaskParentWire>>>,
    claims: crate::runners::rpc::turn_claim::TurnClaim,
    watch: Option<
        Arc<
            dyn Fn() -> crate::runners::in_process::native_run_pause::EngineTurnWatch + Send + Sync,
        >,
    >,
}
impl SessionClientConnection {
    pub fn new(
        routing_id: String,
        client: Arc<dyn HostRpcClient>,
        run_hold: Arc<dyn HostRunHoldClient>,
    ) -> Self {
        Self {
            routing_id,
            client,
            run_hold,
            parent_wire: Arc::new(OnceLock::new()),
            claims: Default::default(),
            watch: None,
        }
    }
    pub fn with_turn_watcher(
        mut self,
        watch: Arc<
            dyn Fn() -> crate::runners::in_process::native_run_pause::EngineTurnWatch + Send + Sync,
        >,
    ) -> Self {
        self.watch = Some(watch);
        self
    }
    /// Called by the connection's agent_start listener; true means Resume owns this turn.
    pub fn consume_turn_claim(&self) -> bool {
        self.claims.consume()
    }
    pub fn task_parent_wire(&self) -> Arc<TaskParentWire> {
        self.parent_wire
            .get_or_init(|| create_host_task_parent_wire(self.run_hold.clone()))
            .clone()
    }
    pub fn send(&self, command: serde_json::Value) -> Result<(), HostError> {
        dispatch_host_command(self.client.as_ref(), Some(&self.routing_id), command)
    }
}

impl super::handle_port::HostSessionPort for SessionClientConnection {
    fn get_available_models(
        &self,
    ) -> Result<Vec<crate::runners::pinned_model_equivalence::CatalogModelIdentity>, HostError>
    {
        self.client.get_available_models()
    }
    fn wait_for_outcome(&self) -> Option<crate::runners::RunnerOutcome> {
        self.client.wait_for_outcome()
    }
    fn transport_lost(&self, message: String) {
        self.client.transport_lost(message)
    }
    fn native_pause(&self) -> Option<Arc<dyn crate::pause::native_child_pause::NativeRunPause>> {
        Some(Arc::new(self.clone()))
    }
    fn consume_turn_claim(&self) -> bool {
        SessionClientConnection::consume_turn_claim(self)
    }
    fn task_parent_wire(&self) -> Option<Arc<TaskParentWire>> {
        Some(SessionClientConnection::task_parent_wire(self))
    }
    fn send(&self, command: serde_json::Value) -> Result<(), HostError> {
        SessionClientConnection::send(self, command)
    }
    fn get_state(&self) -> Result<serde_json::Value, HostError> {
        self.client.get_state()
    }
    fn on_event(
        &self,
        listener: crate::manager::child_handle::ManagedChildListener,
    ) -> crate::manager::child_handle::Unsubscribe {
        self.client.on_event(listener)
    }
    fn close(&self) -> Result<(), HostError> {
        self.client.close(&self.routing_id)
    }
    fn detach(&self) -> Result<(), HostError> {
        self.client.detach(&self.routing_id)
    }
    fn get_pending_inputs(
        &self,
        known_client_message_ids: &[String],
    ) -> Result<Option<serde_json::Value>, HostError> {
        self.client.get_pending_inputs(known_client_message_ids)
    }
    /// `closeConfirmed`: only a live post-close host listing releases ownership.
    fn close_confirmed(&self) -> Option<Result<(), HostError>> {
        Some(super::session_close_proof::confirm_host_session_absent(
            self.client.as_ref(),
            &self.routing_id,
            &|| true,
        ))
    }
}

impl crate::pause::native_child_pause::NativeRunPause for SessionClientConnection {
    fn pause_for_run(
        &self,
        owner: crate::pause::ports::RunPauseOwner,
    ) -> Result<
        crate::pause::ports::EngineInheritReceipt,
        crate::pause::native_child_pause::NativePauseError,
    > {
        let control = crate::runners::task_parent_control::create_task_parent_control(
            self.task_parent_wire(),
        );
        let owner = crate::runners::task_parent_control::RunPauseOwner {
            parent_session_id: owner.parent_session_id,
            parent_pause_seq: owner.parent_pause_seq,
            run_ref: crate::runners::task_parent_control::RunRef {
                kind: match owner.run_ref.kind {
                    crate::pause::ports::RunRefKind::Dag => "dag",
                    crate::pause::ports::RunRefKind::Task => "task",
                    crate::pause::ports::RunRefKind::Workpool => "workpool",
                    crate::pause::ports::RunRefKind::Team => "team",
                }
                .into(),
                id: owner.run_ref.id,
            },
        };
        control
            .pause_for_run(&owner)
            .map(|result| match result {
                crate::runners::task_parent_control::EngineInheritReceipt::Held { child_seq } => {
                    crate::pause::ports::EngineInheritReceipt::Inherited { child_seq }
                }
                crate::runners::task_parent_control::EngineInheritReceipt::WrongParent => {
                    crate::pause::ports::EngineInheritReceipt::WrongParent
                }
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
    fn resume_for_run(
        &self,
        request: crate::pause::ports::ResumeTurnRequest,
    ) -> Result<
        crate::pause::native_child_pause::NativeResumeResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        let control = crate::runners::task_parent_control::create_task_parent_control(
            self.task_parent_wire(),
        );
        let request = crate::runners::task_parent_control::ResumeRequest {
            parent_session_id: request.owner.parent_session_id,
            parent_pause_seq: request.owner.parent_pause_seq,
            resume_id: request.resume_id,
            child_seq_cutoff: request.child_seq_cutoff,
        };
        let watch = self.watch.as_ref().ok_or_else(|| {
            crate::pause::native_child_pause::NativePauseError::Engine(
                "native_pause_unavailable: missing turn watcher".into(),
            )
        })?();
        let release = self.claims.claim();
        match control.resume_for_run(&request) {
            Ok(crate::runners::task_parent_control::EngineResumeReceipt::Released {
                turn_id: Some(turn_id),
                other_hold_live,
            }) if other_hold_live != Some(true) => {
                Ok(crate::pause::native_child_pause::NativeResumeResult::Turn(
                    crate::pause::ports::ResumedChildTurn {
                        turn_id,
                        outcome: watch.outcome,
                    },
                ))
            }
            Ok(crate::runners::task_parent_control::EngineResumeReceipt::Released { .. }) => {
                release();
                (watch.cancel)();
                Ok(crate::pause::native_child_pause::NativeResumeResult::OtherHoldLive)
            }
            Ok(crate::runners::task_parent_control::EngineResumeReceipt::Refused { reason }) => {
                release();
                (watch.cancel)();
                Ok(
                    crate::pause::native_child_pause::NativeResumeResult::Refused(
                        match reason.as_str() {
                            "wrong_parent" => crate::pause::ports::ResumeRefusal::WrongParent,
                            "not_held" => crate::pause::ports::ResumeRefusal::NotHeld,
                            _ => crate::pause::ports::ResumeRefusal::Stale,
                        },
                    ),
                )
            }
            Err(error) => {
                release();
                (watch.cancel)();
                Err(crate::pause::native_child_pause::NativePauseError::Engine(
                    error.to_string(),
                ))
            }
        }
    }
    fn hold_input(
        &self,
        parent: &str,
        input: crate::pause::ports::HoldInput,
    ) -> Result<
        crate::pause::ports::EngineHoldInputResult,
        crate::pause::native_child_pause::NativePauseError,
    > {
        let control = crate::runners::task_parent_control::create_task_parent_control(
            self.task_parent_wire(),
        );
        control
            .hold_input(
                parent,
                &crate::runners::task_parent_control::HeldGuidanceInput {
                    input_id: input.input_id,
                    revision: input.revision,
                    text: input.text,
                },
            )
            .map(|result| match result {
                crate::runners::task_parent_control::EngineHoldInputResult::Held {
                    entry_id,
                    duplicate,
                } => crate::pause::ports::EngineHoldInputResult::Held {
                    entry_id,
                    duplicate,
                },
                crate::runners::task_parent_control::EngineHoldInputResult::AlreadyDelivered {
                    revision,
                } => crate::pause::ports::EngineHoldInputResult::AlreadyDelivered { revision },
                crate::runners::task_parent_control::EngineHoldInputResult::InputIdConflict {
                    held_revision,
                } => crate::pause::ports::EngineHoldInputResult::InputIdConflict { held_revision },
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
    fn observe_hold(
        &self,
        parent: &str,
    ) -> Result<
        Option<crate::pause::ports::ObservedHold>,
        crate::pause::native_child_pause::NativePauseError,
    > {
        let control = crate::runners::task_parent_control::create_task_parent_control(
            self.task_parent_wire(),
        );
        control
            .observe_hold(parent)
            .map(|result| {
                result.map(|hold| crate::pause::ports::ObservedHold {
                    held: hold.held,
                    child_seq: hold.child_seq,
                })
            })
            .map_err(|error| {
                crate::pause::native_child_pause::NativePauseError::Engine(error.to_string())
            })
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct OwnedConnection {
    /// `detachments`: how often `detach()` has run. An open that began before the latest one must not bind.
    detachments: u64,
    client: Option<Arc<dyn HostRpcClient>>,
}

/// The owner of the one engine connection a child's open dials (`HostSessionClient`).
///
/// An open dials a connection nobody holds yet, so a `detach()` that lands mid-dial finds nothing to
/// stop. The owner counts detachments instead: the open reads the count when it begins and may bind
/// its connection only while the count still matches, so a connection it must refuse is stopped by
/// the open itself.
#[derive(Default)]
pub struct HostSessionClient {
    state: Mutex<OwnedConnection>,
}

impl HostSessionClient {
    /// Begins one open and returns its `bindClient`.
    pub fn begin_open(self: &Arc<Self>) -> BindClient {
        let owner = Arc::clone(self);
        let detachments_at_open = locked(&owner.state).detachments;
        Arc::new(move |client: Option<Arc<dyn HostRpcClient>>| {
            owner.bind(detachments_at_open, client)
        })
    }

    fn bind(&self, detachments_at_open: u64, client: Option<Arc<dyn HostRpcClient>>) -> bool {
        let mut owned = locked(&self.state);
        // `detach()` ran while this open was dialing and had nothing to stop: binding now would leave a
        // live connection on a released owner that no teardown ever ends.
        if client.is_some() && owned.detachments != detachments_at_open {
            return false;
        }
        owned.client = client;
        true
    }

    /// Drops the connection and leaves the session running on the host (retained, no attachment).
    pub fn detach(&self) -> Result<(), HostError> {
        let client = {
            let mut owned = locked(&self.state);
            owned.detachments += 1;
            owned.client.take()
        };
        // No routing handle exists before the open answers, and stopping the connection needs none.
        client.map_or(Ok(()), |client| client.detach(""))
    }
}

/// The opens a teardown can still reach. A TypeScript teardown holds the `HostSessionClient` it
/// opened; here the owner lives inside the open call, so an open registers it for as long as it runs
/// and a recovery that gave up can detach whatever its attempts left in flight.
#[derive(Default)]
pub struct InFlightOpens {
    owners: Mutex<Vec<Arc<HostSessionClient>>>,
}

/// Keeps an open registered in [`InFlightOpens`] until the open returns.
pub struct InFlightOpen {
    opens: Arc<InFlightOpens>,
    owner: Arc<HostSessionClient>,
}

impl InFlightOpens {
    #[must_use = "the open is reachable only while this guard is held"]
    pub fn register(self: &Arc<Self>, owner: &Arc<HostSessionClient>) -> InFlightOpen {
        locked(&self.owners).push(Arc::clone(owner));
        InFlightOpen {
            opens: Arc::clone(self),
            owner: Arc::clone(owner),
        }
    }

    /// Detaches every open still running. All are tried; the first failure is returned.
    pub fn detach_all(&self) -> Result<(), HostError> {
        let owners = locked(&self.owners).clone();
        let mut first = None;
        for owner in owners {
            if let Err(error) = owner.detach()
                && first.is_none()
            {
                first = Some(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

impl Drop for InFlightOpen {
    fn drop(&mut self) {
        locked(&self.opens.owners).retain(|owner| !Arc::ptr_eq(owner, &self.owner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Client;
    impl HostRpcClient for Client {
        fn on_event(
            &self,
            _: crate::manager::child_handle::ManagedChildListener,
        ) -> crate::manager::child_handle::Unsubscribe {
            Box::new(|| {})
        }
        fn command(&self, _: serde_json::Value) -> Result<(), HostError> {
            Ok(())
        }
    }
    impl HostRunHoldClient for Client {}
    #[test]
    fn wire_is_reused_only_within_one_engine_connection() {
        let a = SessionClientConnection::new("a".into(), Arc::new(Client), Arc::new(Client));
        let b = SessionClientConnection::new("b".into(), Arc::new(Client), Arc::new(Client));
        assert!(Arc::ptr_eq(&a.task_parent_wire(), &a.task_parent_wire()));
        assert!(!Arc::ptr_eq(&a.task_parent_wire(), &b.task_parent_wire()));
    }
}

#[cfg(test)]
#[path = "session_client_lifetime_tests.rs"]
mod lifetime_tests;
