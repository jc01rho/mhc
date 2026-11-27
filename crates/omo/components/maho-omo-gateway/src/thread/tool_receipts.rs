//! The tool-receipt wrapper of the thread tools: every tool but the two sends records its call under an idempotency key
//! before it runs and settles that record with its answer, so a retried call replays the answer instead of acting twice.
//!
//! Upstream counterpart: the `execute` wrapper inside `buildThreadTools` of `thread/tools.ts` with its receipt-recovery
//! bookkeeping (omo#14623 review H1, M4), the uncertain-create note (#14569, #14060) and `uncertainFailure`,
//! `uncertainNote` and `noteCause` of `thread/tools/internals.ts`. A send owns its idempotency in the gateway engine's
//! receipt, written in the delivery's own transaction, so `thread_send` and `thread_handoff` are not wrapped.
//!
//! The upstream wrapper parses a call's arguments before it admits the receipt, so an invalid call never leaves one. The
//! tool bodies here validate their own arguments, so a body that refuses with `invalid_arguments` releases the receipt it
//! was admitted under (`tool_receipt_release`): the same key then runs again with fixed arguments. A transport failure
//! that upstream throws (`failed`) arrives here as data carrying its message under [`THROWN_KEY`]: the receipt settles
//! uncertain under that message, as a throw's does.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Map, Value, json};

use super::adapter::BoxFuture;
use super::constants::GATEWAY_RECEIPT_RETENTION_MS;
use super::control::create::Retrying;
use super::control::operations::error_json;
use super::errors::{ThreadToolFailure, gateway_schema_refusal, thread_tool_failure};
use super::store_api::GatewayStoreApi;
use super::store_ops::{StoreError, hash_args};
use super::store_tool_receipts::{
    ToolReceiptBegin, ToolReceiptBeginRequest, ToolReceiptScope, ToolReceiptSettleRequest,
    ToolReceiptSettlement,
};
use super::thread_row::current_result;

/// How many keys whose admission failed without a reply may wait for their recovery at once: when full, a call under a new
/// key is refused before its admission, so no key that may still need recovering is ever dropped.
pub const RECEIPT_RECOVERY_MAX_KEYS: usize = 4_096;

/// The top-level key a transport failure answer carries its original message under until [`ToolReceipts::execute`] takes
/// it off: the wrapper settles such a call's receipt uncertain, as a call that threw.
pub(crate) const THROWN_KEY: &str = "thrown";

/// The four store calls the wrapper makes, behind one seam so a test can lose a reply after the store committed it.
pub trait ToolReceiptStore: Send + Sync {
    /// The clock every receipt is stamped and expired against.
    fn receipt_now(&self) -> i64;
    fn begin(&self, request: &ToolReceiptBeginRequest) -> Result<ToolReceiptBegin, StoreError>;
    fn settle(&self, request: &ToolReceiptSettleRequest) -> Result<bool, StoreError>;
    fn release(&self, scope: &ToolReceiptScope) -> Result<bool, StoreError>;
}

impl ToolReceiptStore for Arc<dyn GatewayStoreApi> {
    fn receipt_now(&self) -> i64 {
        GatewayStoreApi::now(&**self)
    }

    fn begin(&self, request: &ToolReceiptBeginRequest) -> Result<ToolReceiptBegin, StoreError> {
        self.tool_receipt_begin(request)
    }

    fn settle(&self, request: &ToolReceiptSettleRequest) -> Result<bool, StoreError> {
        self.tool_receipt_settle(request)
    }

    fn release(&self, scope: &ToolReceiptScope) -> Result<bool, StoreError> {
        self.tool_receipt_release(scope)
    }
}

