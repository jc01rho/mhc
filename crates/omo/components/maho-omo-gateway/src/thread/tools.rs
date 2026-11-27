//! The thread tools: what a model calls to reach another session.
//!
//! Upstream counterpart: `thread/tools.ts`. Every failure is data (`{ kind: "error", error }`), and a
//! session op's tool is its own namespace (`ext_<extension>_<toolName>`).

pub type ExecuteCallback = Arc<dyn Fn(&str, Value) -> BoxFuture<Value> + Send + Sync>;
pub type ThreadSendAs = Arc<dyn Fn(String, Value) -> BoxFuture<Value> + Send + Sync>;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::adapter::BoxFuture;
use super::control::locate::RemoteControlLocator;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::thread_tool_failure;
use super::host_view::ThreadHost;
use super::relay::Keyed;
use super::store_api::GatewayStoreApi;
use super::store_extensions::{DeclaredSessionOp, SessionCaller, StoreExtensionResult};
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender};

#[derive(Clone)]
pub struct ThreadTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub execute: ExecuteCallback,
}

impl std::fmt::Debug for ThreadTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThreadTool")
            .field("name", &self.name)
            .finish()
    }
}

pub struct ThreadToolsOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    /// The calling session's durable id, resolved by the engine, never taken from a tool argument.
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    /// The caller's own provenance, resolved in its runtime.
    pub caller_provenance: Arc<dyn Fn() -> super::types::Provenance + Send + Sync>,
    /// The host surface the session-control tools read (`control_tools.rs`); without one they answer
    /// `host_unavailable` as data.
    pub host: Option<Arc<dyn ThreadHost>>,
    /// The caller's workspace root: the default scope of every address.
    pub caller_workspace_root: Arc<dyn Fn() -> String + Send + Sync>,
    /// `<agentDir>/sessions`: threads no endpoint lists are listed and read from their session files.
    pub sessions_directory: Option<PathBuf>,
    /// The account's other devices, asked for an address this device's book does not name.
    pub remote: Option<Arc<dyn RemoteControlLocator>>,
}

fn error_result(error: impl serde::Serialize) -> Value {
    json!({ "kind": "error", "error": error })
}

fn ok_result(value: Value) -> Value {
    let mut result = value;
    if let Some(object) = result.as_object_mut() {
        object.insert("kind".into(), Value::String("ok".into()));
    }
    result
}

fn invalid_arguments(message: impl Into<String>) -> Value {
    error_result(thread_tool_failure(
        "invalid_arguments",
        message,
        "Fix the arguments and call again.",
        None,
    ))
}

fn delivery_result(result: GatewayDeliveryResult, link: String) -> Value {
    match result {
        GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            delivery,
            effective_mode,
            endpoint_kind: _,
            deduplicated,
            collapsed,
        } => {
            let mut result = json!({
                "thread": thread_id,
                "link": link,
                "delivery": delivery,
                "message_seq": message_seq,
                "deduplicated": deduplicated,
                "delivery_id": delivery_id,
                "effective_mode": effective_mode,
            });
            // A collapse-key repeat the target already held an earlier message for: this text was not delivered
            // (omo#14431).
            if collapsed && let Some(object) = result.as_object_mut() {
                object.insert("collapsed".into(), Value::Bool(true));
            }
            ok_result(result)
        }
        GatewayDeliveryResult::Error { error } => error_result(error),
    }
}

fn thread_link(thread_id: &str, title: &str) -> String {
    super::contracts_results::thread_link(thread_id, title)
}

/// `CollapseKey` (omo#14428, #14431): the caller-chosen key a repeating message is sent under.
fn collapse_key_parameter() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 200,
        "description": "Caller-chosen key for a repeating message (an alert re-sent each minute): a send replaces this sender's earlier message with the same key that the target has not taken yet, and is answered deduplicated and collapsed: true with the earlier delivery_id when the target already holds that one (this text is then not delivered), so one key never leaves more than one undelivered message. Reaches sessions on this device only.",
    })
}

