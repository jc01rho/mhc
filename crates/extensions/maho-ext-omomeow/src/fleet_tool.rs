//! Port of omo-nyang `src/fleet-tool.ts`: the `fleet_task` tool runs a prompt on another idle
//! worker machine through the hub's `fleet-run.ts` and returns the answer as the tool result.

use maho_agent::types::{AgentToolResult, AgentToolUpdateCallback};
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{ExtensionApi, ExtensionFailure};
use maho_tools::definition::{ToolDefinition, ToolError};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

pub const TOOL_NAME: &str = "fleet_task";

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FleetRecord {
    pub id: String,
    pub state: String,
    pub machine: Option<String>,
    pub hostname: Option<String>,
    pub held: Option<String>,
    pub answer: Option<String>,
    pub error: Option<String>,
}

pub struct CallOutput {
    pub code: i32,
    pub out: String,
    pub err: String,
}

pub type CallFuture = Pin<Box<dyn Future<Output = Result<CallOutput, String>> + Send>>;
pub type FleetCall = Arc<dyn Fn(Vec<String>) -> CallFuture + Send + Sync>;

pub fn fleet_tool_script(directory: &Path) -> PathBuf {
    directory.join("fleet-run.ts")
}

pub fn bun_call(script: PathBuf) -> FleetCall {
    Arc::new(move |args| {
        let script = script.clone();
        Box::pin(async move {
            let bun = std::env::var("OMOMEOW_BUN").unwrap_or_else(|_| "bun".to_owned());
            let output = tokio::process::Command::new(bun)
                .arg(&script)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| error.to_string())?;
            Ok(CallOutput {
                code: output.status.code().unwrap_or(-1),
                out: String::from_utf8_lossy(&output.stdout).into_owned(),
                err: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    })
}

fn last_json(out: &str) -> Result<Value, String> {
    let line = out
        .trim()
        .lines()
        .rfind(|line| !line.is_empty())
        .unwrap_or("null");
    serde_json::from_str(line).map_err(|error| error.to_string())
}

async fn fleet_record(call: &FleetCall, args: Vec<String>) -> Result<FleetRecord, String> {
    let verb = args.first().cloned().unwrap_or_default();
    let result = call(args).await?;
    if result.code != 0 {
        let detail = if result.err.is_empty() {
            result.out
        } else {
            result.err
        };
        let detail = detail.trim();
        let tail = &detail[detail.len()
            - detail
                .chars()
                .rev()
                .take(500)
                .map(char::len_utf8)
                .sum::<usize>()..];
        return Err(format!("fleet-run {verb} failed: {tail}"));
    }
    serde_json::from_value(last_json(&result.out)?).map_err(|error| error.to_string())
}

pub fn fleet_result_text(record: &FleetRecord) -> String {
    let where_ = match &record.machine {
        Some(machine) => match &record.hostname {
            Some(host) => format!("{machine} ({host})"),
            None => machine.clone(),
        },
        None => "배치 전".to_owned(),
    };
    if record.state == "done" {
        return format!(
            "fleet {} finished on {where_}.\n\n{}",
            record.id,
            record.answer.as_deref().unwrap_or("")
        );
    }
    let detail = record
        .error
        .as_deref()
        .or(record.held.as_deref())
        .unwrap_or("no detail");
    format!("fleet {} {} on {where_}: {detail}", record.id, record.state)
}

pub fn progress_text(record: &FleetRecord) -> String {
    if record.state == "queued" {
        return match &record.held {
            Some(held) => format!("fleet {} queued: {held}", record.id),
            None => format!("fleet {} queued", record.id),
        };
    }
    format!(
        "fleet {} {} on {}",
        record.id,
        record.state,
        record.machine.as_deref().unwrap_or("?")
    )
}

/// Validated `fleet_task` parameters in the request shape `fleet-run.ts start` expects.
pub fn request(params: &Value, from: Option<&str>) -> Result<Value, String> {
    let object = params.as_object().ok_or("parameters must be an object")?;
    let allowed = [
        "prompt",
        "project",
        "machine",
        "model",
        "thinking",
        "timeout_minutes",
    ];
    if let Some(extra) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown parameter: {extra}"));
    }
    let prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if prompt.is_empty() {
        return Err("prompt must be a non-empty string".into());
    }
    let mut request = Map::new();
    request.insert("prompt".into(), json!(prompt));
    for key in ["project", "machine", "model", "thinking"] {
        match object.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) if text.is_empty() => {}
            Some(Value::String(text)) => {
                request.insert(key.into(), json!(text));
            }
            Some(_) => return Err(format!("{key} must be a string")),
        }
    }
    if let Some(value) = object.get("timeout_minutes").filter(|v| !v.is_null()) {
        let minutes = value
            .as_i64()
            .filter(|m| (1..=240).contains(m))
            .ok_or("timeout_minutes must be an integer from 1 to 240")?;
        request.insert("timeoutMinutes".into(), json!(minutes));
    }
    if let Some(from) = from {
        request.insert("from".into(), json!(from));
    }
    Ok(Value::Object(request))
}

