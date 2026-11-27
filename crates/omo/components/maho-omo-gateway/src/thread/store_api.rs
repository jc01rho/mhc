//! The gateway store facade's API.
//!
//! Upstream counterpart: `thread/gateway/store-api.ts`. Every call is synchronous: the store owns one
//! connection and the worker protocol serializes calls onto it.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::backlog::TargetBacklog;
use super::store_extensions::{
    DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_ops::{DeliveryView, StoreError};
use super::types::{
    ClaimOutcome, ClaimRequest, DeliveryRow, EnqueueOutcome, EnqueueRequest, GatewayStoreStats,
    ProcessIdentity, ReconcileOutcome, ReconcileRequest, RecordOutcomeRequest, RefusalReason,
};

pub type ExtensionErrorListener = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub trait GatewayStoreApi: Send + Sync {
    fn on_after_commit_error(&self, _listener: ExtensionErrorListener) {}
    /// The store's busy timeout: the delay before a caller re-arms an operation that failed with a lock-wait error.
    fn busy_timeout_ms(&self) -> u64;
    /// The clock this store's rows are stamped and expired against.
    fn now(&self) -> i64;
    fn identity(&self) -> Result<ProcessIdentity, StoreError>;
    fn enqueue(&self, request: &EnqueueRequest) -> Result<EnqueueOutcome, StoreError>;
    fn reconcile(&self, request: &ReconcileRequest) -> Result<ReconcileOutcome, StoreError>;
    fn claim(&self, request: &ClaimRequest) -> Result<ClaimOutcome, StoreError>;
    fn record_outcome(&self, request: &RecordOutcomeRequest) -> Result<ClaimOutcome, StoreError>;
    fn refuse_queued(
        &self,
        now: i64,
        delivery_id: &str,
        reason: RefusalReason,
    ) -> Result<bool, StoreError>;
    /// A delivery its session took in and handed back unread at a graceful close (omo#14313); never an applied row.
    fn refuse_returned(
        &self,
        now: i64,
        delivery_id: &str,
        reason: RefusalReason,
    ) -> Result<bool, StoreError>;
    fn complete_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        result: &Value,
    ) -> Result<bool, StoreError>;
    fn abandon_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        error_note: &str,
    ) -> Result<bool, StoreError>;
    fn delivery_view(&self, delivery_id: &str) -> Result<Option<DeliveryView>, StoreError>;
    fn list(
        &self,
        target_durable_id: Option<&str>,
        root_id: Option<&str>,
    ) -> Result<Vec<DeliveryRow>, StoreError>;
    fn is_referenced(&self, durable_id: &str) -> Result<bool, StoreError>;
    /// Each session's undelivered deliveries (omo#14428), keyed by durable id; a session holding none is absent.
    fn backlogs(
        &self,
        durable_ids: &[String],
    ) -> Result<BTreeMap<String, TargetBacklog>, StoreError>;
    fn journal_mode(&self) -> Result<String, StoreError>;
    fn stats(&self) -> GatewayStoreStats;

    fn register_incarnation(
        &self,
        durable_id: &str,
        incarnation: &str,
        endpoint_socket: Option<&str>,
        endpoint_kind: Option<&str>,
    ) -> Result<(), StoreError>;
    fn clear_endpoint(&self, durable_id: &str, incarnation: &str) -> Result<(), StoreError>;
    fn session_endpoint(
        &self,
        durable_id: &str,
    ) -> Result<Option<(String, String, String)>, StoreError>;

    fn register_store_extension(
        &self,
        descriptor: &StoreExtensionRegistration,
        now: i64,
        restored_at: Option<i64>,
    ) -> StoreExtensionResult<i64>;
    fn extension_call(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        now: i64,
    ) -> StoreExtensionResult<Value>;
    fn session_callable_ops(&self) -> Vec<DeclaredSessionOp>;
    fn extension_session_await(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
        now: i64,
    ) -> StoreExtensionResult<Value>;
    fn record_thread_creation(
        &self,
        record: &super::store_extension_facade::ThreadCreationRecord,
        now: i64,
    ) -> Result<(), StoreError>;
}

pub type SharedGatewayStore = Arc<dyn GatewayStoreApi>;
