//! This device's operator endpoint as a `SessionHost`.
//!
//! Upstream counterpart: `thread/control/local-session-host.ts` (`createLocalSessionHost`, `workspaceNote`) and the folder
//! check and model refusal of `local-open.ts` (`openLocally`). The worktree `local-open.ts` also creates is not ported
//! (`worktree/mod.rs`).

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::addressing::{ThreadAddressEntry, workspace_entries};
use crate::thread::contracts_results::ThreadResultSummary;
use crate::thread::host_view::ThreadHost;
use crate::thread::initial_skills::INITIAL_SKILLS_CONTEXT_KEY;
use crate::thread::worktree::thread_worktree::check_folder;

use super::open_reply::model_refusal;
use super::port::{
    ControlError, ControlResult, SessionHost, SessionOpenOptions, SessionOpenRequest,
};
use super::session_host::opened_thread;

/// A thread this device opened in a folder outside the caller's workspace is `scope_denied` to the caller (its creator
/// included) unless `all_scope` is set; the result says so. A thread on another device belongs to the account, so it
/// never carries the note.
pub fn workspace_note(
    thread: &ThreadResultSummary,
    caller_workspace_root: &str,
) -> Vec<(&'static str, Value)> {
    if caller_workspace_root.is_empty() {
        return Vec::new();
    }
    let entry = ThreadAddressEntry {
        thread_id: thread.thread.clone(),
        name: thread.name.clone(),
        cwd: thread.cwd.clone(),
        status: thread.status,
        updated_at: thread.updated_at.clone(),
        session_ids: Vec::new(),
    };
    if !workspace_entries(&[entry], caller_workspace_root).is_empty() {
        return Vec::new();
    }
    vec![
        ("workspace_scope", json!("outside")),
        (
            "scope_next_action",
            json!(format!(
                "Thread {} is in {}, outside this session's workspace: pass all_scope: true when you address it, and to see it in thread_list (tool.thread_list({{ all_scope: true }}) in an eval cell).",
                thread.thread, thread.cwd
            )),
        ),
    ]
}

/// The fields of the engine's `open_session` frame a request names; the skills ride on its context, never as a field.
pub fn open_params(request: &SessionOpenRequest) -> Value {
    let mut params = Map::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            params.insert(key.to_owned(), Value::String(value.clone()));
        }
    };
    put("cwd", &request.cwd);
    put("sessionPath", &request.session_path);
    put("forkFrom", &request.fork_from);
    put("permissionPreset", &request.permission_preset);
    put("provider", &request.provider);
    put("modelId", &request.model_id);
    put("thinkingLevel", &request.thinking_level);
    put("durableSessionId", &request.durable_session_id);
    if !request.initial_skills.is_empty() {
        let mut context = Map::new();
        context.insert(
            INITIAL_SKILLS_CONTEXT_KEY.to_owned(),
            Value::String(json!(request.initial_skills).to_string()),
        );
        params.insert("context".to_owned(), Value::Object(context));
    }
    Value::Object(params)
}

/// The open of a thread on this device: the folder it names is checked first; a model the engine cannot run is the
/// caller's argument to fix.
pub async fn open_locally(
    host: &dyn ThreadHost,
    request: SessionOpenRequest,
    now: &str,
) -> ControlResult<ThreadResultSummary> {
    if let Some(cwd) = request.cwd.as_deref()
        && let Some(refused) = check_folder(cwd)
    {
        return Err(ControlError::Refused(Box::new(refused)));
    }
    let model = request
        .provider
        .as_deref()
        .map(|provider| format!("{provider}/{}", request.model_id.as_deref().unwrap_or("")));
    match host
        .open_host_session(open_params(&request), request.name.clone())
        .await
    {
        Ok(session) => Ok(opened_thread(&session, now)),
        Err(message) => match model_refusal(&message, model.as_deref()) {
            Some(refused) => Err(ControlError::Refused(Box::new(refused))),
            None => Err(ControlError::Transport(message)),
        },
    }
}

/// `visible` is the address book this call assembled; `open` is the host's `open_session`.
pub struct LocalSessionHost {
    host: Arc<dyn ThreadHost>,
    visible: Vec<ThreadAddressEntry>,
    now: String,
}

