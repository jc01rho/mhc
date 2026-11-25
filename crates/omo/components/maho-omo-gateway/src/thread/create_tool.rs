//! The `thread_create` tool.
//!
//! Upstream counterpart: the `create` tool of `thread/tools.ts`. It parses the call, reads this device once, and runs the one
//! create body (`control/create.rs`) under the call's budget; its first prompt goes through the same send path
//! `thread_send` takes. The tool-receipt wrapper (`toolReceiptBegin` / `toolReceiptSettle`) is not ported, so a call under a
//! key already used is answered by the create's own id derived from the key (#14060), not by a stored result.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::adapter::BoxFuture;
use super::contracts_create_params::{ThreadCreateInput, thread_create_parameters};
use super::control::create::{
    CreatePromptSend, CreateSurface, PromptDeliveryError, Retrying, create_thread,
};
use super::control::locate::{ThreadCall, book_of};
use super::control::operations::error_json;
use super::control::port::{ControlError, ControlResult};
use super::control::prompt_start::{
    FIRST_PROMPT_START_RESERVE_MS, FIRST_PROMPT_START_WAIT_MS, PromptStart, await_prompt_start,
    thread_delay,
};
use super::control_tools::{finish, thread_call};
use super::store_extension_facade::ThreadCreationRecord;
use super::store_ops::rfc3339;
use super::tools::{ThreadTool, ThreadToolsOptions, deliver_thread_send};

const DESCRIPTION: &str = "Starts a fresh agent session that runs alongside this one, for when the user wants a second session working in parallel; to spawn a child task instead, use task.";

async fn fresh_call(options: &ThreadToolsOptions) -> Result<ThreadCall, String> {
    let host = options
        .host
        .clone()
        .ok_or_else(|| "host_unavailable:".to_owned())?;
    let view = host.list_view().await?;
    let now = rfc3339(options.store.now());
    let book = book_of(&view, &[], &now);
    Ok(ThreadCall {
        host,
        view,
        book,
        caller_id: (options.caller_session_id)(),
        caller_workspace_root: (options.caller_workspace_root)(),
        sessions_directory: options.sessions_directory.clone(),
        remote: options.remote.clone(),
        now,
    })
}

struct ToolCreateSurface {
    options: Arc<ThreadToolsOptions>,
    operation_id: String,
    caller_id: String,
}

impl CreateSurface for ToolCreateSurface {
    fn operation_id(&self) -> &str {
        &self.operation_id
    }

    fn caller_id(&self) -> &str {
        &self.caller_id
    }

    fn permission_preset(&self) -> Option<String> {
        None
    }

    fn retrying(&self) -> Option<Retrying> {
        None
    }

    fn view(&self) -> BoxFuture<ControlResult<ThreadCall>> {
        let options = self.options.clone();
        Box::pin(async move { fresh_call(&options).await.map_err(ControlError::Transport) })
    }

    fn deliver_prompt(
        &self,
        send: CreatePromptSend,
        idempotency_key: String,
    ) -> BoxFuture<Result<Value, PromptDeliveryError>> {
        let options = self.options.clone();
        let caller_id = self.caller_id.clone();
        Box::pin(async move {
            let mut args = Map::new();
            args.insert("thread".to_owned(), json!(send.thread));
            args.insert("message".to_owned(), json!(send.message));
            if let Some(delivery) = &send.delivery {
                args.insert("delivery".to_owned(), json!(delivery));
            }
            args.insert("all_scope".to_owned(), json!(true));
            args.insert("idempotency_key".to_owned(), json!(idempotency_key));
            let caller_name = options.caller_name.as_ref().and_then(|name| name());
            Ok(deliver_thread_send(
                &options,
                caller_id,
                caller_name,
                &idempotency_key,
                Value::Object(args),
            )
            .await)
        })
    }

    fn await_prompt_start(&self, delivery_id: String) -> BoxFuture<Result<PromptStart, String>> {
        let store = self.options.store.clone();
        let wait = call_budget::wait_within_call_budget(
            FIRST_PROMPT_START_WAIT_MS,
            FIRST_PROMPT_START_RESERVE_MS,
            call_budget::now_ms(),
        );
        Box::pin(async move {
            await_prompt_start(
                |id: &str| store.delivery_view(id),
                &delivery_id,
                wait,
                call_budget::now_ms,
                thread_delay,
            )
            .await
            .map_err(|error| error.message())
        })
    }

    fn record_creation(&self, creator: &str, created: &str) -> Result<(), String> {
        let record = ThreadCreationRecord {
            creator_durable_id: creator.to_owned(),
            created_durable_id: created.to_owned(),
        };
        self.options
            .store
            .record_thread_creation(&record, self.options.store.now())
            .map_err(|error| error.message())
    }
}

async fn create_body(options: Arc<ThreadToolsOptions>, call_id: String, args: Value) -> Value {
    let input = match ThreadCreateInput::parse(&args) {
        Ok(input) => input,
        Err(failure) => return error_json(*failure),
    };
    let call = match thread_call(&options).await {
        Ok(call) => call,
        Err(result) => return result,
    };
    let operation_id = input
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map_or_else(|| format!("call:{call_id}"), str::to_owned);
    let surface = ToolCreateSurface {
        caller_id: (options.caller_session_id)(),
        options: options.clone(),
        operation_id,
    };
    let deadline = call_budget::now_ms() + call_budget::TOOL_CALL_BUDGET_MS;
    finish(call_budget::with_call_deadline(deadline, create_thread(&call, &input, &surface)).await)
}

/// `thread_create`: a new, an existing or a forked thread on this device, with its first prompt.
pub fn thread_create_tool(options: &Arc<ThreadToolsOptions>) -> ThreadTool {
    let options = options.clone();
    ThreadTool {
        name: "thread_create".to_owned(),
        description: DESCRIPTION.to_owned(),
        parameters: thread_create_parameters(),
        execute: Arc::new(move |call_id: &str, args: Value| -> BoxFuture<Value> {
            let options = options.clone();
            let call_id = call_id.to_owned();
            Box::pin(create_body(options, call_id, args))
        }),
    }
}