pub(crate) async fn deliver_thread_send(
    options: &ThreadToolsOptions,
    caller_id: String,
    caller_name: Option<String>,
    call_id: &str,
    args: Value,
) -> Value {
    let Some(thread) = args.get("thread").and_then(Value::as_str) else {
        return invalid_arguments("thread must be a non-empty string");
    };
    let Some(message) = args.get("message").and_then(Value::as_str) else {
        return invalid_arguments("message must be a non-empty string");
    };
    let mode = match args.get("delivery").and_then(Value::as_str) {
        Some(mode) => match GatewayDeliveryMode::parse(mode) {
            Some(mode) => Some(mode),
            None => {
                return invalid_arguments("delivery must be auto, steer or follow_up");
            }
        },
        None => None,
    };
    let sender = GatewaySender::Session {
        durable_id: caller_id,
        name: caller_name,
        turn_id: None,
        cause_delivery_id: None,
    };
    let provenance = (options.caller_provenance)();
    let provenance = Arc::new(provenance);
    let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
        sender_provenance: Some(Arc::new(move || (*provenance).clone())),
        store: options.store.clone(),
        endpoints: options.engine.options().endpoints.clone(),
        resolve: options.engine.options().resolve.clone(),
        now: options.engine.options().now.clone(),
    });
    let mut request = GatewayDeliverRequest::new(sender, thread, message);
    request.mode = mode;
    request.expected_turn_id = args.get("expected_turn_id").and_then(Value::as_i64);
    request.all_scope = args
        .get("all_scope")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    request.idempotency_key = Some(
        args.get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| call_id.to_owned()),
    );
    request.collapse_key = args
        .get("collapse_key")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let result = engine.deliver(request).await;
    delivery_result(result, thread_link(thread, thread))
}

pub fn create_thread_send_as(options: Arc<ThreadToolsOptions>) -> ThreadSendAs {
    Arc::new(move |caller_id: String, args: Value| {
        let options = options.clone();
        Box::pin(async move {
            let key = args
                .get("idempotency_key")
                .and_then(Value::as_str)
                .unwrap_or(&caller_id);
            let call_id = format!("send-as:{key}");
            let caller_name = if caller_id == (options.caller_session_id)() {
                options.caller_name.as_ref().and_then(|name| name())
            } else {
                None
            };
            let mut forwarded = args;
            if let Some(obj) = forwarded.as_object_mut() {
                obj.insert("idempotency_key".into(), Value::String(call_id.clone()));
            }
            deliver_thread_send(&options, caller_id, caller_name, &call_id, forwarded).await
        })
    })
}

/// Every thread tool this component owns. `thread_list` and `thread_read` read the address book and a
/// transcript; they are registered by the component that owns those modules.
pub fn create_thread_tools(options: Arc<ThreadToolsOptions>) -> Vec<ThreadTool> {
    let mut tools = Vec::new();

    for (name, description) in [
        (
            "thread_send",
            "Send a message to another thread and report how it was delivered.",
        ),
        (
            "thread_handoff",
            "Hand work to another thread, resolving its address first.",
        ),
    ] {
        let options = options.clone();
        let mut parameters = json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string", "minLength": 1 },
                "message": { "type": "string", "minLength": 1 },
                "delivery": { "type": "string", "enum": ["auto", "steer", "follow_up"] },
                "expected_turn_id": { "type": "integer" },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" },
            },
            "required": ["thread", "message"],
            "additionalProperties": false,
        });
        // omo#14428: only thread_send takes a collapse key.
        if name == "thread_send"
            && let Some(properties) = parameters
                .get_mut("properties")
                .and_then(Value::as_object_mut)
        {
            properties.insert("collapse_key".into(), collapse_key_parameter());
        }
        tools.push(ThreadTool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters,
            execute: Arc::new(move |call_id: &str, args: Value| {
                let options = options.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let caller_id = (options.caller_session_id)();
                    let caller_name = options.caller_name.as_ref().and_then(|name| name());
                    deliver_thread_send(&options, caller_id, caller_name, &call_id, args).await
                })
            }),
        });
    }

    tools.extend(super::control_tools::create_control_tools(&options));
    tools.push(super::create_tool::thread_create_tool(&options));
    tools
}

/// The tools a session's registered extension ops add: `ext_<extension>_<toolName>`, each waiting on
/// its declared await.
pub type CallerResolver = Arc<dyn Fn(String) -> BoxFuture<Option<String>> + Send + Sync>;