impl LocalSessionHost {
    pub fn new(host: Arc<dyn ThreadHost>, visible: Vec<ThreadAddressEntry>, now: String) -> Self {
        Self { host, visible, now }
    }
}

impl SessionHost for LocalSessionHost {
    fn device(&self) -> Option<&str> {
        None
    }

    fn visible(&self) -> BoxFuture<ControlResult<Vec<ThreadAddressEntry>>> {
        let visible = self.visible.clone();
        Box::pin(async move { Ok(visible) })
    }

    fn open(
        &self,
        request: SessionOpenRequest,
        _options: SessionOpenOptions,
    ) -> BoxFuture<ControlResult<ThreadResultSummary>> {
        let host = self.host.clone();
        let now = self.now.clone();
        Box::pin(async move { open_locally(host.as_ref(), request, &now).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::executor::block_on;

    use super::*;
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::host_view::{SessionCommandPort, ThreadHostSession, ThreadHostView};
    use crate::thread::types::GatewayEndpointKind;

    struct NoPort;

    impl SessionCommandPort for NoPort {
        fn send(&self, _command: Value) -> BoxFuture<Result<Value, String>> {
            Box::pin(async { Err("no port".to_owned()) })
        }
    }

    struct OpeningHost {
        opens: Mutex<Vec<(Value, Option<String>)>>,
        answer: Result<Value, String>,
    }

    impl ThreadHost for OpeningHost {
        fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>> {
            Box::pin(async { Ok(ThreadHostView::default()) })
        }

        fn session_port(&self, _session: &ThreadHostSession) -> Arc<dyn SessionCommandPort> {
            Arc::new(NoPort)
        }

        fn open_host_session(
            &self,
            params: Value,
            name: Option<String>,
        ) -> BoxFuture<Result<ThreadHostSession, String>> {
            self.opens.lock().expect("opens").push((params, name));
            let answer = self.answer.clone().map(|row| {
                ThreadHostSession::from_listing(
                    &row,
                    "/run/host.sock",
                    GatewayEndpointKind::RpcHost,
                    &[],
                )
                .expect("session")
            });
            Box::pin(async move { answer })
        }
    }

    fn host(answer: Result<Value, String>) -> Arc<OpeningHost> {
        Arc::new(OpeningHost {
            opens: Mutex::new(Vec::new()),
            answer,
        })
    }

    fn opened_row(cwd: &str) -> Value {
        json!({
            "sessionId": "rpc-5",
            "durableSessionId": "dur-new",
            "cwd": cwd,
            "name": "child",
            "status": "open",
            "activity": "running",
        })
    }

    #[test]
    fn a_request_becomes_the_open_session_frame_with_the_skills_on_its_context() {
        let request = SessionOpenRequest {
            cwd: Some("/work".into()),
            session_path: Some("/sessions/a.jsonl".into()),
            name: Some("kept out of the frame".into()),
            initial_skills: vec!["programming".into(), "gateway-lead".into()],
            permission_preset: Some("ask".into()),
            provider: Some("anthropic".into()),
            model_id: Some("opus".into()),
            thinking_level: Some("high".into()),
            durable_session_id: Some("dur-1".into()),
            ..SessionOpenRequest::default()
        };
        assert_eq!(
            open_params(&request),
            json!({
                "cwd": "/work",
                "sessionPath": "/sessions/a.jsonl",
                "permissionPreset": "ask",
                "provider": "anthropic",
                "modelId": "opus",
                "thinkingLevel": "high",
                "durableSessionId": "dur-1",
                "context": { "initial_skills": "[\"programming\",\"gateway-lead\"]" },
            })
        );
        assert_eq!(open_params(&SessionOpenRequest::default()), json!({}));
    }

    #[test]
    fn an_open_answers_the_thread_the_host_made_and_passes_the_name_beside_the_frame() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().into_owned();
        let opening = host(Ok(opened_row(&cwd)));
        let local = LocalSessionHost::new(
            opening.clone(),
            Vec::new(),
            "2026-11-26T00:00:00.000Z".into(),
        );
        let request = SessionOpenRequest {
            cwd: Some(cwd.clone()),
            name: Some("child".into()),
            ..SessionOpenRequest::default()
        };
        let opened = block_on(local.open(request, SessionOpenOptions::default())).expect("opened");
        assert_eq!(opened.thread, "dur-new");
        assert_eq!(opened.name, "child");
        assert_eq!(opened.activity, None);
        let opens = opening.opens.lock().expect("opens");
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].0, json!({ "cwd": cwd }));
        assert_eq!(opens[0].1.as_deref(), Some("child"));
        assert_eq!(local.device(), None);
    }

