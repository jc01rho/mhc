//! The one create body.
//!
//! Upstream counterpart: `thread/control/create.ts` (`createThread`) for a new, an existing or a forked thread, here or
//! on another device (#12884, #14060, #14560, #14569).

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::addressing::{
    ThreadAddressEntry, check_name_conflict, to_thread_address_entries,
};
use crate::thread::contracts_create_params::ThreadCreateInput;
use crate::thread::contracts_results::{ThreadResultSummary, thread_link};
use crate::thread::errors::{
    ThreadToolFailure, UPDATE_COMMAND, gateway_schema_refusal, thread_tool_failure,
};
use crate::thread::initial_skills::{InitialSkillsCheck, check_initial_skills};
use crate::thread::store_extension_session_ops::UNKNOWN_CALLER;
use crate::thread::thread_row::opened_result;
use crate::thread::tool_receipts::{note_cause, uncertain_failure};
use crate::thread::worktree::thread_worktree::ThreadWorktree;

use super::create_launch::{CreateLaunch, create_launch};
use super::local_session_host::{LocalSessionHost, workspace_note};
use super::locate::{LocatedControl, ThreadCall, locate_control};
use super::operations::error_json;
use super::plan_create::{
    CreatePlan, LocalExisting, existing_here, file_open, fork_unsupported, plan_create,
};
use super::port::{
    ControlError, ControlResult, SessionAdoptRequest, SessionHost, SessionOpenOptions,
    SessionOpenRequest,
};
use super::prompt_start::PromptStart;
use super::session_host::created_thread_id;

const RENAME_REASON_MAX_CHARS: usize = 300;

/// A create's first prompt as the one send path takes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreatePromptSend {
    pub thread: String,
    pub device: Option<String>,
    pub message: String,
    pub delivery: Option<String>,
}

/// Set when an earlier run under this call's key failed after it may have made a thread (#14060).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retrying {
    pub error_note: Option<String>,
}

/// Why the first prompt's delivery threw instead of answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptDeliveryError {
    SchemaTooNew { found: i64, supported: i64 },
    Failed(String),
}

/// What a create needs from the surface that runs it (the agent tools or the thread SDK).
pub trait CreateSurface: Send + Sync {
    fn operation_id(&self) -> &str;
    fn caller_id(&self) -> &str;
    fn permission_preset(&self) -> Option<String>;
    fn retrying(&self) -> Option<Retrying>;
    fn view(&self) -> BoxFuture<ControlResult<ThreadCall>>;
    fn deliver_prompt(
        &self,
        send: CreatePromptSend,
        idempotency_key: String,
    ) -> BoxFuture<Result<Value, PromptDeliveryError>>;
    fn await_prompt_start(&self, delivery_id: String) -> BoxFuture<Result<PromptStart, String>>;
    fn record_creation(&self, creator: &str, created: &str) -> Result<(), String>;
}

struct Made {
    result: Value,
    opened: Option<ThreadResultSummary>,
}

impl Made {
    fn answer(result: Value) -> Self {
        Self {
            result,
            opened: None,
        }
    }
}

enum FirstPrompt {
    Failed(ThreadToolFailure),
    Sent(Map<String, Value>),
}

enum PromptOutcome {
    Prompt(Value),
    NotStarted(Box<ThreadToolFailure>),
}

struct NameRefusal {
    code: String,
    reason: Option<String>,
}

fn is_ok(result: &Value) -> bool {
    result.get("kind").and_then(Value::as_str) == Some("ok")
}

fn put(result: &mut Value, key: &str, field: Value) {
    if let Some(object) = result.as_object_mut() {
        object.insert(key.to_owned(), field);
    }
}

/// The worktree an uncertain create's first answer kept (its receipt note, `uncertain_note`), when it names a whole one (#14569).
pub fn kept_worktree(note: Option<&str>) -> Option<ThreadWorktree> {
    let note = note?;
    let details = note_cause(note).details?;
    let wt = details.get("worktree")?.as_object()?;
    let text = |key: &str| wt.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(ThreadWorktree {
        path: text("path")?,
        branch: text("branch")?,
        base: text("base")?,
        repository: text("repository")?,
        setup_session_left_open: text("setup_session_left_open"),
    })
}