/// What this process knows about the receipts it began: the calls still running, the ones it could not settle, and the
/// keys whose admission failed without a reply.
#[derive(Default)]
struct Ledger {
    /// Receipts this process began but could not settle (the store gave up at its lock-wait bound): the row stays
    /// `prepared` under this instance, which would answer `idempotency_in_progress` forever. A retry of such a key is
    /// `idempotency_uncertain` with the note instead.
    unsettled: HashMap<String, String>,
    /// Keys whose receipt admission failed without a reply: the store may still have committed the prepared row (its
    /// worker exited after the commit), which this instance's retry would read as `in_progress` forever. Nothing ran for
    /// such a call, so a retry that finds that row while no invocation of this surface is running the key takes it up and
    /// runs the call. Each key is kept until the receipt it may have left expires (admission time + the receipt
    /// retention), at most [`RECEIPT_RECOVERY_MAX_KEYS`] at once.
    unanswered: HashMap<String, i64>,
    /// Keys whose admission is awaiting the store, with how many calls await it: each may become a recovery key, so it
    /// holds a slot from before its admission until the store answers (or the failure leaves it in `unanswered`).
    /// Overlapping calls under one key share one slot.
    admitting: HashMap<String, usize>,
    running: HashSet<String>,
}

impl Ledger {
    fn held_slots(&self) -> usize {
        let admitting_only = self
            .admitting
            .keys()
            .filter(|key| !self.unanswered.contains_key(*key))
            .count();
        self.unanswered.len() + admitting_only
    }

    fn recoverable(&mut self, key: &str, at: i64) -> bool {
        let expired = self.unanswered.get(key).is_some_and(|until| *until <= at);
        if expired {
            self.unanswered.remove(key);
        }
        self.unanswered.contains_key(key)
    }

    fn recovery_full(&mut self, at: i64, max_keys: usize) -> bool {
        if self.held_slots() < max_keys {
            return false;
        }
        self.unanswered.retain(|_, until| *until > at);
        self.held_slots() >= max_keys
    }

    fn release_admitting(&mut self, key: &str) {
        let calls = self.admitting.get(key).copied().unwrap_or(0);
        if calls <= 1 {
            self.admitting.remove(key);
        } else {
            self.admitting.insert(key.to_owned(), calls - 1);
        }
    }
}

enum Admission {
    Answer(Value),
    WithoutReceipt,
    Run(Run),
}

struct Run {
    scope: ToolReceiptScope,
    key: String,
    retrying: Option<Retrying>,
}

/// One tool surface's receipts.
pub struct ToolReceipts {
    store: Arc<dyn ToolReceiptStore>,
    max_keys: usize,
    ledger: Mutex<Ledger>,
}

fn failure(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    details: Option<Value>,
) -> Value {
    error_json(thread_tool_failure(code, message, next_action, details))
}

fn receipt_key(scope: &ToolReceiptScope) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        scope.principal, scope.operation, scope.idempotency_key
    )
}

fn is_ok(result: &Value) -> bool {
    result.get("kind").and_then(Value::as_str) == Some("ok")
}

fn error_code(result: &Value) -> Option<&str> {
    if result.get("kind").and_then(Value::as_str) != Some("error") {
        return None;
    }
    result.get("error")?.get("code")?.as_str()
}

/// A transport failure answer that still carries the message it was thrown with.
pub(crate) fn thrown(mut answer: Value, message: &str) -> Value {
    if let Some(object) = answer.as_object_mut() {
        object.insert(THROWN_KEY.to_owned(), Value::String(message.to_owned()));
    }
    answer
}

fn take_thrown(result: &mut Value) -> Option<String> {
    let message = result.as_object_mut()?.remove(THROWN_KEY)?;
    message.as_str().map(str::to_owned)
}

/// The refusal a gateway store newer than this build answers with. `StoreError::Schema` carries only the message
/// `GatewaySchemaVersionError` renders ("Gateway schema 17 is newer than this binary's supported version 16."), so its
/// two versions are read back from it.
pub(crate) fn schema_refusal(error: &StoreError) -> Option<ThreadToolFailure> {
    let StoreError::Schema(message) = error else {
        return None;
    };
    let versions: Vec<i64> = message
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect();
    match versions.as_slice() {
        [found, supported, ..] => Some(gateway_schema_refusal(*found, *supported)),
        _ => None,
    }
}

