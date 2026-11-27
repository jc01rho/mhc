//! The one create body.
//!
//! Upstream counterpart: `thread/control/create.ts` (`createThread`) for a new, an existing or a forked thread on this
//! device (#12884): the shape checks, then placement (the only step that reads the device), then the name checked against
//! the threads this device sees, then one open through the `SessionHost` - a created session starting in its creator's
//! approval mode - then the creator record, the alias, and the first prompt. An existing thread that is live already is
//! answered from placement (`deduplicated`), so no second open reaches it - but its prompt, when one was given, is sent to
//! it and the create answers that delivery (#14335). Another device, the account alias registry and the tool receipts a
//! retry reads (`retrying`) are the surface's: see `docs/omo-sync/v5.1.83/thread-control.md`.

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

use super::create_launch::{CreateLaunch, create_launch};
use super::local_session_host::{LocalSessionHost, workspace_note};
use super::locate::{LocatedControl, ThreadCall, locate_control};
use super::operations::error_json;
use super::plan_create::{
    CreatePlan, LocalExisting, existing_here, file_open, fork_unsupported, plan_create,
};
use super::port::{
    ControlError, ControlResult, SessionHost, SessionOpenOptions, SessionOpenRequest,
};
use super::prompt_start::PromptStart;
use super::session_host::created_thread_id;

/// The longest untyped rename error kept as `local_name_unavailable_reason`, in code points; longer ones end in an
/// ellipsis.
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

/// Why the first prompt's delivery threw instead of answering: the gateway store is at a schema this build does not read
/// (`gatewaySchemaRefusal`), or any other store or connection failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptDeliveryError {
    SchemaTooNew { found: i64, supported: i64 },
    Failed(String),
}

