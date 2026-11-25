//! The pending sweep (omo#13037 design 5.4 step 6, 5.6 at wake).
//!
//! Upstream counterpart: `thread/wake/pending-sweep.ts`. Run when the consumer connects and when an engine endpoint
//! appears: a session holding open intake rows is woken once one of its senders is admitted now; the rows of a sender
//! the service refuses settle `sender_not_admitted`, and a sender with no answer leaves its rows queued for the next
//! sweep. Rows queued before the session was closed on request settle `target_closed` first, so a close is never undone
//! by a message that was waiting when it happened (omo#14273); a session left with nothing queued is not woken.

use std::collections::HashMap;
use std::sync::Arc;

use super::carrier::AdmitIntake;
use super::closed_intake::settle_closed_intake;
use crate::thread::intake_admission::IntakeAdmission;
use crate::thread::store::SqliteGatewayStore;
use crate::thread::store_api::GatewayStoreApi;
use crate::thread::store_ops::StoreError;
use crate::thread::types::{DeliveryState, RefusalReason};

pub type WakeSession = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type SessionSwitch = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type ClosedAt = Arc<dyn Fn(&str) -> Option<i64> + Send + Sync>;

pub struct PendingSweepOptions {
    pub store: Arc<SqliteGatewayStore>,
    pub admit_intake: AdmitIntake,
    pub wake: WakeSession,
    /// `session_wake.same_device` for the SESSION's own cwd (its project's configuration, 5.3), by durable id.
    pub same_device: SessionSwitch,
    /// When the session was last closed on request (`session_target.rs` `closed_at`); `None` when never.
    pub closed_at: ClosedAt,
}

#[derive(Debug, thiserror::Error)]
pub enum PendingSweepError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Wake(String),
}

pub fn run_pending_sweep(options: &PendingSweepOptions) -> Result<(), PendingSweepError> {
    let store = &options.store;
    let mut answers: HashMap<String, IntakeAdmission> = HashMap::new();
    for session in store.intake_marks().open_sessions()? {
        if let Some(closed_at) = (options.closed_at)(&session.durable_id)
            && settle_closed_intake(store.as_ref(), &session.durable_id, Some(closed_at))?
                .is_empty()
        {
            continue;
        }
        // Local first: a session that is not local can never wake this way, so its project's setting is never read.
        let mut wake = session.local && (options.same_device)(&session.durable_id);
        for device in &session.devices {
            let answer = *answers
                .entry(device.clone())
                .or_insert_with(|| (options.admit_intake)(device));
            if answer == IntakeAdmission::Admitted {
                wake = true;
            }
            if answer != IntakeAdmission::Refused {
                continue;
            }
            let prefix = format!("device:{device}/");
            for row in store.list(Some(session.durable_id.as_str()), None)? {
                if row.state == DeliveryState::Queued
                    && row
                        .ledger_id
                        .as_deref()
                        .is_some_and(|ledger_id| ledger_id.starts_with(&prefix))
                {
                    store.refuse_queued(
                        store.now(),
                        &row.delivery_id,
                        RefusalReason::SenderNotAdmitted,
                    )?;
                }
            }
        }
        if wake {
            (options.wake)(&session.durable_id).map_err(PendingSweepError::Wake)?;
        }
    }
    Ok(())
}