/// The tools a store newer than this build still answers without a receipt: a read that changes nothing and whose answer
/// survives the missing backlog. The listing names why its backlog is absent (`read_ops.rs` `with_backlog`) instead of
/// refusing, so the receipt must not turn that store's refusal into the call's answer.
fn receipt_optional(name: &str) -> bool {
    name == "thread_list"
}

/// The receipt note of a `thread_create` left uncertain (#14569): its first uncertain answer's message and details (the
/// reason, a worktree it kept, a cleanup), so a retry answers that cause and can finish what the first run made.
pub fn uncertain_note(result: &Value) -> String {
    let error = result.get("error");
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut note = Map::new();
    note.insert("message".to_owned(), Value::String(message.to_owned()));
    if let Some(details) = error
        .and_then(|error| error.get("details"))
        .filter(|details| !details.is_null())
    {
        note.insert("details".to_owned(), details.clone());
    }
    Value::Object(note).to_string()
}

/// A receipt note as [`uncertain_note`] wrote it; any other note is its own message, with no details.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteCause {
    pub message: String,
    pub details: Option<Map<String, Value>>,
}

pub fn note_cause(note: &str) -> NoteCause {
    if note.starts_with('{')
        && let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(note)
        && let Some(message) = parsed.get("message").and_then(Value::as_str)
    {
        let details = match parsed.get("details") {
            Some(Value::Object(details)) => Some(details.clone()),
            _ => None,
        };
        return NoteCause {
            message: message.to_owned(),
            details,
        };
    }
    NoteCause {
        message: note.to_owned(),
        details: None,
    }
}

/// The answer to an operation whose earlier run under the same key may have acted: read before retrying.
pub fn uncertain_failure(note: Option<&str>) -> Value {
    let details = note.map(|note| {
        let cause = note_cause(note);
        let mut details = Map::new();
        details.insert("error_note".to_owned(), Value::String(cause.message));
        if let Some(first) = cause.details {
            details.insert("first_cause".to_owned(), Value::Object(first));
        }
        Value::Object(details)
    });
    failure(
        "idempotency_uncertain",
        "The earlier operation may have been delivered.",
        "Read the target transcript before deciding whether to retry.",
        details,
    )
}

impl ToolReceipts {
    pub fn new(store: Arc<dyn ToolReceiptStore>) -> Arc<Self> {
        Self::with_recovery_bound(store, RECEIPT_RECOVERY_MAX_KEYS)
    }

