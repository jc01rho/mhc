//! Receiver half of the gateway: the only path that moves a row out of `queued`.
//!
//! Upstream counterpart: `thread/gateway/drain.ts`. Each pass starts with the store's `BEGIN
//! IMMEDIATE` barrier (`reconcile`), then admits queued rows in seq order with a two-phase claim:
//! T1 claims the row, the runtime call runs outside any transaction, T2 records the outcome.

pub type LogCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;
pub type OnAdmittedCallback = Option<Arc<dyn Fn(&str, &str) + Send + Sync>>;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::adapter::{
    Admission, DrainWakeEvent, GatewayAdmissionKind, RuntimePhase, SessionControlDrainResult,
    SessionRuntimePort, WakeReason, to_session_control_drain_result,
};
use super::decision::{
    DeliveryDecision, Expect, RefusalKind, TargetState, decide_delivery, effective_lane_of,
};
use super::drain_admit::AdmitStep;
use super::intake_admission::{AdmitIntake, IntakeAdmissionPass, IntakeVerdict};
use super::provenance::{delivery_provenance_line, render_delivery_text};
use super::store_api::GatewayStoreApi;
use super::types::{
    AdmissionLedger, ClaimOutcome, ClaimRequest, DeliveryRow, EffectiveLane, ProcessIdentity,
    ReconcileRequest, RefusalReason,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxDrainResult {
    pub admitted: Vec<Admission>,
    pub outcomes: Vec<(String, GatewayAdmissionKind)>,
}

impl InboxDrainResult {
    pub fn session_control_result(&self) -> SessionControlDrainResult {
        to_session_control_drain_result(&self.outcomes)
    }
}

/// The notice a session shows when a delivery waits behind its running turn or the user's draft.
pub fn queued_notice(row: &DeliveryRow) -> String {
    format!(
        "remote message from {} queued ({})",
        row.envelope.actor, row.delivery_id
    )
}

/// The text and display text the engine admits a row with. A row taken in from another device carries the sender's own
/// rendering verbatim, so the engine's dedupe also covers the same delivery arriving live (omo#13037); every other row
/// is rendered here.
pub(super) fn admission_text(row: &DeliveryRow) -> (String, String) {
    if row.ledger_id.is_some() {
        return (
            row.verbatim_text
                .clone()
                .unwrap_or_else(|| row.body.clone()),
            row.verbatim_display_text
                .clone()
                .unwrap_or_else(|| row.body.clone()),
        );
    }
    (
        render_delivery_text(row),
        format!(
            "{}\n{}",
            delivery_provenance_line(row.envelope.provenance.as_ref()),
            row.body
        ),
    )
}

fn releasing_reason(reason: WakeReason) -> bool {
    matches!(
        reason,
        WakeReason::Submission
            | WakeReason::DraftCleared
            | WakeReason::Idle
            | WakeReason::Start
            | WakeReason::Continue
    )
}

pub struct InboxDrainOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub runtime: SessionRuntimePort,
    pub durable_id: String,
    pub session_path: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub log: LogCallback,
    /// Shows the queued notice in the session's own UI.
    pub notify: LogCallback,
    /// A delivery this drain started, steered or queued into the session.
    pub on_admitted: OnAdmittedCallback,
    /// Asked once per sender device for intake rows from another device (omo#13037 5.6).
    pub admit_intake: Option<AdmitIntake>,
    /// True while a wake lease still guards this session's drain from racing an opening carrier.
    pub wake_gated: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// A held row (the user is composing) costs no store write and stops the pass, so nothing behind it
/// overtakes it.
pub struct InboxDrain {
    pub(super) options: Arc<InboxDrainOptions>,
    pub(super) held: Mutex<BTreeMap<String, i64>>,
    noticed: Mutex<BTreeMap<String, bool>>,
    /// One pass at a time, the way the upstream promise chain serializes them.
    pass: Mutex<()>,
}

impl InboxDrain {
    pub fn new(options: InboxDrainOptions) -> Self {
        Self {
            options: Arc::new(options),
            held: Mutex::new(BTreeMap::new()),
            noticed: Mutex::new(BTreeMap::new()),
            pass: Mutex::new(()),
        }
    }

    pub fn held_deliveries(&self) -> BTreeMap<String, i64> {
        self.held
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    pub fn is_session_referenced(&self) -> bool {
        self.options
            .store
            .is_referenced(&self.options.durable_id)
            .unwrap_or(false)
    }

    pub(super) fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => self.options.store.now(),
        }
    }

    pub(super) fn identity(&self) -> ProcessIdentity {
        self.options.store.identity().unwrap_or(ProcessIdentity {
            pid: std::process::id() as i64,
            process_start_time: None,
            instance_id: String::new(),
            runtime_instance: None,
        })
    }

    pub(super) fn notice_queued(&self, row: &DeliveryRow) {
        let Some(notify) = &self.options.notify else {
            return;
        };
        let already = self
            .noticed
            .lock()
            .map(|mut noticed| noticed.insert(row.delivery_id.clone(), true).is_some())
            .unwrap_or(true);
        if !already {
            notify(&queued_notice(row));
        }
    }

    pub(super) fn log(&self, line: &str) {
        if let Some(log) = &self.options.log {
            log(line);
        }
    }

    /// One drain pass; `event.reason` decides whether a held row may be released.
    pub fn drain(
        &self,
        event: &DrainWakeEvent,
    ) -> Result<InboxDrainResult, super::store_ops::StoreError> {
        let _serialized = self.pass.lock();
        let self_identity = self.identity();
        let ledger: AdmissionLedger = (self.options.runtime.list_admitted_deliveries)();
        let reconciled = self.options.store.reconcile(&ReconcileRequest {
            now: self.now(),
            target_durable_id: self.options.durable_id.clone(),
            self_identity,
            ledger,
            session_path: (self.options.session_path)(),
        })?;
        for id in &reconciled.dual_runtime {
            self.log(&format!(
                "dual_runtime: delivery {id} is claimed by another live process for {}",
                self.options.durable_id
            ));
        }
        let mut out = DrainOutputs::default();
        {
            let queued_ids: Vec<String> = reconciled
                .queued
                .iter()
                .map(|row| row.delivery_id.clone())
                .collect();
            if let Ok(mut held) = self.held.lock() {
                held.retain(|id, _| queued_ids.contains(id));
            }
        }
        if self
            .options
            .wake_gated
            .as_ref()
            .is_some_and(|gated| gated())
        {
            for row in &reconciled.queued {
                out.admitted.push(Admission {
                    delivery_id: row.delivery_id.clone(),
                    kind: super::adapter::ExternalAdmissionKind::HeldDraft,
                });
                out.outcomes
                    .push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
            }
            return Ok(out.into_result());
        }
        let mut intake_admission = IntakeAdmissionPass::new(self.options.admit_intake.clone());
        // The pass's first row that would start the idle session is admitted last, after every later row of the pass
        // has joined it, so that one start carries them all.
        let mut held_start: Option<(DeliveryRow, DeliveryRow)> = None;
        for (index, row) in reconciled.queued.iter().enumerate() {
            let gate = (self.options.runtime.admission_gate)();
            let held_revision = self
                .held
                .lock()
                .ok()
                .and_then(|held| held.get(&row.delivery_id).copied());
            if let Some(held_revision) = held_revision
                && !releasing_reason(event.reason)
                && held_revision == gate.editor_revision
            {
                break;
            }
            if !gate.can_admit {
                if let Ok(mut held) = self.held.lock() {
                    held.insert(row.delivery_id.clone(), gate.editor_revision);
                }
                self.notice_queued(row);
                out.admitted.push(Admission {
                    delivery_id: row.delivery_id.clone(),
                    kind: super::adapter::ExternalAdmissionKind::HeldDraft,
                });
                out.outcomes
                    .push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
                break;
            }
            if let Ok(mut held) = self.held.lock() {
                held.remove(&row.delivery_id);
            }
            match intake_admission.check(row) {
                IntakeVerdict::Admit => {}
                IntakeVerdict::Skip => continue,
                IntakeVerdict::Refuse => {
                    let _ = self.options.store.refuse_queued(
                        self.now(),
                        &row.delivery_id,
                        RefusalReason::SenderNotAdmitted,
                    );
                    out.outcomes.push((
                        row.delivery_id.clone(),
                        GatewayAdmissionKind::SenderNotAdmitted,
                    ));
                    continue;
                }
            }
            let decision = decide_delivery(
                TargetState::Phase((self.options.runtime.phase)()),
                row.mode_requested,
                row.expected_turn_id,
                Some(gate.turn_epoch),
            );
            let (deliver_as, expect) = match decision {
                DeliveryDecision::Admit {
                    deliver_as, expect, ..
                } => (deliver_as, expect),
                DeliveryDecision::Refuse { reason } => {
                    let (refusal, gateway_kind) = if reason == RefusalKind::NotSteerable {
                        (
                            RefusalReason::NotSteerable,
                            GatewayAdmissionKind::NotSteerable,
                        )
                    } else {
                        (
                            RefusalReason::TurnConflict,
                            GatewayAdmissionKind::TurnConflict,
                        )
                    };
                    let _ = self
                        .options
                        .store
                        .refuse_queued(self.now(), &row.delivery_id, refusal);
                    out.admitted.push(Admission {
                        delivery_id: row.delivery_id.clone(),
                        kind: super::adapter::ExternalAdmissionKind::TurnConflict,
                    });
                    out.outcomes.push((row.delivery_id.clone(), gateway_kind));
                    continue;
                }
                DeliveryDecision::QueuedOffline => {
                    let _ = self.options.store.refuse_queued(
                        self.now(),
                        &row.delivery_id,
                        RefusalReason::TurnConflict,
                    );
                    out.admitted.push(Admission {
                        delivery_id: row.delivery_id.clone(),
                        kind: super::adapter::ExternalAdmissionKind::TurnConflict,
                    });
                    out.outcomes
                        .push((row.delivery_id.clone(), GatewayAdmissionKind::TurnConflict));
                    continue;
                }
            };
            let starts = expect == Expect::Started;
            let claimed = self.options.store.claim(&ClaimRequest {
                now: self.now(),
                delivery_id: row.delivery_id.clone(),
                self_identity: self.identity(),
                lane: if starts && held_start.is_some() {
                    EffectiveLane::Steer
                } else {
                    effective_lane_of(deliver_as, expect)
                },
            })?;
            let ClaimOutcome::Claimed { row: claimed_row } = claimed else {
                continue;
            };
            // A run that began while the pass waited on the store takes the held start first, so the session still
            // sees the rows in seq order.
            if held_start.is_some()
                && (self.options.runtime.phase)() != RuntimePhase::Idle
                && let Some((first, first_claimed)) = held_start.take()
                && self.admit_claimed(&mut out, &first, &first_claimed, false)? == AdmitStep::Stop
            {
                break;
            }
            if starts
                && held_start.is_none()
                && index + 1 < reconciled.queued.len()
                && (self.options.runtime.phase)() == RuntimePhase::Idle
            {
                held_start = Some((row.clone(), *claimed_row));
                continue;
            }
            let joins_start = held_start.is_some();
            if self.admit_claimed(&mut out, row, &claimed_row, joins_start)? == AdmitStep::Stop {
                break;
            }
        }
        if let Some((row, claimed)) = held_start.take() {
            self.admit_claimed(&mut out, &row, &claimed, false)?;
        }
        let order: BTreeMap<&str, usize> = reconciled
            .queued
            .iter()
            .enumerate()
            .map(|(index, row)| (row.delivery_id.as_str(), index))
            .collect();
        let position = |id: &str| order.get(id).copied().unwrap_or(0);
        out.admitted
            .sort_by_key(|admission| position(&admission.delivery_id));
        out.outcomes.sort_by_key(|(id, _)| position(id));
        Ok(out.into_result())
    }
}

