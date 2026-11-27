//! Admission of one claimed row.
//!
//! Upstream counterpart: `thread/gateway/drain-admit.ts`. The lane is decided again here, from the session's phase and
//! turn read in the same synchronous step as the admission, so nothing can change between the decision and the engine
//! call: the decision made before the claim is only the lane the claim records. A row that joins a held-back start,
//! while the session is still idle, goes to the steering queue without starting it, so the start's run takes it before
//! its first provider request (steer-all). An auto row the engine still answers `turn_conflict` waits for the next
//! pass; only an explicit steer, which names its turn, is refused.

use super::adapter::{
    Admission, AdmitExternalMessageInput, ExternalAdmissionKind, GatewayAdmissionKind,
    RuntimePhase, WhenIdle,
};
use super::decision::{
    DeliveryDecision, Expect, RefusalKind, TargetState, decide_delivery, effective_lane_of,
};
use super::drain::{DrainOutputs, InboxDrain, admission_text, kind_label};
use super::provenance::delivery_sender;
use super::store_ops::{StoreError, engine_key};
use super::types::{
    DeliveryRow, EffectiveLane, GatewayDeliveryMode, OutcomeRecord, RecordOutcomeRequest,
    RefusalReason,
};

/// Whether the pass goes on to its next row or ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdmitStep {
    Next,
    Stop,
}

impl InboxDrain {
    fn record(&self, row: &DeliveryRow, outcome: OutcomeRecord) -> Result<(), StoreError> {
        self.options.store.record_outcome(&RecordOutcomeRequest {
            now: self.now(),
            delivery_id: row.delivery_id.clone(),
            self_identity: self.identity(),
            outcome,
        })?;
        Ok(())
    }

    fn refuse_claimed(
        &self,
        out: &mut DrainOutputs,
        row: &DeliveryRow,
        kind: RefusalKind,
    ) -> Result<AdmitStep, StoreError> {
        let (reason, gateway_kind) = match kind {
            RefusalKind::NotSteerable => (
                RefusalReason::NotSteerable,
                GatewayAdmissionKind::NotSteerable,
            ),
            RefusalKind::TurnConflict => (
                RefusalReason::TurnConflict,
                GatewayAdmissionKind::TurnConflict,
            ),
        };
        self.record(row, OutcomeRecord::Refused { reason })?;
        out.admitted.push(Admission {
            delivery_id: row.delivery_id.clone(),
            kind: ExternalAdmissionKind::TurnConflict,
        });
        out.outcomes.push((row.delivery_id.clone(), gateway_kind));
        Ok(AdmitStep::Next)
    }

