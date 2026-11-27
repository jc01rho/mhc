//! The query-only read of the caller's own delivery: `thread_read { thread, delivery_id }` (omo#14623).
//!
//! Upstream counterpart: `thread/remote/receipt.ts`. Only the authenticated caller's own send is answered. A remote
//! send's retained receipt answers through its reconciliation; any other delivery answers from the gateway store's
//! receipt outcome. Upstream's reconciliation also asks the peer device for its receipt over the sessions port; this
//! build's port has no such query, so the peer is never asked and the retained outcome stands, as it does upstream when
//! the peer does not know the query.

use serde_json::Value;

use crate::thread::contracts_results::ThreadReceiptOk;
use crate::thread::control::operations::{error_json, ok_json};
use crate::thread::errors::thread_tool_failure;
use crate::thread::store_api::GatewayStoreApi;
use crate::thread::store_ops::{ReceiptOutcomeRequest, StoreError};
use crate::thread::store_tool_receipts::ReconcileRemoteReceiptRequest;
use crate::thread::types::ReceiptOutcome;

/// What became of `delivery_id`, which `caller` sent to exactly `thread`, as the tool's answer.
pub fn read_receipt(
    store: &dyn GatewayStoreApi,
    caller: &str,
    thread: &str,
    delivery_id: &str,
) -> Value {
    match outcome_of(store, caller, thread, delivery_id) {
        Ok(outcome) => ok_json(&ThreadReceiptOk {
            thread: thread.to_owned(),
            delivery_id: delivery_id.to_owned(),
            outcome,
        }),
        Err(error) => error_json(thread_tool_failure(
            "internal_error",
            format!("The gateway store could not answer: {error}"),
            "Retry the call; if it keeps failing, check the gateway store.",
            None,
        )),
    }
}

fn outcome_of(
    store: &dyn GatewayStoreApi,
    caller: &str,
    thread: &str,
    delivery_id: &str,
) -> Result<ReceiptOutcome, StoreError> {
    let principal = format!("session:{caller}");
    let Some(retained) = store.remote_receipt(store.now(), &principal, delivery_id)? else {
        return store.receipt_outcome(&ReceiptOutcomeRequest {
            now: store.now(),
            principal,
            delivery_id: delivery_id.to_owned(),
            target_durable_id: thread.to_owned(),
            session_path: None,
        });
    };
    let sent = &retained.result;
    if sent["thread_id"].as_str() != Some(thread) {
        return Ok(ReceiptOutcome::OutcomeUnavailable);
    }
    let (Some(sent_id), Some(device)) = (sent["delivery_id"].as_str(), sent["device"].as_str())
    else {
        return Ok(ReceiptOutcome::OutcomeUnavailable);
    };
    store.reconcile_remote_receipt(&ReconcileRemoteReceiptRequest {
        now: store.now(),
        principal,
        idempotency_key: retained.idempotency_key.clone(),
        delivery_id: sent_id.to_owned(),
        target_durable_id: thread.to_owned(),
        device: device.to_owned(),
        outcome: ReceiptOutcome::OutcomeUnavailable,
    })
}