/// What one drain pass admitted, as the host-facing admissions and the gateway's own outcome per row.
#[derive(Default)]
pub(super) struct DrainOutputs {
    pub(super) admitted: Vec<Admission>,
    pub(super) outcomes: Vec<(String, GatewayAdmissionKind)>,
}

impl DrainOutputs {
    fn into_result(self) -> InboxDrainResult {
        InboxDrainResult {
            admitted: self.admitted,
            outcomes: self.outcomes,
        }
    }
}

pub(super) fn kind_label(kind: super::adapter::ExternalAdmissionKind) -> &'static str {
    match kind {
        super::adapter::ExternalAdmissionKind::Started => "started",
        super::adapter::ExternalAdmissionKind::Queued => "queued",
        super::adapter::ExternalAdmissionKind::Steered => "steered",
        super::adapter::ExternalAdmissionKind::Held => "held",
        super::adapter::ExternalAdmissionKind::TurnConflict => "turn_conflict",
        super::adapter::ExternalAdmissionKind::HeldDraft => "held_draft",
        super::adapter::ExternalAdmissionKind::AlreadyAdmitted => "already_admitted",
    }
}

/// The gateway admission kind a drain result reports for one delivery.
pub fn gateway_kind(kind: super::adapter::ExternalAdmissionKind) -> GatewayAdmissionKind {
    GatewayAdmissionKind::from_external(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::store_ops::engine_key;
    use crate::thread::types::{EnvelopeOrigin, GatewayDeliveryMode, StoredDeliveryEnvelope};

    fn row() -> DeliveryRow {
        DeliveryRow {
            delivery_id: "d-1".into(),
            target_durable_id: "receiver".into(),
            seq: 1,
            sender: "session:sender".into(),
            sender_turn: None,
            envelope: StoredDeliveryEnvelope {
                provenance: None,
                origin: EnvelopeOrigin::Session {
                    session: "sender".into(),
                },
                actor: "planner".into(),
                via: vec![],
                root_id: "root".into(),
                hop: 1,
                delivery_id: "d-1".into(),
            },
            body: "hi".into(),
            bytes: 2,
            mode_requested: GatewayDeliveryMode::Auto,
            mode_effective: None,
            expected_turn_id: None,
            state: super::super::types::DeliveryState::Queued,
            reason: None,
            admitted_by: None,
            claimed_at: None,
            attempt: 0,
            admission_kind: None,
            wait_kind: None,
            turn_epoch: None,
            root_id: "root".into(),
            hop: 1,
            created_at: 1,
            updated_at: 1,
            expires_at: 9,
            ledger_id: None,
            verbatim_text: None,
            verbatim_display_text: None,
            collapse_key: None,
        }
    }

    #[test]
    fn the_queued_notice_names_the_sender_and_the_delivery() {
        assert_eq!(
            queued_notice(&row()),
            "remote message from planner queued (d-1)"
        );
    }

    #[test]
    fn a_row_taken_in_from_another_device_is_admitted_under_its_ledger_key_as_written() {
        let mut intake = row();
        intake.ledger_id = Some("device:b/d-1".into());
        intake.verbatim_text = Some("the sender's rendered text".into());
        intake.verbatim_display_text = Some("shown".into());
        assert_eq!(engine_key(&intake), "device:b/d-1");
        assert_eq!(
            admission_text(&intake),
            ("the sender's rendered text".to_owned(), "shown".to_owned())
        );

        let local = row();
        assert_eq!(engine_key(&local), "d-1");
        let (text, display_text) = admission_text(&local);
        assert_eq!(text, render_delivery_text(&local));
        assert!(display_text.ends_with("\nhi"), "{display_text}");
    }

    #[test]
    fn only_a_releasing_reason_lets_a_held_row_go() {
        assert!(releasing_reason(WakeReason::Submission));
        assert!(releasing_reason(WakeReason::DraftCleared));
        assert!(!releasing_reason(WakeReason::Inbox));
        assert!(!releasing_reason(WakeReason::Emitted));
    }

    #[test]
    fn held_admission_and_waiting_question_write_their_outcomes_without_noticing_a_held_row() {
        use super::super::adapter::{
            AdmitResult, ExternalAdmissionKind, RuntimePhase, SessionAdmissionGate,
        };
        use super::super::store::{GatewayStoreOptions, create_gateway_store};
        use super::super::types::{EnqueueOutcome, EnqueueRequest, Provenance, ReceiptRef};

        let dir = tempfile::tempdir().expect("temp dir");
        let store = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store");
        let enqueue = |id: &str, now: i64| {
            let outcome = store
                .enqueue(&EnqueueRequest {
                    provenance: Provenance {
                        workspace: "w".into(),
                        project: "p".into(),
                        lane: None,
                        session: None,
                    },
                    now,
                    delivery_id: id.into(),
                    target_durable_id: "receiver".into(),
                    sender_principal: "session:sender".into(),
                    rate_principal: None,
                    sender_node: "sender".into(),
                    sender_turn: None,
                    cause_delivery_id: None,
                    claimed_root_id: None,
                    origin: EnvelopeOrigin::Session {
                        session: "sender".into(),
                    },
                    actor: "planner".into(),
                    body: id.into(),
                    mode: GatewayDeliveryMode::Auto,
                    expected_turn_id: None,
                    receipt: ReceiptRef {
                        idempotency_key: id.into(),
                        args_hash: id.into(),
                    },
                    endpoint_kind: None,
                    collapse_key: None,
                })
                .expect("enqueue");
            assert!(matches!(outcome, EnqueueOutcome::Inserted { .. }));
        };
        enqueue("d-held", 1_000);

        let phase = Arc::new(Mutex::new(RuntimePhase::Idle));
        let phase_read = phase.clone();
        let notices = Arc::new(Mutex::new(Vec::<String>::new()));
        let noticed = notices.clone();
        let admitted_log = Arc::new(Mutex::new(Vec::<String>::new()));
        let admitted_cb = admitted_log.clone();
        let drain = InboxDrain::new(InboxDrainOptions {
            store: store.clone(),
            runtime: SessionRuntimePort {
                phase: Arc::new(move || *phase_read.lock().expect("phase")),
                admission_gate: Arc::new(|| SessionAdmissionGate {
                    can_admit: true,
                    hold_reason: None,
                    editor_revision: 1,
                    turn_epoch: 0,
                }),
                admit_external_message: Arc::new(|input| {
                    let kind = if input.delivery_id == "d-held" {
                        ExternalAdmissionKind::Held
                    } else {
                        ExternalAdmissionKind::Queued
                    };
                    Ok(AdmitResult {
                        kind,
                        turn_epoch: 0,
                    })
                }),
                list_admitted_deliveries: Arc::new(AdmissionLedger::default),
            },
            durable_id: "receiver".into(),
            session_path: Arc::new(|| None),
            now: Some(Arc::new(|| 1_001)),
            log: None,
            notify: Some(Arc::new(move |line| {
                noticed.lock().expect("notices").push(line.to_owned())
            })),
            on_admitted: Some(Arc::new(move |id, kind| {
                admitted_cb
                    .lock()
                    .expect("admitted")
                    .push(format!("{id}:{kind}"))
            })),
            admit_intake: None,
            wake_gated: None,
        });

        let result = drain
            .drain(&DrainWakeEvent {
                reason: WakeReason::Start,
                delivery_ids: None,
            })
            .expect("first drain");
        assert_eq!(result.admitted.len(), 1);
        assert!(
            notices.lock().expect("notices").is_empty(),
            "held row must not trigger queued notice"
        );
        assert!(admitted_log.lock().expect("admitted").is_empty());

        enqueue("d-question", 1_002);
        *phase.lock().expect("phase") = RuntimePhase::WaitingQuestion;
        drain
            .drain(&DrainWakeEvent {
                reason: WakeReason::Inbox,
                delivery_ids: None,
            })
            .expect("question drain");
        let held_view = store
            .delivery_view("d-held")
            .expect("view")
            .expect("d-held");
        assert_eq!(held_view.row.admission_kind.as_deref(), Some("held"));
        assert_eq!(held_view.row.wait_kind, None);
        let q_view = store
            .delivery_view("d-question")
            .expect("view")
            .expect("d-question");
        assert_eq!(q_view.row.admission_kind.as_deref(), Some("queued"));
        assert_eq!(q_view.row.wait_kind.as_deref(), Some("approval"));
    }
}