    /// Admits one claimed row; `AdmitStep::Stop` ends the pass. `joins_start`: the row follows a held-back start of the
    /// same pass.
    pub(super) fn admit_claimed(
        &self,
        out: &mut DrainOutputs,
        row: &DeliveryRow,
        claimed: &DeliveryRow,
        joins_start: bool,
    ) -> Result<AdmitStep, StoreError> {
        let runtime = &self.options.runtime;
        let decision = decide_delivery(
            TargetState::Phase((runtime.phase)()),
            row.mode_requested,
            row.expected_turn_id,
            Some((runtime.admission_gate)().turn_epoch),
        );
        let (deliver_as, decided_turn, expect) = match decision {
            DeliveryDecision::Admit {
                deliver_as,
                expected_turn_id,
                expect,
            } => (deliver_as, expected_turn_id, expect),
            DeliveryDecision::Refuse { reason } => return self.refuse_claimed(out, row, reason),
            DeliveryDecision::QueuedOffline => {
                return self.refuse_claimed(out, row, RefusalKind::TurnConflict);
            }
        };
        // Only a row the live decision would start joins the held start: a turn-pinned steer or anything else keeps
        // its own lane.
        let joins = joins_start && expect == Expect::Started;
        let lane = if joins {
            EffectiveLane::Steer
        } else {
            effective_lane_of(deliver_as, expect)
        };
        // A row taken in from another device is admitted under the engine's ledger key with the sender's text as
        // written, so the engine's own dedupe also covers the same delivery arriving live (omo#13037, design 5.5).
        let engine_id = engine_key(claimed).to_owned();
        let mut shown = claimed.clone();
        shown.mode_effective = Some(lane);
        let (text, display_text) = admission_text(&shown);
        let admitted = (runtime.admit_external_message)(AdmitExternalMessageInput {
            delivery_id: engine_id.clone(),
            text,
            deliver_as: if joins {
                GatewayDeliveryMode::Steer
            } else {
                deliver_as
            },
            expected_turn_id: if joins { None } else { decided_turn },
            when_idle: joins.then_some(WhenIdle::Queue),
            sender: Some(delivery_sender(claimed)),
            display_text: Some(display_text),
        });
        let result = match admitted {
            Ok(result) => result,
            Err(error) => {
                self.record(row, OutcomeRecord::Requeue)?;
                self.log(&format!(
                    "admission closed for {}: {error}",
                    self.options.durable_id
                ));
                return Ok(AdmitStep::Stop);
            }
        };
        if result.kind == ExternalAdmissionKind::HeldDraft {
            self.record(row, OutcomeRecord::Requeue)?;
            if let Ok(mut held) = self.held.lock() {
                held.insert(
                    row.delivery_id.clone(),
                    (runtime.admission_gate)().editor_revision,
                );
            }
            self.notice_queued(row);
            out.admitted.push(Admission {
                delivery_id: row.delivery_id.clone(),
                kind: ExternalAdmissionKind::HeldDraft,
            });
            out.outcomes
                .push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
            return Ok(AdmitStep::Stop);
        }
        if result.kind == ExternalAdmissionKind::TurnConflict {
            if row.mode_requested == GatewayDeliveryMode::Auto {
                self.record(row, OutcomeRecord::Requeue)?;
            } else {
                self.record(
                    row,
                    OutcomeRecord::Refused {
                        reason: RefusalReason::TurnConflict,
                    },
                )?;
                out.admitted.push(Admission {
                    delivery_id: row.delivery_id.clone(),
                    kind: ExternalAdmissionKind::TurnConflict,
                });
                out.outcomes
                    .push((row.delivery_id.clone(), GatewayAdmissionKind::TurnConflict));
            }
            return Ok(AdmitStep::Next);
        }
        // A row queued to join the start runs in that start's turn, which is what a steered row tells its sender.
        let kind = if joins && result.kind == ExternalAdmissionKind::Queued {
            ExternalAdmissionKind::Steered
        } else {
            result.kind
        };
        if kind == ExternalAdmissionKind::Queued {
            self.notice_queued(claimed);
        }
        let written = result.kind == ExternalAdmissionKind::AlreadyAdmitted
            && (runtime.list_admitted_deliveries)()
                .emitted
                .contains(&engine_id);
        // A delivery queued while the session waits on a question waits on that approval (omo#13014).
        let wait_kind = (kind == ExternalAdmissionKind::Queued
            && (runtime.phase)() == RuntimePhase::WaitingQuestion)
            .then(|| "approval".to_owned());
        let admission_kind = kind_label(kind).to_owned();
        self.record(
            row,
            if written {
                OutcomeRecord::Applied {
                    admission_kind,
                    wait_kind,
                    turn_epoch: result.turn_epoch,
                    mode_effective: Some(lane),
                }
            } else {
                OutcomeRecord::Admitted {
                    admission_kind,
                    wait_kind,
                    turn_epoch: result.turn_epoch,
                    mode_effective: Some(lane),
                }
            },
        )?;
        out.admitted.push(Admission {
            delivery_id: row.delivery_id.clone(),
            kind,
        });
        out.outcomes.push((
            row.delivery_id.clone(),
            GatewayAdmissionKind::from_external(kind),
        ));
        if matches!(
            kind,
            ExternalAdmissionKind::Started
                | ExternalAdmissionKind::Steered
                | ExternalAdmissionKind::Queued
        ) && let Some(on_admitted) = &self.options.on_admitted
        {
            on_admitted(&row.delivery_id, kind_label(kind));
        }
        Ok(AdmitStep::Next)
    }
}

#[cfg(test)]
#[path = "drain_admit_tests.rs"]
mod tests;