/// What a create needs from the surface that runs it (the agent tools or the thread SDK).
pub trait CreateSurface: Send + Sync {
    /// The call's idempotency key: a new thread on this device is created under an id derived from it (#14060).
    fn operation_id(&self) -> &str;
    fn caller_id(&self) -> &str;
    /// The creator's own approval mode, which a created thread cannot exceed; `None` when it is unknown.
    fn permission_preset(&self) -> Option<String>;
    fn retrying(&self) -> Option<Retrying>;
    /// A fresh host view, for finding again a thread this create already made.
    fn view(&self) -> BoxFuture<ControlResult<ThreadCall>>;
    /// Sends the first prompt to the thread the create made, as the surface's own sender, with the creator's authority.
    /// A typed refusal is the `Ok` answer's `error` branch; `Err` is a delivery that threw.
    fn deliver_prompt(
        &self,
        send: CreatePromptSend,
        idempotency_key: String,
    ) -> BoxFuture<Result<Value, PromptDeliveryError>>;
    /// Waits, bounded by the call's budget, for the first prompt's gateway delivery on this device to start a turn.
    fn await_prompt_start(&self, delivery_id: String) -> BoxFuture<Result<PromptStart, String>>;
    /// Records that `creator` made `created` (a session op's `caller_created_target` reads it).
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

fn uncertain_failure(note: Option<&str>) -> Value {
    error_json(thread_tool_failure(
        "idempotency_uncertain",
        "The earlier operation may have been delivered.",
        "Read the target transcript before deciding whether to retry.",
        note.map(|note| json!({ "error_note": note })),
    ))
}

/// The create for a new, an existing or a forked thread: every answer is data, `Err` is a failure of the leg itself
/// (`ControlError::Transport`).
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
    if let Some(retrying) = surface.retrying() {
        let derived = created_thread_id(&caller_id, surface.operation_id());
        let found = existing_here(call, &derived, &caller_id).await;
        if !matches!(found, LocalExisting::Live(_) | LocalExisting::File { .. }) {
            return Ok(uncertain_failure(retrying.error_note.as_deref()));
        }
    }
    let Made { result, opened } = create_made(call, value, surface, launch).await?;
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
) -> ControlResult<Made> {
    let caller_id = surface.caller_id().to_owned();
    let operation_id = surface.operation_id().to_owned();
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
    let (request, creates, previous_name, local_alias) =
        match plan_create(call, value, &caller_id).await {
            CreatePlan::Done { result, thread } => {
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
                local_alias,
            } => (request, creates, previous_name, local_alias),
        };
    // #13031: an engine that does not fork on open would start a blank thread for an open carrying forkFrom, so the fork
    // is refused before anything opens (`cross-device.ts` `planCreate`, `plan.host.forks()`). An open placement sent past
    // this device asks that device's engine, not this one's.
    let device_host: Option<Arc<dyn SessionHost>> = request.remote.as_deref().and_then(|device| {
        call.remote
            .as_deref()
            .and_then(|remote| remote.session_host(device))
    });
    let opener: &dyn SessionHost = match &device_host {
        Some(host) => host.as_ref(),
        None => &host,
    };
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
    let earlier = match created_id.as_deref() {
        Some(id) => Some(existing_here(call, id, &caller_id).await),
        None => None,
    };
    let deduplicated = matches!(
        earlier,
        Some(LocalExisting::Live(_) | LocalExisting::File { .. })
    );
    if creates
        && !deduplicated
        && let Some(wanted) = name.as_deref()
    {
        let visible = opener.visible().await?;
        if let Err(conflict) = check_name_conflict(&visible, wanted) {
            return Ok(Made::answer(error_json(*conflict)));
        }
    }
    let kept = if creates {
        None
    } else {
        name.clone().or(previous_name)
    };
    let mut open = SessionOpenRequest {
        cwd: request.cwd.clone(),
        session_path: request.session_path.clone(),
        fork_from: request.fork_from.clone(),
        name: if creates { name.clone() } else { None },
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
    } else {
        keep_name(surface, opener, created, kept.as_deref(), &thread.thread).await?
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

/// The thread an earlier run of this create made, found again when the open threw after the host made the session.
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

/// thread_create records its creator (the engine-resolved caller) once the host opened the session; a session op's
/// `caller_created_target` reads it.
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

/// A reopened thread keeps its name (#14274). The engine resumes a session file without the name its listing showed, so
/// the name given at reopen, else the one the thread had, is set again through thread_rename's path. A name that cannot
/// be set is a warning on the reopened thread, never an error: it runs.
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

/// Names a thread the create already made, through the same path thread_rename takes (the name check against the
/// device's threads, then the located control's `set_name`). Answers the refusing code instead of failing, because the
/// thread exists either way: a rename that fails with no typed refusal answers `host_unavailable` with the error's message
/// as `reason`, so the failure stays traceable.
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

/// Delivers a create's `prompt` to the thread it just made, through the surface's one send path (#13410), under a key
/// derived from the create's own: a replayed create answers its stored result, so the prompt is sent once per key. The
/// target is the created thread itself, never a lookup by name, so a listing that has not caught up yet cannot answer
/// not_found (#13001).
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

/// The thread exists from here on: a delivery that throws (a store or connection failure, not a typed refusal) is the
/// prompt's outcome, never the create's, so the caller still gets the thread it made. A store schema this binary does not
/// know refuses typed here too (#13927): the generic refusal says "nothing was sent or changed" and "restart this
/// session"; neither holds for a create whose thread exists, so this keeps the code and details but says what actually
/// happened and what to do. The binary's schema comes first and the store's second (#14228).
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

/// A created thread runs its first prompt, or the create fails (the held-thread report: an ok create whose prompt sat
/// queued for hours with nobody told). A prompt the send answered started or steered stands. So does one the thread
/// admitted to its queue (behind a turn, a question or a hold), answered with what it waits on: it runs after that, and a
/// resend would deliver it twice (#14363 review M1). So does one stored for a session that is woken with it: it runs when
/// the wake does, with a next_action not to send it again (#14363 delta review L1). A prompt not yet admitted is waited
/// for, bounded by the call's budget, until its row is admitted. A prompt no device stored or admitted answers
/// `prompt_not_started`, which names the thread, the delivery and its last state: the thread exists, so the recovery is a
/// read and a send to it, never another create. A delivery that failed outright stays the prompt's own error on the ok
/// create.
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
        Some(PromptStart::Started { delivery }) => {
            prompt.insert(
                "delivery".to_owned(),
                serde_json::to_value(delivery).unwrap_or(Value::Null),
            );
            return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
        }
        Some(PromptStart::Queued { waiting_on }) => {
            let position = if kind == "queued" {
                delivery
                    .get("queue_position")
                    .and_then(Value::as_i64)
                    .unwrap_or(1)
            } else {
                1
            };
            let mut queued = Map::new();
            queued.insert("kind".to_owned(), json!("queued"));
            queued.insert("queue_position".to_owned(), json!(position));
            if let Some(wait) = waiting_on {
                queued.insert(
                    "waiting_on".to_owned(),
                    serde_json::to_value(wait).unwrap_or(Value::Null),
                );
            }
            prompt.insert("delivery".to_owned(), Value::Object(queued));
            return Ok(PromptOutcome::Prompt(Value::Object(prompt)));
        }
        _ => {}
    }
    let state = match &waited {
        Some(PromptStart::NotStarted { state }) => state.clone(),
        _ => kind.clone(),
    };
    let waiting_text = waiting_kind
        .map(|wait| format!(", waiting on its {wait}"))
        .unwrap_or_default();
    let mut details = Map::new();
    details.insert("thread".to_owned(), json!(thread.thread));
    if let Some(device) = &thread.device {
        details.insert("device".to_owned(), json!(device));
    }
    if let Some(id) = &delivery_id {
        details.insert("delivery_id".to_owned(), json!(id));
    }
    details.insert("state".to_owned(), json!(state));
    if let Some(wait) = &waiting_on {
        details.insert("waiting_on".to_owned(), wait.clone());
    }
    Ok(PromptOutcome::NotStarted(Box::new(thread_tool_failure(
        "prompt_not_started",
        format!(
            "Thread {} was created{where_text}, but its first prompt did not start a turn (its delivery is {state}{waiting_text}).",
            thread.thread
        ),
        format!(
            "The thread exists: read it with thread_read (tool.thread_read({{ thread }}) in an eval cell) to see whether the prompt ran, then send the prompt again with thread_send to {} (tool.thread_send({{ thread, message }}) in an eval cell) if it did not. Do not create the thread again.",
            thread.thread
        ),
        Some(Value::Object(details)),
    ))))
}
