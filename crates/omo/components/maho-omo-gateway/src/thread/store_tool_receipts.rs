//! Tool receipts: the durable idempotency record of a thread tool call, in the gateway store's `receipts` table.
//!
//! Upstream counterpart: `thread/gateway/store-tool-receipts.ts`. Every operation is one `BEGIN IMMEDIATE`
//! transaction. The tool-receipt wrapper that calls these around a tool's execution is not ported (`create_tool.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::constants::GATEWAY_RECEIPT_RETENTION_MS;
use super::store_ops::{
    SqlAccess, StoreContext, StoreError, row_opt_str, row_str, transaction, v_i64, v_str,
};
use super::types::ReceiptOutcome;

/// The operation a delivery to a session on another device is receipted under.
pub const REMOTE_SEND_OPERATION: &str = "thread_send:remote";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReceiptScope {
    pub principal: String,
    pub operation: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReceiptBeginRequest {
    pub scope: ToolReceiptScope,
    pub now: i64,
    pub args_hash: String,
    /// A caller that may recover an uncertain call (thread_create, omo#14060) takes it back to `prepared`.
    pub retry_uncertain: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolReceiptSettlement {
    Completed { result: Value },
    Uncertain { error_note: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolReceiptSettleRequest {
    pub scope: ToolReceiptScope,
    pub now: i64,
    pub settlement: ToolReceiptSettlement,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolReceiptReviseRequest {
    pub scope: ToolReceiptScope,
    pub now: i64,
    pub result: Value,
}

/// The first cause a reserved retry of an uncertain call keeps through every later settle or takeover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRecovery {
    pub error_note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolReceiptBegin {
    Accepted,
    Replay {
        result: Value,
    },
    Conflict,
    /// This instance holds the key's `prepared` receipt. `recovery` names a reserved retry of an uncertain call (it
    /// keeps the first cause); a fresh call's reservation has none. A caller whose admission reply was lost resumes by
    /// it (omo#14623 review round 2 H1), so a lost recovery reply never restarts the call as a fresh one.
    InProgress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<ToolRecovery>,
    },
    Uncertain {
        error_note: Option<String>,
    },
    /// The uncertain receipt is now this call's to recover (`retry_uncertain`): settle it like a new one.
    Retry {
        error_note: Option<String>,
    },
}

struct StoredReceipt {
    args_hash: String,
    status: String,
    owner_instance: String,
    result: Option<String>,
    error_note: Option<String>,
}

fn select_receipt(
    ctx: &StoreContext,
    scope: &ToolReceiptScope,
) -> Result<Option<StoredReceipt>, StoreError> {
    let found = ctx.one(
        "SELECT args_hash, status, owner_instance, result, error_note FROM receipts WHERE principal = ? AND operation = ? AND idempotency_key = ?",
        &[
            v_str(&scope.principal),
            v_str(&scope.operation),
            v_str(&scope.idempotency_key),
        ],
    )?;
    Ok(found.map(|row| StoredReceipt {
        args_hash: row_str(&row, "args_hash"),
        status: row_str(&row, "status"),
        owner_instance: row_str(&row, "owner_instance"),
        result: row_opt_str(&row, "result"),
        error_note: row_opt_str(&row, "error_note"),
    }))
}

/// `prepared` before the side effect, `completed` with its result after, `uncertain` when it threw. A prepared receipt
/// of ANOTHER instance means that process died mid-call: the effect may have landed, so the receipt becomes
/// `uncertain` and is never retried unless the caller may recover it (`retry_uncertain`), which takes it back to
/// `prepared` under its own instance in the same transaction, so one retry runs at a time and an overlapping one is
/// `in_progress`. The first cause (`error_note`) is kept through every later settle or takeover.
pub fn tool_receipt_begin(
    ctx: &StoreContext,
    request: &ToolReceiptBeginRequest,
) -> Result<ToolReceiptBegin, StoreError> {
    transaction(ctx, "tool_receipt_begin", || {
        let scope = &request.scope;
        ctx.run(
            "DELETE FROM receipts WHERE principal = ? AND operation = ? AND idempotency_key = ? AND expires_at <= ?",
            &[
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
                v_i64(request.now),
            ],
        )?;
        admit_tool_receipt(ctx, request)
    })
}

fn admit_tool_receipt(
    ctx: &StoreContext,
    request: &ToolReceiptBeginRequest,
) -> Result<ToolReceiptBegin, StoreError> {
    let scope = &request.scope;
    let Some(receipt) = select_receipt(ctx, scope)? else {
        ctx.run(
            "INSERT INTO receipts (principal, operation, idempotency_key, args_hash, status, delivery_id, owner_instance, created_at, updated_at, expires_at) VALUES (?, ?, ?, ?, 'prepared', NULL, ?, ?, ?, ?)",
            &[
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
                v_str(&request.args_hash),
                v_str(&ctx.config.instance_id),
                v_i64(request.now),
                v_i64(request.now),
                v_i64(request.now + GATEWAY_RECEIPT_RETENTION_MS),
            ],
        )?;
        return Ok(ToolReceiptBegin::Accepted);
    };
    if receipt.args_hash != request.args_hash {
        return Ok(ToolReceiptBegin::Conflict);
    }
    if receipt.status == "completed" {
        let result = receipt.result.as_deref().map_or(Value::Null, |text| {
            serde_json::from_str(text).unwrap_or(Value::Null)
        });
        return Ok(ToolReceiptBegin::Replay { result });
    }
    if receipt.status == "prepared" && receipt.owner_instance == ctx.config.instance_id {
        return Ok(ToolReceiptBegin::InProgress {
            recovery: receipt
                .error_note
                .map(|error_note| ToolRecovery { error_note }),
        });
    }
    if receipt.status == "prepared" {
        ctx.run(
            "UPDATE receipts SET status = 'uncertain', error_note = COALESCE(error_note, ?), updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ?",
            &[
                v_str("the process that began this call ended before recording its outcome"),
                v_i64(request.now),
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
            ],
        )?;
    }
    let error_note = ctx
        .one(
            "SELECT error_note FROM receipts WHERE principal = ? AND operation = ? AND idempotency_key = ?",
            &[
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
            ],
        )?
        .and_then(|row| row_opt_str(&row, "error_note"));
    if !request.retry_uncertain {
        return Ok(ToolReceiptBegin::Uncertain { error_note });
    }
    ctx.run(
        "UPDATE receipts SET status = 'prepared', owner_instance = ?, updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ? AND status = 'uncertain'",
        &[
            v_str(&ctx.config.instance_id),
            v_i64(request.now),
            v_str(&scope.principal),
            v_str(&scope.operation),
            v_str(&scope.idempotency_key),
        ],
    )?;
    Ok(ToolReceiptBegin::Retry { error_note })
}

/// Settles this instance's own `prepared` receipt: completed with the call's result, or uncertain under its note (the
/// first cause is kept). Answers whether it settled one.
pub fn tool_receipt_settle(
    ctx: &StoreContext,
    request: &ToolReceiptSettleRequest,
) -> Result<bool, StoreError> {
    transaction(ctx, "tool_receipt_settle", || {
        let scope = &request.scope;
        let changed = match &request.settlement {
            ToolReceiptSettlement::Completed { result } => ctx.run(
                "UPDATE receipts SET status = 'completed', result = ?, updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ? AND status = 'prepared' AND owner_instance = ?",
                &[
                    v_str(result.to_string()),
                    v_i64(request.now),
                    v_str(&scope.principal),
                    v_str(&scope.operation),
                    v_str(&scope.idempotency_key),
                    v_str(&ctx.config.instance_id),
                ],
            )?,
            ToolReceiptSettlement::Uncertain { error_note } => ctx.run(
                "UPDATE receipts SET status = 'uncertain', error_note = COALESCE(error_note, ?), updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ? AND status = 'prepared' AND owner_instance = ?",
                &[
                    v_str(error_note),
                    v_i64(request.now),
                    v_str(&scope.principal),
                    v_str(&scope.operation),
                    v_str(&scope.idempotency_key),
                    v_str(&ctx.config.instance_id),
                ],
            )?,
        };
        Ok(changed == 1)
    })
}

/// Replaces a completed receipt's answer with what later became of the same call, so a retry under its key replays
/// that: a relayed steer the receiver held behind its open draft until the steered turn ended was answered `queued`,
/// and its receipt then answers the `turn_conflict` refusal. Only the instance that settled the receipt revises it.
pub fn tool_receipt_revise(
    ctx: &StoreContext,
    request: &ToolReceiptReviseRequest,
) -> Result<bool, StoreError> {
    transaction(ctx, "tool_receipt_revise", || {
        let scope = &request.scope;
        let changed = ctx.run(
            "UPDATE receipts SET result = ?, updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ? AND status = 'completed' AND owner_instance = ?",
            &[
                v_str(request.result.to_string()),
                v_i64(request.now),
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
                v_str(&ctx.config.instance_id),
            ],
        )?;
        Ok(changed == 1)
    })
}

/// Drops a `prepared` receipt this instance holds for a call that wrote nothing before it failed, so the next call
/// under the same key runs again instead of replaying that failure. A receipt another instance holds, one already
/// settled and a reserved retry of an uncertain call are left alone.
pub fn tool_receipt_release(
    ctx: &StoreContext,
    scope: &ToolReceiptScope,
) -> Result<bool, StoreError> {
    transaction(ctx, "tool_receipt_release", || {
        let changed = ctx.run(
            "DELETE FROM receipts WHERE principal = ? AND operation = ? AND idempotency_key = ? AND status = 'prepared' AND owner_instance = ? AND error_note IS NULL",
            &[
                v_str(&scope.principal),
                v_str(&scope.operation),
                v_str(&scope.idempotency_key),
                v_str(&ctx.config.instance_id),
            ],
        )?;
        Ok(changed == 1)
    })
}

/// A delivery to a session on another device, as its sender's receipt retained it: the key it was sent under and the
/// answer (a `RelayDeliverySuccess`, kept as the JSON the receipt stores).
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteReceipt {
    pub idempotency_key: String,
    pub result: Value,
}

/// The caller's completed remote send of `delivery_id`, when its receipt is still retained.
pub fn remote_receipt(
    ctx: &StoreContext,
    now: i64,
    principal: &str,
    delivery_id: &str,
) -> Result<Option<RemoteReceipt>, StoreError> {
    let found = ctx.one(
        "SELECT idempotency_key, result FROM receipts WHERE principal = ? AND operation = ? AND status = 'completed' AND expires_at > ? AND json_extract(result, '$.delivery_id') = ?",
        &[
            v_str(principal),
            v_str(REMOTE_SEND_OPERATION),
            v_i64(now),
            v_str(delivery_id),
        ],
    )?;
    Ok(found.and_then(|row| {
        let result = serde_json::from_str(&row_str(&row, "result")).ok()?;
        Some(RemoteReceipt {
            idempotency_key: row_str(&row, "idempotency_key"),
            result,
        })
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileRemoteReceiptRequest {
    pub now: i64,
    pub principal: String,
    pub idempotency_key: String,
    pub delivery_id: String,
    pub target_durable_id: String,
    pub device: String,
    pub outcome: ReceiptOutcome,
}

/// Records what the other device reported of a remote send on its retained receipt. A terminal fact already recorded
/// stays; anything else answers `outcome_unavailable` when the receipt is not this very send.
pub fn reconcile_remote_receipt(
    ctx: &StoreContext,
    request: &ReconcileRemoteReceiptRequest,
) -> Result<ReceiptOutcome, StoreError> {
    transaction(ctx, "reconcile_remote_receipt", || {
        let Some(prior) =
            remote_receipt(ctx, request.now, &request.principal, &request.delivery_id)?
        else {
            return Ok(ReceiptOutcome::OutcomeUnavailable);
        };
        let names = |key: &str| prior.result.get(key).and_then(Value::as_str);
        if prior.idempotency_key != request.idempotency_key
            || names("device") != Some(request.device.as_str())
            || names("thread_id") != Some(request.target_durable_id.as_str())
        {
            return Ok(ReceiptOutcome::OutcomeUnavailable);
        }
        let known = prior
            .result
            .get("outcome")
            .and_then(|value| serde_json::from_value::<ReceiptOutcome>(value.clone()).ok());
        if let Some(known @ (ReceiptOutcome::Applied | ReceiptOutcome::Refused { .. })) = known {
            return Ok(known);
        }
        let mut result = prior.result.clone();
        if let Some(object) = result.as_object_mut() {
            object.insert(
                "outcome".to_owned(),
                serde_json::to_value(request.outcome).unwrap_or(Value::Null),
            );
        }
        ctx.run(
            "UPDATE receipts SET result = ?, updated_at = ? WHERE principal = ? AND operation = ? AND idempotency_key = ?",
            &[
                v_str(result.to_string()),
                v_i64(request.now),
                v_str(&request.principal),
                v_str(REMOTE_SEND_OPERATION),
                v_str(&request.idempotency_key),
            ],
        )?;
        Ok(request.outcome)
    })
}