    #[test]
    fn a_folder_that_is_missing_refuses_before_the_host_is_asked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("gone").to_string_lossy().into_owned();
        let opening = host(Ok(opened_row(&gone)));
        let local =
            LocalSessionHost::new(opening.clone(), Vec::new(), "2026-11-26T00:00:00Z".into());
        let request = SessionOpenRequest {
            cwd: Some(gone),
            ..SessionOpenRequest::default()
        };
        let Err(ControlError::Refused(refused)) =
            block_on(local.open(request, SessionOpenOptions::default()))
        else {
            panic!("expected a refusal");
        };
        assert_eq!(refused.code.as_str(), "invalid_arguments");
        assert_eq!(
            refused.details.expect("details")["reason"],
            "folder_missing"
        );
        assert!(opening.opens.lock().expect("opens").is_empty());
    }

    #[test]
    fn a_model_the_engine_cannot_run_is_model_not_found_and_any_other_failure_is_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().into_owned();
        let request = SessionOpenRequest {
            cwd: Some(cwd),
            provider: Some("anthropic".into()),
            model_id: Some("nope".into()),
            ..SessionOpenRequest::default()
        };
        let refusing = LocalSessionHost::new(
            host(Err("model_unavailable: no model anthropic/nope".into())),
            Vec::new(),
            "2026-11-26T00:00:00Z".into(),
        );
        let Err(ControlError::Refused(refused)) =
            block_on(refusing.open(request.clone(), SessionOpenOptions::default()))
        else {
            panic!("expected a refusal");
        };
        assert_eq!(refused.code.as_str(), "model_not_found");
        assert_eq!(
            refused.details.expect("details"),
            json!({ "reason": "model_unavailable", "model": "anthropic/nope" })
        );
        let failing = LocalSessionHost::new(
            host(Err("connection closed".into())),
            Vec::new(),
            "2026-11-26T00:00:00Z".into(),
        );
        assert_eq!(
            block_on(failing.open(request, SessionOpenOptions::default())).expect_err("failed"),
            ControlError::Transport("connection closed".into())
        );
    }

    #[test]
    fn a_thread_outside_the_callers_workspace_says_so_and_one_inside_does_not() {
        let inside = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let thread = |cwd: &std::path::Path| ThreadResultSummary {
            thread: "dur-1".into(),
            name: "x".into(),
            unnamed: None,
            status: ThreadStatus::Live,
            state: crate::thread::contracts_state::ThreadState::Idle,
            model: None,
            activity: None,
            error_code: None,
            turn_id: None,
            waiting_on: None,
            engine_version: None,
            backlog: None,
            as_of: None,
            machine: None,
            cwd: cwd.to_string_lossy().into_owned(),
            created_at: "2026-11-26T00:00:00Z".into(),
            updated_at: None,
            link: String::new(),
            device: None,
            alias: None,
            surface: None,
            controls: None,
            error_note: None,
            skills: None,
        };
        let root = inside.path().to_string_lossy().into_owned();
        assert!(workspace_note(&thread(inside.path()), &root).is_empty());
        assert!(workspace_note(&thread(outside.path()), "").is_empty());
        let note = workspace_note(&thread(outside.path()), &root);
        assert_eq!(note[0], ("workspace_scope", json!("outside")));
        assert_eq!(note[1].0, "scope_next_action");
        assert!(
            note[1]
                .1
                .as_str()
                .is_some_and(|text| text.contains("pass all_scope: true"))
        );
    }
}