/// Resolve only an exact durable address-book entry. A name/routing alias must never become a caller.
pub async fn resolve_caller_durable_id(
    resolve: &super::engine::GatewayResolve,
    runtime_id: &str,
) -> Option<String> {
    if runtime_id.is_empty() || runtime_id == "unknown" {
        return None;
    }
    match resolve(runtime_id, true).await {
        super::engine::GatewayResolution::Ok(target) if target.durable_id == runtime_id => {
            Some(target.durable_id)
        }
        _ => None,
    }
}

/// Compatibility surface without an address book fails closed; production uses the resolver below.
pub fn register_extension_tools(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
) -> Vec<ThreadTool> {
    build_extension_tools(store, caller, Arc::new(|_| Box::pin(async { None })))
}

pub fn build_extension_tools(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
    resolve: CallerResolver,
) -> Vec<ThreadTool> {
    store
        .session_callable_ops()
        .into_iter()
        .map(|declared| extension_tool(store.clone(), caller.clone(), resolve.clone(), declared))
        .collect()
}

fn extension_tool(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
    resolve: CallerResolver,
    declared: DeclaredSessionOp,
) -> ThreadTool {
    ThreadTool {
        name: declared.registered_name,
        description: declared.entry.description.unwrap_or_default(),
        parameters: declared.entry.parameters,
        execute: Arc::new(move |_call_id: &str, args: Value| {
            let (store, caller, resolve, extension, op) = (
                store.clone(),
                caller.clone(),
                resolve.clone(),
                declared.extension.clone(),
                declared.entry.op.clone(),
            );
            Box::pin(async move {
                let result = match resolve(caller()).await {
                    Some(caller_durable_id) => store.extension_session_await(
                        &extension,
                        &op,
                        &args,
                        &SessionCaller { caller_durable_id },
                        store.now(),
                    ),
                    None => StoreExtensionResult::refused(
                        super::store_extensions::StoreExtensionRefusalCode::CallerContextMissing,
                        "The calling session's durable id is not in the thread address book; the op was not run.",
                    ),
                };
                match result {
                    StoreExtensionResult::Ok { value } => json!({"kind":"ok", "value":value}),
                    StoreExtensionResult::Refused { code, message } => {
                        json!({"kind":"refused", "code":code.as_str(), "message":message})
                    }
                }
            })
        }),
    }
}

pub async fn record_thread_creator(
    store: &dyn GatewayStoreApi,
    resolve: &super::engine::GatewayResolve,
    runtime_id: &str,
    created: &Value,
) -> Result<(), super::store_ops::StoreError> {
    if created["kind"] != "ok" {
        return Ok(());
    }
    let Some(created_id) = created["thread"]["thread_id"].as_str() else {
        return Ok(());
    };
    if let Some(creator) = resolve_caller_durable_id(resolve, runtime_id).await
        && creator != created_id
    {
        store.record_thread_creation(
            &super::store_extension_facade::ThreadCreationRecord {
                creator_durable_id: creator,
                created_durable_id: created_id.to_owned(),
            },
            store.now(),
        )?;
    }
    Ok(())
}

/// Registry state persists across session starts; duplicate composed names skip every declaration.
#[derive(Default)]
pub struct ExtensionToolRegistration {
    mine: std::collections::HashSet<String>,
    reported: std::collections::HashSet<String>,
}
impl ExtensionToolRegistration {
    pub fn session_start(
        &mut self,
        tools: Vec<ThreadTool>,
        present: &[String],
        log: &dyn Fn(&str),
    ) -> Vec<ThreadTool> {
        let mut counts = std::collections::HashMap::new();
        for tool in &tools {
            *counts.entry(tool.name.clone()).or_insert(0usize) += 1;
        }
        tools
            .into_iter()
            .filter(|tool| {
                let why = if counts[&tool.name] > 1 {
                    Some("more than one extension declares it")
                } else if present.contains(&tool.name) && !self.mine.contains(&tool.name) {
                    Some("the session already has a tool by that name")
                } else {
                    None
                };
                if let Some(why) = why {
                    if self.reported.insert(tool.name.clone()) {
                        log(&format!(
                            "thread gateway: extension session tool {} was not registered: {why}",
                            super::remote::ports::escape_for_display(&tool.name)
                        ));
                    }
                    false
                } else {
                    self.mine.insert(tool.name.clone());
                    true
                }
            })
            .collect()
    }
}