/// The create for a new, an existing or a forked thread: every answer is data, `Err` is a failure of the leg itself.
pub async fn create_thread(
    call: &ThreadCall,
    value: &ThreadCreateInput,
    surface: &dyn CreateSurface,
) -> ControlResult<Value> {
    match create_inner(call, value, surface).await {
        Err(ControlError::Refused(failure)) => Ok(error_json(*failure)),
        other => other,
    }
}

async fn create_inner(
    call: &ThreadCall,
    value: &ThreadCreateInput,
    surface: &dyn CreateSurface,
) -> ControlResult<Value> {
    let launch = match create_launch(value, surface.permission_preset().as_deref()) {
        Ok(launch) => launch,
        Err(failure) => return Ok(error_json(*failure)),
    };
    if value
        .prompt
        .as_deref()
        .is_some_and(|prompt| prompt.trim().is_empty())
    {
        return Ok(error_json(thread_tool_failure(
            "invalid_arguments",
            "prompt is blank, so it is no first message; nothing was created.",
            "Pass the new thread's first message as prompt, or drop prompt and pass name to open an empty named thread.",
            Some(json!({ "field": "prompt" })),
        )));
    }
    let named = value
        .name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
        || value
            .alias
            .as_deref()
            .is_some_and(|alias| !alias.trim().is_empty());
    if value.thread.is_none() && value.fork_from.is_none() && value.prompt.is_none() && !named {
        return Ok(error_json(thread_tool_failure(
            "invalid_arguments",
            "thread_create (tool.thread_create() in an eval cell) makes a new thread only with a prompt or a name; neither was given, so nothing was created.",
            "Pass prompt (the new thread's first message) or name, or pass thread to open an existing thread.",
            None,
        )));
    }
    let caller_id = surface.caller_id().to_owned();
    let retrying = surface.retrying();
    let mut retries_elsewhere = false;
    if retrying.is_some() {
        let derived = created_thread_id(&caller_id, surface.operation_id());
        let found = existing_here(call, &derived, &caller_id).await;
        retries_elsewhere = !matches!(found, LocalExisting::Live(_) | LocalExisting::File { .. });
    }
    let Made { result, opened } = create_made(
        call,
        value,
        surface,
        launch,
        retrying.as_ref(),
        retries_elsewhere,
    )
    .await?;
    if value.prompt.is_none() {
        let mut answered = result;
        if is_ok(&answered) {
            put(&mut answered, "prompt", Value::Null);
        }
        return Ok(answered);
    }
    let Some(opened) = opened.filter(|_| is_ok(&result)) else {
        return Ok(result);
    };
    let first = first_prompt(&opened, value, surface).await;
    match prompt_that_ran(first, &opened, surface).await? {
        PromptOutcome::NotStarted(failure) => Ok(error_json(*failure)),
        PromptOutcome::Prompt(prompt) => {
            let mut answered = result;
            put(&mut answered, "prompt", prompt);
            Ok(answered)
        }
    }
}