/// Starts a run and waits for it in rounds short enough for a forwarded hub tool; an aborted call
/// cancels it.
pub async fn run_fleet_task(
    call: &FleetCall,
    request: Value,
    signal: Option<&AbortSignal>,
    progress: &(dyn Fn(&FleetRecord) + Send + Sync),
) -> Result<FleetRecord, String> {
    let mut record = fleet_record(call, vec!["start".into(), request.to_string()]).await?;
    progress(&record);
    while record.state == "queued" || record.state == "running" {
        let wait = fleet_record(
            call,
            vec!["wait".into(), record.id.clone(), "100000".into()],
        );
        let next = match signal {
            Some(signal) => tokio::select! {
                biased;
                () = signal.cancelled() => None,
                next = wait => Some(next),
            },
            None => Some(wait.await),
        };
        let Some(next) = next else {
            let cancel = Arc::clone(call);
            let id = record.id.clone();
            tokio::spawn(async move {
                let _ = cancel(vec!["cancel".into(), id]).await;
            });
            return Err(format!("fleet {} cancelled", record.id));
        };
        let next = next?;
        if next.state != record.state || next.machine != record.machine {
            progress(&next);
        }
        record = next;
    }
    Ok(record)
}

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": { "type": "string", "description": "Complete instructions for the other session" },
            "project": { "type": "string", "description": "Absolute project path; mapped to the same path under the other machine's home" },
            "machine": { "type": "string", "description": "Fleet machine name to pin; omit to let the hub choose an idle one" },
            "model": { "type": "string", "description": "provider/model for the fleet session; omit for the default" },
            "thinking": { "type": "string", "description": "Thinking level for the fleet session" },
            "timeout_minutes": { "type": "integer", "minimum": 1, "maximum": 240, "description": "Turn limit, default 40" }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

pub fn definition() -> ToolDefinition {
    let mut tool = ToolDefinition::new(
        TOOL_NAME,
        "Run a prompt on another, idle worker machine of the OmOMeow fleet in a fresh omo session and return its final answer. \
Use it instead of task/agent for independent, self-contained work that would crowd this machine (builds, test suites, large investigations). \
The other session has no access to this conversation: put everything it needs in the prompt. One machine runs one task at a time, so a call may wait for a free machine.",
        parameters(),
        Arc::new(|_call| {
            Box::pin(async {
                Err(ToolError::Message(
                    "fleet_task runs through its extension executor".into(),
                ))
            })
        }),
    );
    tool.label = "Fleet task".into();
    tool.prompt_snippet = Some(
        "Run a self-contained prompt on another idle fleet machine and return its answer".into(),
    );
    tool.prompt_guidelines = Some(vec![
        "Use fleet_task for independent work that would crowd this machine; several fleet_task calls in one eval parallel() spread over several machines.".into(),
        "fleet_task prompts must be self-contained: the fleet session cannot see this conversation, and a project path is mapped to the other machine's home.".into(),
    ]);
    tool
}

pub async fn execute(
    call: FleetCall,
    params: Value,
    signal: Option<AbortSignal>,
    update: Option<AgentToolUpdateCallback>,
) -> Result<AgentToolResult, ExtensionFailure> {
    let from = std::env::var("OMOMEOW_TASK_ID")
        .ok()
        .filter(|id| !id.is_empty());
    let request = request(&params, from.as_deref()).map_err(ExtensionFailure::new)?;
    let progress = move |record: &FleetRecord| {
        if let Some(update) = &update {
            update(AgentToolResult::text(progress_text(record)));
        }
    };
    let record = run_fleet_task(&call, request, signal.as_ref(), &progress)
        .await
        .map_err(ExtensionFailure::new)?;
    let mut result = AgentToolResult::text(fleet_result_text(&record));
    result.details = json!({ "id": record.id, "state": record.state, "machine": record.machine, "hostname": record.hostname });
    if record.state != "done" {
        result.is_error = Some(true);
    }
    Ok(result)
}