/// The key a tool call's receipt is recorded under.
pub fn call_key(principal: &str, call_id: &str) -> Keyed {
    Keyed {
        principal: principal.to_owned(),
        idempotency_key: Some(call_id.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caller_identity_must_equal_address_book_durable_id() {
        let resolver: super::super::engine::GatewayResolve = Arc::new(|_, _| {
            Box::pin(async {
                super::super::engine::GatewayResolution::Ok(super::super::engine::GatewayTarget {
                    durable_id: "durable".into(),
                    endpoint: None,
                    liveness: super::super::adapter::EndpointLiveness::Dead,
                })
            })
        });
        assert_eq!(
            resolve_caller_durable_id(&resolver, "durable")
                .await
                .as_deref(),
            Some("durable")
        );
        for id in ["", "unknown", "rpc-1", "alias"] {
            assert_eq!(resolve_caller_durable_id(&resolver, id).await, None);
        }
    }

    #[test]
    fn colliding_declarations_and_non_owned_tools_never_replace() {
        let tool = |name: &str| ThreadTool {
            name: name.into(),
            description: String::new(),
            parameters: json!({}),
            execute: Arc::new(|_, _| Box::pin(async { Value::Null })),
        };
        let mut registry = ExtensionToolRegistration::default();
        let logs = std::sync::Mutex::new(Vec::new());
        let log = |line: &str| logs.lock().unwrap().push(line.to_owned());
        for _ in 0..2 {
            let accepted = registry.session_start(
                vec![tool("same"), tool("same"), tool("builtin"), tool("ours")],
                &["builtin".into()],
                &log,
            );
            assert_eq!(
                accepted
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["ours"]
            );
        }
        assert_eq!(logs.lock().unwrap().len(), 2);
        assert_eq!(
            registry
                .session_start(vec![tool("ours")], &["ours".into()], &log)
                .len(),
            1
        );
    }

    #[test]
    fn a_skipped_extension_tool_is_logged_with_its_name_escaped() {
        let tool = |name: &str| ThreadTool {
            name: name.into(),
            description: String::new(),
            parameters: json!({}),
            execute: Arc::new(|_, _| Box::pin(async { Value::Null })),
        };
        let mut registry = ExtensionToolRegistration::default();
        let logs = std::sync::Mutex::new(Vec::new());
        let log = |line: &str| logs.lock().unwrap().push(line.to_owned());
        let unsafe_name = "ext_a\u{1b}[31m\u{202e}b";
        let accepted = registry.session_start(
            vec![
                tool(unsafe_name),
                tool(unsafe_name),
                tool("ext_plain_zz_open"),
            ],
            &["ext_plain_zz_open".into()],
            &log,
        );
        assert!(accepted.is_empty());
        let logs = logs.into_inner().unwrap();
        assert_eq!(logs.len(), 2);
        for line in &logs {
            assert!(
                !line
                    .chars()
                    .any(|character| character.is_control() || character == '\u{202e}'),
                "{line:?}"
            );
        }
        assert!(
            logs[0].contains("ext_a\\u001b[31m\\u202eb"),
            "{:?}",
            logs[0]
        );
        assert!(logs[1].contains("ext_plain_zz_open"), "{:?}", logs[1]);
    }

    #[test]
    fn a_call_key_keeps_the_idempotency_key() {
        let keyed = call_key("session:sender", "call-1");
        assert_eq!(keyed.idempotency_key.as_deref(), Some("call-1"));
    }

    #[test]
    fn an_extension_tool_is_named_in_its_own_namespace() {
        let declared = DeclaredSessionOp {
            entry: super::super::store_extensions::SessionCallableOp {
                op: "open".into(),
                parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                target_arg: None,
                internal: None,
                tool_name: Some("open".into()),
                description: Some("Open.".into()),
                await_spec: None,
                wake: None,
            },
            extension: "alpha".into(),
            registered_name: "ext_alpha_open".into(),
        };
        assert_eq!(
            super::super::extension_registrations::registered_tool_name(
                &declared.extension,
                "open"
            ),
            declared.registered_name
        );
    }
}