async fn create_made(
    call: &ThreadCall,
    value: &ThreadCreateInput,
    surface: &dyn CreateSurface,
    launch: CreateLaunch,
    retrying: Option<&Retrying>,
    retries_elsewhere: bool,
) -> ControlResult<Made> {
    let caller_id = surface.caller_id().to_owned();
    let operation_id = surface.operation_id().to_owned();
    let still_uncertain = || uncertain_failure(retrying.and_then(|r| r.error_note.as_deref()));
    let first_worktree = kept_worktree(retrying.and_then(|r| r.error_note.as_deref()));
    let skills = value
        .skills
        .as_deref()
        .filter(|names| !names.is_empty())
        .map(check_initial_skills);
    if let Some(InitialSkillsCheck::Invalid { names }) = &skills {
        return Ok(Made::answer(error_json(thread_tool_failure(
            "invalid_arguments",
            format!(
                "Not a skill name: {}. Pass the names of installed skills, never a path or text.",
                names.join(", ")
            ),
            "Pass skill names as listed by the skill index.",
            Some(json!({ "invalid": names })),
        ))));
    }
    let loaded: Option<Vec<String>> = match skills {
        Some(InitialSkillsCheck::Ok { names }) => Some(names),
        _ => None,
    };
    let host = LocalSessionHost::new(
        call.host.clone(),
        to_thread_address_entries(&call.book),
        call.now.clone(),
    );
    let (request, creates, previous_name, reopened_id, local_alias) =
        match plan_create(call, value, &caller_id).await {
            CreatePlan::Done { result, thread } => {
                if retries_elsewhere {
                    return Ok(Made::answer(still_uncertain()));
                }
                let opened = if is_ok(&result) {
                    thread.map(|thread| *thread)
                } else {
                    None
                };
                return Ok(Made { result, opened });
            }
            CreatePlan::Open {
                request,
                creates,
                previous_name,
                reopened_id,
                local_alias,
            } => (request, creates, previous_name, reopened_id, local_alias),
        };
    let device_host: Option<Arc<dyn SessionHost>> = request.remote.as_deref().and_then(|device| {
        call.remote
            .as_deref()
            .and_then(|remote| remote.session_host(device))
    });
    let opener: &dyn SessionHost = match &device_host {
        Some(host) => host.as_ref(),
        None => &host,
    };
    if retries_elsewhere && (!creates || !opener.supports_adopt()) {
        return Ok(Made::answer(still_uncertain()));
    }
    if let Some(source) = request.fork_from.as_deref()
        && !opener.forks().await?
    {
        return Ok(Made::answer(fork_unsupported(
            source,
            request.remote.as_deref(),
        )));
    }
    let name = if local_alias.is_none() {
        value.name.clone()
    } else {
        value
            .name
            .clone()
            .or_else(|| value.alias.as_deref().map(|alias| alias.trim().to_owned()))
    };
    let created_id = creates.then(|| created_thread_id(&caller_id, &operation_id));
    let earlier = if opener.device().is_none() {
        match created_id.as_deref() {
            Some(id) => Some(existing_here(call, id, &caller_id).await),
            None => None,
        }
    } else {
        None
    };
    let deduplicated = matches!(
        earlier,
        Some(LocalExisting::Live(_) | LocalExisting::File { .. })
    );
    let checks_name_before_open = creates || opener.device().is_some();
    if checks_name_before_open
        && !deduplicated
        && let Some(wanted) = name.as_deref()
    {
        let visible: Vec<ThreadAddressEntry> = opener
            .visible()
            .await?
            .into_iter()
            .filter(|t| {
                Some(t.thread_id.as_str()) != reopened_id.as_deref()
                    && Some(t.thread_id.as_str()) != created_id.as_deref()
            })
            .collect();
        if let Err(conflict) = check_name_conflict(&visible, wanted) {
            return Ok(Made::answer(error_json(*conflict)));
        }
    }
    let kept = if creates {
        None
    } else {
        name.clone().or(previous_name)
    };
    let open_name = if creates {
        name.clone()
    } else if opener.device().is_none() {
        None
    } else {
        kept.clone()
    };
    let mut open = SessionOpenRequest {
        cwd: request.cwd.clone(),
        session_path: request.session_path.clone(),
        fork_from: request.fork_from.clone(),
        name: open_name.clone(),
        worktree: creates.then(|| value.worktree.clone()).flatten(),
        durable_session_id: created_id.clone(),
        remote_session: request.remote_session.clone(),
        ..SessionOpenRequest::default()
    };
    if creates {
        if let Some(skills) = &loaded {
            open.initial_skills = skills.clone();
        }
        open.permission_preset = launch.permission_preset.clone();
        open.provider = launch.profile.provider.clone();
        open.model_id = launch.profile.model_id.clone();
        open.thinking_level = launch.profile.thinking_level.clone();
    }
    let thread: ThreadResultSummary = match &earlier {
        Some(LocalExisting::Live(live)) => (**live).clone(),
        Some(LocalExisting::File {
            session_path, cwd, ..
        }) => {
            host.open(
                file_open(session_path, cwd.as_deref()),
                SessionOpenOptions::default(),
            )
            .await?
        }
        _ if retries_elsewhere => {
            let Some(id) = created_id.clone() else {
                return Ok(Made::answer(still_uncertain()));
            };
            let found = opener
                .adopt(
                    SessionAdoptRequest {
                        durable_session_id: id,
                        name: open_name,
                        worktree: first_worktree,
                    },
                    SessionOpenOptions {
                        confirm_reach: true,
                    },
                )
                .await?;
            match found {
                Some(thread) => thread,
                None => return Ok(Made::answer(still_uncertain())),
            }
        }
        _ => match opener
            .open(
                open,
                SessionOpenOptions {
                    confirm_reach: creates,
                },
            )
            .await
        {
            Ok(thread) => thread,
            Err(ControlError::Transport(message)) => {
                if opener.device().is_some() {
                    return Err(ControlError::Transport(message));
                }
                match reopened(surface, &host, created_id.as_deref(), &caller_id).await? {
                    Some(found) => found,
                    None => return Err(ControlError::Transport(message)),
                }
            }
            Err(refused) => return Err(refused),
        },
    };
    let mut created = opened_result(&thread, deduplicated);
    if creates && let Some(names) = &loaded {
        put(&mut created, "requested_skills", json!(names));
    }
    for (key, note) in workspace_note(&thread, &call.caller_workspace_root) {
        put(&mut created, key, note);
    }
    let recorded = if creates {
        record_creator(call, surface, &caller_id, created)?
    } else if opener.device().is_none() {
        keep_name(surface, opener, created, kept.as_deref(), &thread.thread).await?
    } else {
        created
    };
    let mut answered = recorded;
    if let Some(reason) = local_alias
        && is_ok(&answered)
    {
        put(&mut answered, "alias_scope", json!("local"));
        put(&mut answered, "alias_unavailable", json!(reason));
    }
    Ok(Made {
        result: answered,
        opened: Some(thread),
    })
}