/// The directory holding `fleet-run.ts`: beside the `--extension` file the gateway passed, else
/// `$OMOMEOW_HOME/src`.
pub fn script_directory(extension_path: &str) -> Option<PathBuf> {
    let path = Path::new(extension_path);
    if path.extension().is_some_and(|e| e == "ts") {
        return path.parent().map(Path::to_path_buf);
    }
    std::env::var_os("OMOMEOW_HOME").map(|home| PathBuf::from(home).join("src"))
}

pub fn register_fleet_tool(
    api: &mut ExtensionApi,
    call: FleetCall,
) -> Result<(), ExtensionFailure> {
    // A fleet session never hands its own work on again.
    if std::env::var_os("OMOMEOW_FLEET_RUN").is_some_and(|v| !v.is_empty()) {
        return Ok(());
    }
    api.register_tool_with_extension_context(
        definition(),
        Arc::new(move |_, params, signal, update, _| {
            let call = Arc::clone(&call);
            Box::pin(execute(call, params, signal, update))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn record(id: &str, state: &str, machine: Option<&str>) -> String {
        json!({"id": id, "state": state, "machine": machine, "request": {"prompt": "x"}, "createdAt": 1}).to_string()
    }

    fn scripted(replies: Vec<(i32, String)>, seen: Arc<Mutex<Vec<Vec<String>>>>) -> FleetCall {
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        Arc::new(move |args| {
            seen.lock().unwrap().push(args);
            let (code, out) = replies.lock().unwrap().next().unwrap_or((1, String::new()));
            Box::pin(async move {
                Ok(CallOutput {
                    code,
                    out: format!("log line\n{out}\n"),
                    err: String::new(),
                })
            })
        })
    }

    #[test]
    fn request_validates_and_renames_fields() {
        let built = request(
            &json!({"prompt": "  go ", "machine": "linux8", "timeout_minutes": 30, "model": ""}),
            Some("t1"),
        )
        .unwrap();
        assert_eq!(
            built,
            json!({"prompt": "go", "machine": "linux8", "timeoutMinutes": 30, "from": "t1"})
        );
        assert!(request(&json!({"prompt": " "}), None).is_err());
        assert!(request(&json!({"prompt": "x", "timeout_minutes": 0}), None).is_err());
        assert!(request(&json!({"prompt": "x", "extra": 1}), None).is_err());
    }

    #[tokio::test]
    async fn run_waits_until_finished_and_reports_changes() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let call = scripted(
            vec![
                (0, record("0a1b2c3d", "queued", None)),
                (0, record("0a1b2c3d", "running", Some("linux8"))),
                (
                    0,
                    json!({"id":"0a1b2c3d","state":"done","machine":"linux8","answer":"42"})
                        .to_string(),
                ),
            ],
            Arc::clone(&seen),
        );
        let updates = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&updates);
        let done = run_fleet_task(
            &call,
            json!({"prompt":"p"}),
            None,
            &move |r: &FleetRecord| sink.lock().unwrap().push(progress_text(r)),
        )
        .await
        .unwrap();
        assert_eq!(
            fleet_result_text(&done),
            "fleet 0a1b2c3d finished on linux8.\n\n42"
        );
        assert_eq!(
            *updates.lock().unwrap(),
            vec![
                "fleet 0a1b2c3d queued",
                "fleet 0a1b2c3d running on linux8",
                "fleet 0a1b2c3d done on linux8"
            ]
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0][0], "start");
        assert_eq!(seen[1], vec!["wait", "0a1b2c3d", "100000"]);
    }

    #[tokio::test]
    async fn failed_start_surfaces_the_tool_error() {
        let call = scripted(vec![(2, String::new())], Arc::new(Mutex::new(Vec::new())));
        let error = run_fleet_task(&call, json!({"prompt":"p"}), None, &|_: &FleetRecord| {})
            .await
            .unwrap_err();
        assert!(error.starts_with("fleet-run start failed"));
    }

    #[test]
    fn unfinished_record_names_its_reason() {
        let record: FleetRecord =
            serde_json::from_value(json!({"id":"aa","state":"failed","error":"boom"})).unwrap();
        assert_eq!(
            fleet_result_text(&record),
            "fleet aa failed on 배치 전: boom"
        );
        assert_eq!(
            script_directory("/x/src/model-provider.ts"),
            Some(PathBuf::from("/x/src"))
        );
    }
}