    /// The same surface keeping at most `max_keys` waiting recovery keys, so a test can reach the bound without filling
    /// four thousand of them.
    pub fn with_recovery_bound(store: Arc<dyn ToolReceiptStore>, max_keys: usize) -> Arc<Self> {
        Arc::new(Self {
            store,
            max_keys,
            ledger: Mutex::new(Ledger::default()),
        })
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `body` as the receipted call `name` of `caller_id`: admitted once under its idempotency key (`idempotency_key`
    /// in `args`, else `call:<call_id>`), settled with its answer. `body` takes the earlier uncertain run it resumes, set
    /// only for a `thread_create` retried under a key whose first run may have made its thread (#14060).
    pub async fn execute<F>(
        &self,
        name: &str,
        call_id: &str,
        caller_id: &str,
        args: Value,
        body: F,
    ) -> Value
    where
        F: FnOnce(Option<Retrying>) -> BoxFuture<Value> + Send,
    {
        // Querying the outcome of an earlier delivery reads no conversation and writes no receipt of its own.
        if name == "thread_read"
            && args.get("delivery_id").is_some_and(Value::is_string)
            && args.get("thread").is_some_and(Value::is_string)
        {
            let mut answer = body(None).await;
            take_thrown(&mut answer);
            return answer;
        }
        let run = match self.admit(name, call_id, caller_id, &args) {
            Admission::Answer(answer) => return answer,
            // A listing a store newer than this build still answers runs unrecorded: the backlog is an optional detail
            // of the listing and a receipt only replays an answer that a read never changes.
            Admission::WithoutReceipt => {
                let mut answer = body(None).await;
                take_thrown(&mut answer);
                return answer;
            }
            Admission::Run(run) => run,
        };
        self.ledger().running.insert(run.key.clone());
        let retrying = run.retrying.clone();
        let mut result = body(run.retrying.clone()).await;
        let thrown = take_thrown(&mut result);
        self.settle(name, &run, retrying.as_ref(), &result, thrown);
        self.ledger().running.remove(&run.key);
        result
    }

    fn admit(&self, name: &str, call_id: &str, caller_id: &str, args: &Value) -> Admission {
        let explicit = args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty());
        let scope = ToolReceiptScope {
            principal: format!("session:{caller_id}"),
            operation: name.to_owned(),
            idempotency_key: explicit.map_or_else(|| format!("call:{call_id}"), str::to_owned),
        };
        let key = receipt_key(&scope);
        let at = self.store.receipt_now();
        {
            // The slot is taken before the admission, so overlapping calls cannot all pass the check.
            let mut ledger = self.ledger();
            if !ledger.recoverable(&key, at)
                && !ledger.admitting.contains_key(&key)
                && ledger.recovery_full(at, self.max_keys)
            {
                return Admission::Answer(failure(
                    "overloaded",
                    format!(
                        "{} earlier calls on this surface failed before the gateway store answered and may still need recovering; nothing ran.",
                        self.max_keys
                    ),
                    "Retry those calls under their own idempotency keys, then retry this one.",
                    Some(json!({
                        "budget": "receipt_recovery",
                        "max_keys": self.max_keys,
                    })),
                ));
            }
            *ledger.admitting.entry(key.clone()).or_insert(0) += 1;
        }
        let begun = self.store.begin(&ToolReceiptBeginRequest {
            scope: scope.clone(),
            now: at,
            args_hash: hash_args(args),
            retry_uncertain: name == "thread_create",
        });
        let mut admission = match begun {
            Ok(admission) => {
                self.ledger().release_admitting(&key);
                admission
            }
            Err(error) => {
                // Nothing has run yet, so a store that cannot admit the receipt fails the call as data: the store lock
                // held past its wait bound is `overloaded`, a store newer than this build its schema refusal, any other
                // failure `internal_error`.
                let message = error.message();
                let answer = if error.is_lock_wait_exceeded() {
                    // The store gave up waiting for its lock, but its worker may still commit the row it was writing: the
                    // key holds a recovery slot exactly as a lost reply does.
                    self.ledger()
                        .unanswered
                        .insert(key.clone(), at + GATEWAY_RECEIPT_RETENTION_MS);
                    failure(
                        "overloaded",
                        format!(
                            "The gateway store is locked by another process; nothing ran: {message}"
                        ),
                        "Wait a few seconds, then retry the call.",
                        None,
                    )
                } else if let Some(refusal) = schema_refusal(&error) {
                    if receipt_optional(name) {
                        self.ledger().release_admitting(&key);
                        return Admission::WithoutReceipt;
                    }
                    error_json(refusal)
                } else {
                    // The slot passes to the key's recovery entry when the failure left one.
                    self.ledger()
                        .unanswered
                        .insert(key.clone(), at + GATEWAY_RECEIPT_RETENTION_MS);
                    failure(
                        "internal_error",
                        format!(
                            "The gateway store could not record the call; nothing ran: {message}"
                        ),
                        "Retry the call; if it keeps failing, check the gateway store.",
                        None,
                    )
                };
                self.ledger().release_admitting(&key);
                return Admission::Answer(answer);
            }
        };
        {
            // The store committed this key's reservation for a call whose reply was lost, so nothing ran under it: the
            // call resumes as the reservation it found, a recovery keeping its first cause (#14623 review round 2 H1).
            let mut ledger = self.ledger();
            let resumes = matches!(admission, ToolReceiptBegin::InProgress { .. })
                && ledger.recoverable(&key, at)
                && !ledger.running.contains(&key)
                && !ledger.unsettled.contains_key(&key);
            if resumes {
                admission = match admission {
                    ToolReceiptBegin::InProgress { recovery: None } => ToolReceiptBegin::Accepted,
                    ToolReceiptBegin::InProgress {
                        recovery: Some(recovery),
                    } => ToolReceiptBegin::Retry {
                        error_note: Some(recovery.error_note),
                    },
                    other => other,
                };
            }
            ledger.unanswered.remove(&key);
        }
        match admission {
            ToolReceiptBegin::Replay { result } => {
                let mut result = current_result(result);
                if name == "thread_create"
                    && is_ok(&result)
                    && let Some(object) = result.as_object_mut()
                {
                    object.insert("deduplicated".to_owned(), Value::Bool(true));
                }
                Admission::Answer(result)
            }
            ToolReceiptBegin::Conflict => Admission::Answer(failure(
                "idempotency_conflict",
                "The idempotency key was already used with different arguments.",
                "Retry with a new idempotency_key.",
                None,
            )),
            ToolReceiptBegin::InProgress { .. } => {
                let note = self.ledger().unsettled.get(&key).cloned();
                Admission::Answer(match note {
                    Some(note) => failure(
                        "idempotency_uncertain",
                        "The earlier operation may have been delivered.",
                        "Read the target transcript before deciding whether to retry.",
                        Some(json!({ "error_note": note })),
                    ),
                    None => failure(
                        "idempotency_in_progress",
                        "The same operation is already in progress.",
                        "Wait for the earlier call to settle, then retry.",
                        None,
                    ),
                })
            }
            // #14060: a thread_create that failed after the host made its thread may recover by re-running it; the
            // create itself refuses unless it finds that thread. The store reserved the receipt for this retry alone
            // (#14623 H1), so its answer settles it like a new call's.
            ToolReceiptBegin::Uncertain { error_note } => {
                Admission::Answer(uncertain_failure(error_note.as_deref()))
            }
            ToolReceiptBegin::Retry { error_note } => Admission::Run(Run {
                scope,
                key,
                retrying: Some(Retrying { error_note }),
            }),
            ToolReceiptBegin::Accepted => Admission::Run(Run {
                scope,
                key,
                retrying: None,
            }),
        }
    }

    fn settle(
        &self,
        name: &str,
        run: &Run,
        retrying: Option<&Retrying>,
        result: &Value,
        thrown: Option<String>,
    ) {
        if thrown.is_none() && retrying.is_none() && error_code(result) == Some("invalid_arguments")
        {
            // Refused before anything ran: no answer to keep, and the key may be used again with fixed arguments.
            if let Err(error) = self.store.release(&run.scope) {
                self.unrecorded(&run.key, "the call was refused", &error.message());
            }
            return;
        }
        // A create that may have made its thread (its open's reply was lost) has no answer yet: its receipt stays
        // uncertain, so a retry under the key runs the create again, which finds that thread or answers uncertain again
        // (#14060). The note keeps the first uncertain answer's cause (#14569); a retry that fails in any way has not
        // settled what the first run made, so its receipt stays uncertain under that same note and its error is never
        // replayed as final.
        let undecided = (name == "thread_create"
            && error_code(result).is_some()
            && (error_code(result) == Some("idempotency_uncertain") || retrying.is_some()))
        .then(|| {
            retrying
                .and_then(|retrying| retrying.error_note.clone())
                .unwrap_or_else(|| uncertain_note(result))
        });
        let settlement = match (thrown, undecided) {
            (Some(message), _) => ToolReceiptSettlement::Uncertain {
                error_note: message,
            },
            (None, Some(note)) => ToolReceiptSettlement::Uncertain { error_note: note },
            (None, None) => ToolReceiptSettlement::Completed {
                result: result.clone(),
            },
        };
        let settled = self.store.settle(&ToolReceiptSettleRequest {
            scope: run.scope.clone(),
            now: self.store.receipt_now(),
            settlement,
        });
        match settled {
            Ok(true) => {}
            Ok(false) => self.unrecorded(
                &run.key,
                "the call ran",
                "this call no longer held its receipt",
            ),
            Err(error) => self.unrecorded(&run.key, "the call ran", &error.message()),
        }
    }

    fn unrecorded(&self, key: &str, what: &str, why: &str) {
        self.ledger().unsettled.insert(
            key.to_owned(),
            format!("{what}, and its receipt could not be recorded: {why}"),
        );
    }
}