async fn reopened(
    surface: &dyn CreateSurface,
    host: &LocalSessionHost,
    created_id: Option<&str>,
    caller_id: &str,
) -> ControlResult<Option<ThreadResultSummary>> {
    let Some(id) = created_id else {
        return Ok(None);
    };
    let fresh = surface.view().await?;
    match existing_here(&fresh, id, caller_id).await {
        LocalExisting::Live(thread) => Ok(Some(*thread)),
        LocalExisting::File {
            session_path, cwd, ..
        } => host
            .open(
                file_open(&session_path, cwd.as_deref()),
                SessionOpenOptions::default(),
            )
            .await
            .map(Some),
        LocalExisting::Error(_) => Ok(None),
    }
}

fn record_creator(
    call: &ThreadCall,
    surface: &dyn CreateSurface,
    caller_id: &str,
    created: Value,
) -> ControlResult<Value> {
    let created_id = created
        .get("thread")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(created_id) = created_id else {
        return Ok(created);
    };
    if caller_id.is_empty() || caller_id == UNKNOWN_CALLER {
        return Ok(created);
    }
    let creator = call
        .book
        .iter()
        .find(|entry| entry.durable_id == caller_id)
        .map(|entry| entry.durable_id.clone());
    if let Some(creator) = creator
        && creator != created_id
    {
        surface
            .record_creation(&creator, &created_id)
            .map_err(ControlError::Transport)?;
    }
    Ok(created)
}

async fn keep_name(
    surface: &dyn CreateSurface,
    host: &dyn SessionHost,
    mut created: Value,
    wanted: Option<&str>,
    thread_id: &str,
) -> ControlResult<Value> {
    let name = wanted.map(str::trim).unwrap_or_default();
    if name.is_empty() || !is_ok(&created) {
        return Ok(created);
    }
    let visible = host.visible().await?;
    match name_created_thread(surface, thread_id, name, &visible).await? {
        None => {
            put(&mut created, "name", json!(name));
            put(&mut created, "link", json!(thread_link(thread_id, name)));
        }
        Some(refusal) => {
            let mut details = Map::new();
            details.insert("reason".to_owned(), json!("name_not_applied"));
            details.insert("name".to_owned(), json!(name));
            if let Some(cause) = refusal.reason {
                details.insert("cause".to_owned(), json!(cause));
            }
            let warning = thread_tool_failure(
                &refusal.code,
                "The thread was reopened, but its name was not set on it.",
                "Name it with thread_rename (tool.thread_rename({ thread, name }) in an eval cell).",
                Some(Value::Object(details)),
            );
            let mut warnings = created
                .get("warnings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            warnings.push(serde_json::to_value(&warning).unwrap_or(Value::Null));
            put(&mut created, "warnings", Value::Array(warnings));
        }
    }
    Ok(created)
}

async fn name_created_thread(
    surface: &dyn CreateSurface,
    thread_id: &str,
    name: &str,
    visible: &[ThreadAddressEntry],
) -> ControlResult<Option<NameRefusal>> {
    let others: Vec<ThreadAddressEntry> = visible
        .iter()
        .filter(|entry| entry.thread_id != thread_id)
        .cloned()
        .collect();
    if let Err(conflict) = check_name_conflict(&others, name) {
        return Ok(Some(NameRefusal {
            code: conflict.code.as_str().to_owned(),
            reason: None,
        }));
    }
    let fresh = surface.view().await?;
    let refusal = |code: &str, reason: Option<String>| -> ControlResult<Option<NameRefusal>> {
        Ok(Some(NameRefusal {
            code: code.to_owned(),
            reason,
        }))
    };
    match locate_control(&fresh, thread_id, true).await {
        LocatedControl::NotLive { .. } => refusal("not_resumable", None),
        LocatedControl::Error(failure) => refusal(failure.code.as_str(), None),
        LocatedControl::Control(control) => match control.set_name(name.to_owned()).await {
            Ok(()) => Ok(None),
            Err(ControlError::Refused(failure)) => refusal(failure.code.as_str(), None),
            Err(ControlError::Transport(message)) => {
                let reason = if message.chars().count() > RENAME_REASON_MAX_CHARS {
                    let kept: String = message.chars().take(RENAME_REASON_MAX_CHARS).collect();
                    format!("{kept}…")
                } else {
                    message
                };
                refusal("host_unavailable", Some(reason))
            }
        },
    }
}

async fn first_prompt(
    thread: &ThreadResultSummary,
    value: &ThreadCreateInput,
    surface: &dyn CreateSurface,
) -> FirstPrompt {
    let thread_id = thread.thread.as_str();
    let send = CreatePromptSend {
        thread: thread_id.to_owned(),
        device: thread.device.clone(),
        message: value.prompt.clone().unwrap_or_default(),
        delivery: value.delivery.clone(),
    };
    let key = format!("{}:prompt", surface.operation_id());
    match surface.deliver_prompt(send, key).await {
        Ok(sent) => sent_prompt(sent, thread_id),
        Err(error) => FirstPrompt::Failed(undelivered_prompt(thread_id, error)),
    }
}

fn sent_prompt(sent: Value, thread_id: &str) -> FirstPrompt {
    if sent.get("kind").and_then(Value::as_str) == Some("error") {
        let failure = sent
            .get("error")
            .cloned()
            .and_then(|error| serde_json::from_value::<ThreadToolFailure>(error).ok());
        return FirstPrompt::Failed(failure.unwrap_or_else(|| {
            thread_tool_failure(
                "internal_error",
                "The first prompt's delivery answered an error the create could not read.",
                format!("Read the thread with thread_read (tool.thread_read({{ thread }}) in an eval cell) to see whether the prompt arrived, then send it with thread_send to {thread_id} if it did not."),
                Some(json!({ "thread": thread_id })),
            )
        }));
    }
    let mut prompt = Map::new();
    for key in ["delivery", "deduplicated", "delivery_id", "effective_mode"] {
        if let Some(field) = sent.get(key) {
            prompt.insert(key.to_owned(), field.clone());
        }
    }
    FirstPrompt::Sent(prompt)
}

fn undelivered_prompt(thread_id: &str, error: PromptDeliveryError) -> ThreadToolFailure {
    match error {
        PromptDeliveryError::SchemaTooNew { found, supported } => {
            let refusal = gateway_schema_refusal(found, supported);
            let mut details = refusal.details.unwrap_or_else(|| json!({}));
            if let Some(object) = details.as_object_mut() {
                object.insert("thread".to_owned(), json!(thread_id));
            }
            thread_tool_failure(
                refusal.code.as_str(),
                format!(
                    "The thread was created, but its first prompt was not delivered: this omo build reads gateway schema {supported}, while this machine's gateway store is at schema {found}: a newer omo build already upgraded it."
                ),
                format!(
                    "Update omo with `{UPDATE_COMMAND}`, then send the prompt with thread_send to {thread_id} (tool.thread_send({{ thread, message }}) in an eval cell)."
                ),
                Some(details),
            )
        }
        PromptDeliveryError::Failed(message) => thread_tool_failure(
            "internal_error",
            format!("The thread was created, but its first prompt was not delivered: {message}."),
            format!(
                "Send it with thread_send to {thread_id} (tool.thread_send({{ thread, message }}) in an eval cell); read the thread first if the prompt may have arrived."
            ),
            Some(json!({ "thread": thread_id })),
        ),
    }
}

async fn prompt_that_ran(
    prompt: FirstPrompt,
    thread: &ThreadResultSummary,
    surface: &dyn CreateSurface,
) -> ControlResult<PromptOutcome> {
    let mut prompt = match prompt {
        FirstPrompt::Failed(failure) => {
            return Ok(PromptOutcome::Prompt(json!({ "error": failure })));
        }
        FirstPrompt::Sent(prompt) => prompt,
    };
    let delivery = prompt.get("delivery").cloned().unwrap_or(Value::Null);
    let kind = delivery
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if kind == "started" || kind == "steered" {
        return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
    }
    let waiting_on = if kind == "queued" {
        delivery.get("waiting_on").cloned()
    } else {
        None
    };
    let waiting_kind = waiting_on
        .as_ref()
        .and_then(|wait| wait.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let local = thread.device.is_none();
    let where_text = thread
        .device
        .as_deref()
        .map_or_else(String::new, |device| format!(" on device {device}"));
    if waiting_kind.as_deref() == Some("wake") {
        prompt.insert(
            "next_action".to_owned(),
            json!(format!(
                "The prompt is stored{where_text} and runs when {} wakes: do not send it again. Check it with thread_read (tool.thread_read({{ thread }}) in an eval cell).",
                thread.thread
            )),
        );
        return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
    }
    if !local && kind == "queued" {
        return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
    }
    let delivery_id = prompt
        .get("delivery_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let waited = match delivery_id.clone() {
        Some(id) if local => Some(
            surface
                .await_prompt_start(id)
                .await
                .map_err(ControlError::Transport)?,
        ),
        _ => None,
    };
    match &waited {
        Some(PromptStart::Started(started)) => {
            prompt.insert(
                "delivery".to_owned(),
                serde_json::to_value(started).unwrap_or(Value::Null),
            );
            return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
        }
        Some(PromptStart::Queued {
            waiting_on: waited_wait,
        }) => {
            let position = delivery
                .get("queue_position")
                .and_then(Value::as_i64)
                .unwrap_or(1);
            let mut queued = Map::new();
            queued.insert("kind".to_owned(), json!("queued"));
            queued.insert("queue_position".to_owned(), json!(position));
            if let Some(wait) = waited_wait {
                queued.insert(
                    "waiting_on".to_owned(),
                    serde_json::to_value(wait).unwrap_or(Value::Null),
                );
            }
            prompt.insert("delivery".to_owned(), Value::Object(queued));
            return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
        }
        Some(PromptStart::NotStarted { .. }) | None => {}
    }
    let state = match &waited {
        Some(PromptStart::NotStarted { state }) => state.as_str(),
        _ => kind.as_str(),
    };
    let wait_note = waiting_kind
        .as_deref()
        .map_or_else(String::new, |kind| format!(", waiting on its {kind}"));
    let thread_id = thread.thread.as_str();
    let mut details = Map::new();
    details.insert("thread".to_owned(), json!(thread_id));
    if let Some(device) = &thread.device {
        details.insert("device".to_owned(), json!(device));
    }
    if let Some(id) = delivery_id {
        details.insert("delivery_id".to_owned(), json!(id));
    }
    details.insert("state".to_owned(), json!(state));
    if let Some(waiting_on) = waiting_on {
        details.insert("waiting_on".to_owned(), waiting_on);
    }
    Ok(PromptOutcome::NotStarted(Box::new(thread_tool_failure(
        "prompt_not_started",
        format!(
            "Thread {thread_id} was created{where_text}, but its first prompt did not start a turn (its delivery is {state}{wait_note})."
        ),
        format!(
            "The thread exists: read it with thread_read (tool.thread_read({{ thread }}) in an eval cell) to see whether the prompt ran, then send the prompt again with thread_send to {thread_id} (tool.thread_send({{ thread, message }}) in an eval cell) if it did not. Do not create the thread again."
        ),
        Some(Value::Object(details)),
    ))))
}
