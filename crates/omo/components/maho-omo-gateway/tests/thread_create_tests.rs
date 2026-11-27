//! `thread_create` over a faux device (omo v5.1.80..v5.1.83 create rows): the shape checks, placement, the name check, the
//! open, a reopen from a session file, the creator record, and the first prompt with each of its failure reasons. The
//! surface's sender and its wait for the prompt to start are scripted seams, so nothing here sleeps or waits on a clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use serde_json::{Value, json};

use maho_omo_gateway::gateway_database_path;
use maho_omo_gateway::thread::adapter::{
    AdmitResult, BoxFuture, ExternalAdmissionKind, GatewayEndpointPort, RuntimePhase,
    SessionAdmissionGate, SessionRuntimePort,
};
use maho_omo_gateway::thread::address_book::{
    AddressBookHost, HostListSessions, HostSessionStatus,
};
use maho_omo_gateway::thread::addressing::{ThreadAddressEntry, check_name_conflict};
use maho_omo_gateway::thread::attachment_fence::RpcTransport;
use maho_omo_gateway::thread::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
};
use maho_omo_gateway::thread::contracts_create_params::ThreadCreateInput;
use maho_omo_gateway::thread::contracts_results::{ThreadDelivery, ThreadStatus};
use maho_omo_gateway::thread::control::create::{
    CreatePromptSend, CreateSurface, PromptDeliveryError, Retrying, create_thread,
};
use maho_omo_gateway::thread::control::locate::{ThreadCall, book_of};
use maho_omo_gateway::thread::control::port::{ControlError, ControlResult};
use maho_omo_gateway::thread::control::prompt_start::PromptStart;
use maho_omo_gateway::thread::control::session_host::created_thread_id;
use maho_omo_gateway::thread::engine::GatewayResolution;
use maho_omo_gateway::thread::errors::{
    THREAD_ERROR_CODES, gateway_schema_refusal, thread_tool_failure,
};
use maho_omo_gateway::thread::host_view::{
    SessionCommandPort, ThreadHost, ThreadHostSession, ThreadHostView,
};
use maho_omo_gateway::thread::live_surface::{LiveThreadSurface, LiveThreadSurfaceOptions};
use maho_omo_gateway::thread::store::{GatewayStoreOptions, create_gateway_store};
use maho_omo_gateway::thread::thread_row::opened_result;
use maho_omo_gateway::thread::types::{
    AdmissionLedger, DeliveryWait, GatewayEndpointKind, Provenance,
};

const CALLER: &str = "0aa00001-0000-4000-8000-000000000001";
const LOCAL: &str = "0bb00001-0000-4000-8000-000000000002";
const SOCKET: &str = "/run/fake.sock";
const NOW: &str = "2026-11-26T00:00:00.000Z";
const EXITED: &str = "01a1e90d-b8c2-72c1-ac4a-036aebcfdcab";

fn host_session(route: &str, durable: &str, name: Option<&str>, cwd: &str) -> ThreadHostSession {
    ThreadHostSession {
        session_id: route.to_owned(),
        durable_session_id: Some(durable.to_owned()),
        session_path: None,
        cwd: cwd.to_owned(),
        name: name.map(str::to_owned),
        status: Some(HostSessionStatus::Open),
        kind: None,
        archived: false,
        activity: None,
        error_code: None,
        turn_id: None,
        model: None,
        waiting: None,
        engine_version: None,
        created_at: Some("2026-11-01T00:00:00.000Z".to_owned()),
        updated_at: Some("2026-11-02T00:00:00.000Z".to_owned()),
        endpoint_kind: GatewayEndpointKind::RpcHost,
        controls: Vec::new(),
        socket: SOCKET.to_owned(),
    }
}

struct NamePort {
    names: Arc<Mutex<Vec<(String, String)>>>,
}

impl SessionCommandPort for NamePort {
    fn send(&self, command: Value) -> BoxFuture<Result<Value, String>> {
        if command["type"] == "set_session_name" {
            let route = command["sessionId"].as_str().unwrap_or_default().to_owned();
            let name = command["name"].as_str().unwrap_or_default().to_owned();
            self.names.lock().expect("names").push((route, name));
        }
        Box::pin(async {
            Ok(json!({ "type": "response", "command": "set_session_name", "success": true }))
        })
    }
}

struct FakeHost {
    sessions: Mutex<Vec<ThreadHostSession>>,
    opens: Mutex<Vec<(Value, Option<String>)>>,
    names: Arc<Mutex<Vec<(String, String)>>>,
    open_failure: Mutex<Option<String>>,
    lose_reply: Mutex<bool>,
    capabilities: Mutex<Vec<String>>,
}

impl ThreadHost for FakeHost {
    fn list_view(&self) -> BoxFuture<Result<ThreadHostView, String>> {
        let sessions = self.sessions.lock().expect("sessions").clone();
        let host = AddressBookHost {
            socket: SOCKET.to_owned(),
            list_sessions: Some(HostListSessions::ok(
                sessions
                    .iter()
                    .map(ThreadHostSession::host_session)
                    .collect(),
            )),
            endpoint_kind: Some(GatewayEndpointKind::RpcHost),
            alive: None,
            reason: None,
            legacy: true,
        };
        let view = ThreadHostView {
            sessions,
            hosts: vec![host],
            disk: Vec::new(),
        };
        Box::pin(async move { Ok(view) })
    }

    fn session_port(&self, _session: &ThreadHostSession) -> Arc<dyn SessionCommandPort> {
        Arc::new(NamePort {
            names: self.names.clone(),
        })
    }

    fn open_host_session(
        &self,
        params: Value,
        name: Option<String>,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        self.opens
            .lock()
            .expect("opens")
            .push((params.clone(), name.clone()));
        let failure = self.open_failure.lock().expect("failure").clone();
        if let Some(message) = failure {
            return Box::pin(async move { Err(message) });
        }
        let mut sessions = self.sessions.lock().expect("sessions");
        let from_file = params
            .get("sessionPath")
            .and_then(Value::as_str)
            .and_then(|path| Path::new(path).file_stem())
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.rsplit('_').next())
            .map(str::to_owned);
        let durable = from_file
            .or_else(|| {
                params
                    .get("durableSessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("dur-new-{}", sessions.len() + 1));
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("/")
            .to_owned();
        let session = host_session(
            &format!("rpc-{}", sessions.len() + 1),
            &durable,
            name.as_deref(),
            &cwd,
        );
        sessions.push(session.clone());
        if *self.lose_reply.lock().expect("lose") {
            return Box::pin(async { Err("connection closed".to_owned()) });
        }
        Box::pin(async move { Ok(session) })
    }

    fn capabilities(&self) -> BoxFuture<Result<Vec<String>, String>> {
        let capabilities = self.capabilities.lock().expect("capabilities").clone();
        Box::pin(async move { Ok(capabilities) })
    }
}

struct FakeSurface {
    operation_id: String,
    caller_id: String,
    caller_cwd: String,
    sessions_directory: Option<PathBuf>,
    preset: Option<String>,
    retrying: Option<Retrying>,
    host: Arc<FakeHost>,
    deliveries: Mutex<Vec<(CreatePromptSend, String)>>,
    delivery_answer: Mutex<Result<Value, PromptDeliveryError>>,
    start_answer: Mutex<Result<PromptStart, String>>,
    waited: Mutex<Vec<String>>,
    creations: Mutex<Vec<(String, String)>>,
}

impl CreateSurface for FakeSurface {
    fn operation_id(&self) -> &str {
        &self.operation_id
    }

    fn caller_id(&self) -> &str {
        &self.caller_id
    }

    fn permission_preset(&self) -> Option<String> {
        self.preset.clone()
    }

    fn retrying(&self) -> Option<Retrying> {
        self.retrying.clone()
    }

    fn view(&self) -> BoxFuture<ControlResult<ThreadCall>> {
        let listing = self.host.list_view();
        let host: Arc<dyn ThreadHost> = self.host.clone();
        let book_surface = (
            self.caller_id.clone(),
            self.caller_cwd.clone(),
            self.sessions_directory.clone(),
        );
        Box::pin(async move {
            let view = listing.await.map_err(ControlError::Transport)?;
            let book = book_of(&view, &[], NOW);
            Ok(ThreadCall {
                host,
                view,
                book,
                caller_id: book_surface.0,
                caller_workspace_root: book_surface.1,
                sessions_directory: book_surface.2,
                remote: None,
                now: NOW.to_owned(),
            })
        })
    }

    fn deliver_prompt(
        &self,
        send: CreatePromptSend,
        idempotency_key: String,
    ) -> BoxFuture<Result<Value, PromptDeliveryError>> {
        self.deliveries
            .lock()
            .expect("deliveries")
            .push((send, idempotency_key));
        let answer = self.delivery_answer.lock().expect("answer").clone();
        Box::pin(async move { answer })
    }

    fn await_prompt_start(&self, delivery_id: String) -> BoxFuture<Result<PromptStart, String>> {
        self.waited.lock().expect("waited").push(delivery_id);
        let answer = self.start_answer.lock().expect("start").clone();
        Box::pin(async move { answer })
    }

    fn record_creation(&self, creator: &str, created: &str) -> Result<(), String> {
        self.creations
            .lock()
            .expect("creations")
            .push((creator.to_owned(), created.to_owned()));
        Ok(())
    }
}

fn sent_answer(delivery: Value) -> Value {
    json!({
        "kind": "ok",
        "thread": "unused",
        "link": "unused",
        "delivery": delivery,
        "message_seq": 1,
        "deduplicated": false,
        "delivery_id": "d-1",
        "effective_mode": "start",
    })
}

struct Fixture {
    workspace: tempfile::TempDir,
    host: Arc<FakeHost>,
    surface: FakeSurface,
}

fn fixture() -> Fixture {
    let workspace = tempfile::tempdir().expect("workspace");
    let cwd = workspace.path().to_string_lossy().into_owned();
    let host = Arc::new(FakeHost {
        sessions: Mutex::new(vec![
            host_session("route-caller", CALLER, Some("caller"), &cwd),
            host_session("route-local", LOCAL, Some("local peer"), &cwd),
        ]),
        opens: Mutex::new(Vec::new()),
        names: Arc::new(Mutex::new(Vec::new())),
        open_failure: Mutex::new(None),
        lose_reply: Mutex::new(false),
        capabilities: Mutex::new(Vec::new()),
    });
    let surface = FakeSurface {
        operation_id: "call:call-1".to_owned(),
        caller_id: CALLER.to_owned(),
        caller_cwd: cwd,
        sessions_directory: None,
        preset: None,
        retrying: None,
        host: host.clone(),
        deliveries: Mutex::new(Vec::new()),
        delivery_answer: Mutex::new(Ok(sent_answer(
            json!({ "kind": "started", "turn_id": "1" }),
        ))),
        start_answer: Mutex::new(Ok(PromptStart::NotStarted {
            state: "queued".to_owned(),
        })),
        waited: Mutex::new(Vec::new()),
        creations: Mutex::new(Vec::new()),
    };
    Fixture {
        workspace,
        host,
        surface,
    }
}

fn run(fixture: &Fixture, args: Value) -> Value {
    let input = ThreadCreateInput::parse(&args).expect("the arguments parse");
    let call = block_on(fixture.surface.view()).expect("view");
    block_on(create_thread(&call, &input, &fixture.surface)).expect("the create answers")
}

fn opens(fixture: &Fixture) -> Vec<(Value, Option<String>)> {
    fixture.host.opens.lock().expect("opens").clone()
}

fn failure_code(result: &Value) -> &str {
    assert_eq!(result["kind"], "error", "{result}");
    result["error"]["code"].as_str().unwrap_or_default()
}

fn derived_id() -> String {
    created_thread_id(CALLER, "call:call-1")
}

fn write_session_file(sessions: &Path, id: &str, cwd: &str, name: Option<&str>) -> PathBuf {
    let directory = sessions.join("--exited--");
    std::fs::create_dir_all(&directory).expect("session directory");
    let file = directory.join(format!("2026-11-17T10-29-13-026Z_{id}.jsonl"));
    let mut header = json!({ "type": "session", "version": 3, "sessionId": id, "cwd": cwd });
    if let Some(name) = name {
        header["name"] = json!(name);
    }
    std::fs::write(&file, format!("{header}\n")).expect("session file");
    file
}

#[test]
fn a_named_create_opens_one_session_in_the_callers_folder_and_answers_the_thread() {
    let fixture = fixture();
    let created = run(&fixture, json!({ "name": "builder" }));
    let id = derived_id();
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["thread"], id);
    assert_eq!(created["name"], "builder");
    assert_eq!(created["status"], "live");
    assert_eq!(created["state"], "idle");
    assert_eq!(created["deduplicated"], false);
    assert_eq!(created.get("prompt"), Some(&Value::Null));
    assert!(created.get("workspace_scope").is_none());
    assert!(
        created["link"]
            .as_str()
            .is_some_and(|link| link.starts_with("[builder]("))
    );
    let made = opens(&fixture);
    assert_eq!(made.len(), 1);
    assert_eq!(
        made[0].0,
        json!({ "cwd": fixture.surface.caller_cwd, "durableSessionId": id })
    );
    assert_eq!(made[0].1.as_deref(), Some("builder"));
    assert_eq!(
        *fixture.surface.creations.lock().expect("creations"),
        vec![(CALLER.to_owned(), id)]
    );
}

#[test]
fn a_second_run_under_the_same_key_finds_the_thread_it_made_and_opens_nothing() {
    let fixture = fixture();
    let first = run(&fixture, json!({ "name": "replay" }));
    let second = run(&fixture, json!({ "name": "replay" }));
    assert_eq!(first["deduplicated"], false, "{first}");
    assert_eq!(second["kind"], "ok", "{second}");
    assert_eq!(second["deduplicated"], true);
    assert_eq!(second["thread"], first["thread"]);
    assert_eq!(opens(&fixture).len(), 1);
}

#[test]
fn a_name_another_thread_carries_is_a_name_conflict_and_nothing_opens() {
    let fixture = fixture();
    let refused = run(&fixture, json!({ "name": "Local Peer" }));
    assert_eq!(failure_code(&refused), "name_conflict");
    assert_eq!(
        refused["error"]["details"]["candidates"][0]["thread"],
        LOCAL
    );
    assert!(opens(&fixture).is_empty());
}

#[test]
fn a_create_with_no_prompt_and_no_name_or_with_a_blank_prompt_is_refused_before_anything_opens() {
    let fixture = fixture();
    for args in [json!({}), json!({ "name": "   " }), json!({ "alias": " " })] {
        let refused = run(&fixture, args.clone());
        assert_eq!(failure_code(&refused), "invalid_arguments", "{args}");
        assert!(refused["error"]["details"].is_null(), "{args}");
    }
    for args in [
        json!({ "prompt": "   " }),
        json!({ "name": "named", "prompt": "\n\t" }),
    ] {
        let refused = run(&fixture, args.clone());
        assert_eq!(failure_code(&refused), "invalid_arguments", "{args}");
        assert_eq!(refused["error"]["details"]["field"], "prompt", "{args}");
    }
    assert!(opens(&fixture).is_empty());
}

#[test]
fn launch_options_ride_the_open_and_a_wider_approval_mode_is_refused() {
    let mut fixture = fixture();
    fixture.surface.operation_id = "call:narrow".to_owned();
    fixture.surface.preset = Some("workspace".to_owned());
    let wider = run(
        &fixture,
        json!({ "name": "wide", "permission_preset": "full-access" }),
    );
    assert_eq!(failure_code(&wider), "scope_denied");
    assert_eq!(
        wider["error"]["details"],
        json!({ "reason": "permission_preset_wider", "requested": "full-access", "creator": "workspace" })
    );
    assert!(opens(&fixture).is_empty());

    let narrower = run(
        &fixture,
        json!({
            "name": "narrow",
            "permission_preset": "read-only",
            "model": "anthropic/claude-opus-5-5",
            "reasoning": "high",
        }),
    );
    assert_eq!(narrower["kind"], "ok", "{narrower}");
    let made = opens(&fixture);
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].0["permissionPreset"], "read-only");
    assert_eq!(made[0].0["provider"], "anthropic");
    assert_eq!(made[0].0["modelId"], "claude-opus-5-5");
    assert_eq!(made[0].0["thinkingLevel"], "high");

    fixture.surface.operation_id = "call:inherit".to_owned();
    let inherited = run(&fixture, json!({ "name": "inherit" }));
    assert_eq!(inherited["kind"], "ok", "{inherited}");
    assert_eq!(opens(&fixture)[1].0["permissionPreset"], "workspace");
}

#[test]
fn a_model_that_is_not_provider_slash_id_and_launch_fields_with_a_thread_are_refused() {
    let fixture = fixture();
    for model in ["opus", "/opus", "anthropic/"] {
        let refused = run(&fixture, json!({ "name": "m", "model": model }));
        assert_eq!(failure_code(&refused), "invalid_arguments", "{model}");
    }
    let existing = run(
        &fixture,
        json!({ "thread": LOCAL, "model": "anthropic/opus" }),
    );
    assert_eq!(failure_code(&existing), "invalid_arguments");
    assert_eq!(existing["error"]["details"]["fields"], json!(["model"]));
    assert!(opens(&fixture).is_empty());
}

#[test]
fn a_model_the_engine_cannot_run_is_model_not_found_and_no_thread_is_left_behind() {
    let fixture = fixture();
    *fixture.host.open_failure.lock().expect("failure") =
        Some("model_unavailable: no model anthropic/nope".to_owned());
    let refused = run(
        &fixture,
        json!({ "name": "nope", "model": "anthropic/nope" }),
    );
    assert_eq!(failure_code(&refused), "model_not_found");
    assert_eq!(
        refused["error"]["details"],
        json!({ "reason": "model_unavailable", "model": "anthropic/nope" })
    );
    assert_eq!(fixture.host.sessions.lock().expect("sessions").len(), 2);
}

#[test]
fn a_live_thread_is_answered_deduplicated_without_an_open_and_a_prompt_goes_to_it() {
    let fixture = fixture();
    let opened = run(&fixture, json!({ "thread": LOCAL }));
    assert_eq!(opened["kind"], "ok", "{opened}");
    assert_eq!(opened["thread"], LOCAL);
    assert_eq!(opened["name"], "local peer");
    assert_eq!(opened["deduplicated"], true);
    assert!(opens(&fixture).is_empty());
    assert!(
        fixture
            .surface
            .creations
            .lock()
            .expect("creations")
            .is_empty()
    );
    assert!(
        fixture
            .surface
            .deliveries
            .lock()
            .expect("deliveries")
            .is_empty()
    );

    let prompted = run(&fixture, json!({ "thread": LOCAL, "prompt": "carry on" }));
    assert_eq!(prompted["kind"], "ok", "{prompted}");
    assert_eq!(prompted["deduplicated"], true);
    assert_eq!(prompted["prompt"]["delivery"]["kind"], "started");
    assert!(opens(&fixture).is_empty());
    let sent = fixture.surface.deliveries.lock().expect("deliveries");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.thread, LOCAL);
    assert_eq!(sent[0].0.message, "carry on");
    assert_eq!(sent[0].1, "call:call-1:prompt");
}

#[test]
fn a_session_file_is_reopened_naming_its_folder_and_keeping_the_name_it_had() {
    let mut fixture = fixture();
    let sessions = fixture.workspace.path().join("sessions");
    let cwd = fixture.surface.caller_cwd.clone();
    let file = write_session_file(&sessions, EXITED, &cwd, Some("fix the build"));
    fixture.surface.sessions_directory = Some(sessions);

    let reopened = run(&fixture, json!({ "thread": EXITED }));
    assert_eq!(reopened["kind"], "ok", "{reopened}");
    assert_eq!(reopened["thread"], EXITED);
    assert_eq!(reopened["deduplicated"], false);
    assert_eq!(reopened["name"], "fix the build");
    let made = opens(&fixture);
    assert_eq!(made.len(), 1);
    assert_eq!(
        made[0].0,
        json!({ "sessionPath": file.to_string_lossy(), "cwd": cwd })
    );
    assert_eq!(made[0].1, None);
    assert_eq!(
        *fixture.host.names.lock().expect("names"),
        vec![("rpc-3".to_owned(), "fix the build".to_owned())]
    );
    assert!(
        fixture
            .surface
            .creations
            .lock()
            .expect("creations")
            .is_empty()
    );
}

#[test]
fn a_name_given_at_reopen_is_applied_and_one_another_thread_carries_is_a_warning_not_an_error() {
    let mut fixture = fixture();
    let sessions = fixture.workspace.path().join("sessions");
    let cwd = fixture.surface.caller_cwd.clone();
    write_session_file(&sessions, EXITED, &cwd, Some("fix the build"));
    fixture.surface.sessions_directory = Some(sessions);

    let named = run(&fixture, json!({ "thread": EXITED, "name": "x" }));
    assert_eq!(named["kind"], "ok", "{named}");
    assert_eq!(named["name"], "x");
    assert_eq!(
        *fixture.host.names.lock().expect("names"),
        vec![("rpc-3".to_owned(), "x".to_owned())]
    );
    fixture.host.names.lock().expect("names").clear();
    fixture.host.sessions.lock().expect("sessions").pop();

    let taken = run(&fixture, json!({ "thread": EXITED, "name": "local peer" }));
    assert_eq!(taken["kind"], "ok", "{taken}");
    let warning = &taken["warnings"][0];
    assert_eq!(warning["code"], "name_conflict", "{taken}");
    assert_eq!(warning["details"]["reason"], "name_not_applied");
    assert_eq!(warning["details"]["name"], "local peer");
    assert!(fixture.host.names.lock().expect("names").is_empty());
}

#[test]
fn an_id_no_device_holds_is_not_found_and_nothing_opens() {
    let mut fixture = fixture();
    let sessions = fixture.workspace.path().join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    fixture.surface.sessions_directory = Some(sessions);
    for args in [
        json!({ "thread": "01a1e90d-0000-7000-8000-000000000000" }),
        json!({ "thread": "nobody" }),
    ] {
        let refused = run(&fixture, args.clone());
        assert_eq!(failure_code(&refused), "not_found", "{args}");
    }
    assert!(opens(&fixture).is_empty());
}

#[test]
fn a_create_whose_reply_was_lost_finds_its_thread_and_prompts_it_once() {
    let fixture = fixture();
    *fixture.host.lose_reply.lock().expect("lose") = true;
    let created = run(
        &fixture,
        json!({ "name": "lost-reply", "prompt": "build it" }),
    );
    let id = derived_id();
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["thread"], id);
    assert_eq!(created["deduplicated"], false);
    assert_eq!(opens(&fixture).len(), 1);
    assert_eq!(
        fixture.surface.deliveries.lock().expect("deliveries").len(),
        1
    );
}

#[test]
fn a_retry_runs_again_only_when_the_thread_its_key_derives_is_here() {
    let mut fixture = fixture();
    fixture.surface.retrying = Some(Retrying {
        error_note: Some("the call failed after the host opened the session".to_owned()),
    });
    let uncertain = run(&fixture, json!({ "name": "retried" }));
    assert_eq!(failure_code(&uncertain), "idempotency_uncertain");
    assert_eq!(
        uncertain["error"]["details"]["error_note"],
        "the call failed after the host opened the session"
    );
    assert!(opens(&fixture).is_empty());

    fixture
        .host
        .sessions
        .lock()
        .expect("sessions")
        .push(host_session(
            "route-made",
            &derived_id(),
            Some("retried"),
            "/work",
        ));
    let found = run(&fixture, json!({ "name": "retried", "prompt": "build it" }));
    assert_eq!(found["kind"], "ok", "{found}");
    assert_eq!(found["thread"], derived_id());
    assert_eq!(found["deduplicated"], true);
    assert!(opens(&fixture).is_empty());
    assert_eq!(
        fixture.surface.deliveries.lock().expect("deliveries").len(),
        1
    );
}

#[test]
fn skills_are_checked_before_anything_opens_and_ride_the_open_context() {
    let fixture = fixture();
    let invalid = run(
        &fixture,
        json!({ "name": "s", "skills": ["programming", "../etc/passwd"] }),
    );
    assert_eq!(failure_code(&invalid), "invalid_arguments");
    assert_eq!(
        invalid["error"]["details"]["invalid"],
        json!(["../etc/passwd"])
    );
    assert!(opens(&fixture).is_empty());

    let created = run(
        &fixture,
        json!({ "name": "s", "skills": ["programming", "programming"] }),
    );
    assert_eq!(
        created["requested_skills"],
        json!(["programming"]),
        "{created}"
    );
    assert_eq!(
        opens(&fixture)[0].0["context"],
        json!({ "initial_skills": "[\"programming\"]" })
    );
}

#[test]
fn an_alias_with_no_account_is_only_the_local_name_and_a_given_name_wins() {
    let mut fixture = fixture();
    let aliased = run(&fixture, json!({ "alias": "bee" }));
    assert_eq!(aliased["kind"], "ok", "{aliased}");
    assert_eq!(aliased["name"], "bee");
    assert_eq!(aliased["alias_scope"], "local");
    assert_eq!(aliased["alias_unavailable"], "no_remote");
    assert_eq!(opens(&fixture)[0].1.as_deref(), Some("bee"));

    fixture.surface.operation_id = "call:both".to_owned();
    let both = run(&fixture, json!({ "alias": "wasp", "name": "hornet" }));
    assert_eq!(both["name"], "hornet", "{both}");
    assert_eq!(both["alias_scope"], "local");

    let reopening = run(&fixture, json!({ "thread": LOCAL, "alias": "bee" }));
    assert_eq!(failure_code(&reopening), "host_unavailable");
}

#[test]
fn another_device_a_fork_and_a_worktree_are_refused_before_anything_opens() {
    let fixture = fixture();
    let device = run(&fixture, json!({ "name": "d", "device": "studio" }));
    assert_eq!(failure_code(&device), "host_unavailable");
    let with_device = run(&fixture, json!({ "thread": LOCAL, "device": "b" }));
    assert_eq!(failure_code(&with_device), "invalid_arguments");
    let both = run(&fixture, json!({ "thread": LOCAL, "fork_from": LOCAL }));
    assert_eq!(failure_code(&both), "invalid_arguments");
    let fork = run(&fixture, json!({ "fork_from": LOCAL }));
    assert_eq!(failure_code(&fork), "unsupported");
    assert_eq!(
        fork["error"]["details"],
        json!({ "reason": "fork_unsupported", "thread": LOCAL })
    );
    let opening_worktree = run(&fixture, json!({ "thread": LOCAL, "worktree": {} }));
    assert_eq!(failure_code(&opening_worktree), "invalid_arguments");
    let worktree = run(
        &fixture,
        json!({ "name": "w", "worktree": { "branch": "feat/x" } }),
    );
    assert_eq!(failure_code(&worktree), "unsupported");
    assert_eq!(
        worktree["error"]["details"]["reason"],
        "worktree_unsupported"
    );
    assert!(opens(&fixture).is_empty());
}

// thread-create-fork.test.ts (#13031): an engine that advertises open_session_fork forks on open_session { forkFrom }; the
// create sends it the source and answers the fork, a new thread, never a refusal and never a blank session.
#[test]
fn a_fork_on_an_engine_that_forks_opens_with_the_source_and_answers_a_new_thread() {
    let fixture = fixture();
    *fixture.host.capabilities.lock().expect("capabilities") =
        vec!["unload_session".to_owned(), "open_session_fork".to_owned()];

    let forked = run(&fixture, json!({ "fork_from": LOCAL }));

    assert_eq!(forked["kind"], "ok", "{forked}");
    assert_eq!(forked["thread"], derived_id());
    assert_ne!(forked["thread"], LOCAL);
    let opened = opens(&fixture);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0["forkFrom"], LOCAL);
    assert_eq!(opened[0].0["durableSessionId"], derived_id());
}

// thread-create-fork.test.ts (#13031), on this device: a source the forking engine does not hold is refused not_found with
// the engine's reason, naming the device that holds the source as the way out.
#[test]
fn a_source_the_forking_engine_does_not_hold_is_not_found_with_the_engines_reason() {
    let fixture = fixture();
    *fixture.host.capabilities.lock().expect("capabilities") = vec!["open_session_fork".to_owned()];
    *fixture.host.open_failure.lock().expect("failure") =
        Some("thread RPC request failed: \"fork_source_not_local\"".to_owned());

    let refused = run(&fixture, json!({ "fork_from": EXITED }));

    assert_eq!(failure_code(&refused), "not_found");
    assert_eq!(
        refused["error"]["details"],
        json!({ "reason": "fork_source_not_local", "thread": EXITED })
    );
    assert!(
        refused["error"]["next_action"]
            .as_str()
            .is_some_and(|next| next.contains("device that holds its source")),
        "{refused}"
    );
    assert_eq!(opens(&fixture)[0].0["forkFrom"], EXITED);
}

#[test]
fn a_folder_that_does_not_exist_refuses_and_a_relative_one_resolves_against_the_callers_folder() {
    let fixture = fixture();
    let missing = run(&fixture, json!({ "name": "f", "cwd": "missing" }));
    assert_eq!(failure_code(&missing), "invalid_arguments");
    assert_eq!(missing["error"]["details"]["reason"], "folder_missing");
    assert!(opens(&fixture).is_empty());

    // A workspace is a git work tree (upstream `sameWorkspace`): a folder under a root that holds no `.git` is outside
    // it, so the caller's folder is marked as one for the folder below to belong to it.
    std::fs::create_dir(fixture.workspace.path().join(".git")).expect("git marker");
    std::fs::create_dir(fixture.workspace.path().join("sub")).expect("sub");
    let relative = run(&fixture, json!({ "name": "f", "cwd": "sub" }));
    assert_eq!(relative["kind"], "ok", "{relative}");
    let expected = fixture.workspace.path().join("sub");
    assert_eq!(
        opens(&fixture)[0].0["cwd"].as_str().map(Path::new),
        Some(expected.as_path())
    );
    assert_eq!(relative.get("workspace_scope"), None);
}

#[test]
fn a_thread_made_outside_the_callers_workspace_says_so() {
    let fixture = fixture();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let cwd = elsewhere.path().to_string_lossy().into_owned();
    let created = run(&fixture, json!({ "name": "away", "cwd": cwd }));
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["workspace_scope"], "outside");
    assert!(
        created["scope_next_action"]
            .as_str()
            .is_some_and(|action| action.contains("pass all_scope: true"))
    );
}

#[test]
fn a_first_prompt_that_started_or_steered_stands_as_the_delivery_the_send_answered() {
    let fixture = fixture();
    let created = run(&fixture, json!({ "name": "go", "prompt": "build it" }));
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(
        created["prompt"],
        json!({
            "delivery": { "kind": "started", "turn_id": "1" },
            "deduplicated": false,
            "delivery_id": "d-1",
            "effective_mode": "start",
        })
    );
    let sent = fixture.surface.deliveries.lock().expect("deliveries");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.thread, derived_id());
    assert_eq!(sent[0].0.message, "build it");
    assert_eq!(sent[0].0.delivery, None);
    assert_eq!(sent[0].1, "call:call-1:prompt");
    assert!(fixture.surface.waited.lock().expect("waited").is_empty());
}

#[test]
fn a_prompt_stored_for_a_wake_is_queued_with_a_next_action_not_to_send_it_again() {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") = Ok(sent_answer(
        json!({ "kind": "queued", "queue_position": 1, "waiting_on": { "kind": "wake" } }),
    ));
    let created = run(&fixture, json!({ "name": "wake", "prompt": "carry on" }));
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["prompt"]["delivery"]["waiting_on"]["kind"], "wake");
    let next = created["prompt"]["next_action"]
        .as_str()
        .unwrap_or_default();
    assert!(next.contains("do not send it again"), "{next}");
    assert!(next.contains(&derived_id()), "{next}");
    assert!(fixture.surface.waited.lock().expect("waited").is_empty());
}

#[test]
fn a_prompt_not_yet_admitted_waits_for_its_start_and_answers_what_it_became() {
    let mut fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") = Ok(sent_answer(
        json!({ "kind": "queued", "queue_position": 2 }),
    ));
    *fixture.surface.start_answer.lock().expect("start") = Ok(PromptStart::Started {
        delivery: ThreadDelivery::Started {
            turn_id: "4".to_owned(),
        },
    });
    let started = run(&fixture, json!({ "name": "wait", "prompt": "go" }));
    assert_eq!(
        started["prompt"]["delivery"],
        json!({ "kind": "started", "turn_id": "4" })
    );
    assert_eq!(
        *fixture.surface.waited.lock().expect("waited"),
        vec!["d-1".to_owned()]
    );

    *fixture.surface.start_answer.lock().expect("start") = Ok(PromptStart::Queued {
        waiting_on: Some(DeliveryWait {
            kind: "approval".to_owned(),
            device: None,
        }),
    });
    fixture.surface.operation_id = "call:second".to_owned();
    let queued = run(&fixture, json!({ "name": "wait-2", "prompt": "go" }));
    assert_eq!(
        queued["prompt"]["delivery"],
        json!({ "kind": "queued", "queue_position": 2, "waiting_on": { "kind": "approval" } }),
        "{queued}"
    );
}

#[test]
fn a_first_prompt_that_never_starts_fails_prompt_not_started_naming_the_thread_the_delivery_and_its_state()
 {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") = Ok(sent_answer(
        json!({ "kind": "queued", "queue_position": 1 }),
    ));
    let failed = run(&fixture, json!({ "name": "never", "prompt": "build it" }));
    assert_eq!(failure_code(&failed), "prompt_not_started");
    assert_eq!(
        failed["error"]["details"],
        json!({ "thread": derived_id(), "delivery_id": "d-1", "state": "queued" })
    );
    let message = failed["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.ends_with(
            "was created, but its first prompt did not start a turn (its delivery is queued)."
        ),
        "{message}"
    );
    let next = failed["error"]["next_action"].as_str().unwrap_or_default();
    assert!(next.contains("Do not create the thread again."), "{next}");
    assert_eq!(opens(&fixture).len(), 1);
}

#[test]
fn a_prompt_the_send_refused_is_the_prompts_own_error_on_the_ok_create() {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") = Ok(json!({
        "kind": "error",
        "error": { "code": "queue_full", "message": "full", "next_action": "wait" },
    }));
    let created = run(&fixture, json!({ "name": "refused", "prompt": "go" }));
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["prompt"]["error"]["code"], "queue_full");
    assert_eq!(created["prompt"]["error"]["message"], "full");
}

#[test]
fn a_delivery_that_throws_is_the_prompts_outcome_and_the_created_thread_is_still_answered() {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") =
        Err(PromptDeliveryError::Failed("connection lost".to_owned()));
    let created = run(&fixture, json!({ "name": "threw", "prompt": "go" }));
    let id = derived_id();
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["thread"], id);
    let error = &created["prompt"]["error"];
    assert_eq!(error["code"], "internal_error");
    assert_eq!(
        error["message"],
        "The thread was created, but its first prompt was not delivered: connection lost."
    );
    assert_eq!(error["details"], json!({ "thread": id }));
}

#[test]
fn a_store_schema_this_build_does_not_read_names_the_binarys_schema_first_and_the_stores_second() {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") =
        Err(PromptDeliveryError::SchemaTooNew {
            found: 17,
            supported: 16,
        });
    let created = run(&fixture, json!({ "name": "newer", "prompt": "go" }));
    let id = derived_id();
    assert_eq!(created["kind"], "ok", "{created}");
    let error = &created["prompt"]["error"];
    assert_eq!(error["code"], "gateway_schema_too_new");
    assert_eq!(
        error["message"],
        "The thread was created, but its first prompt was not delivered: this omo build reads gateway schema 16, while this machine's gateway store is at schema 17: a newer omo build already upgraded it."
    );
    assert_eq!(
        error["next_action"],
        format!(
            "Update omo with `omo update`, then send the prompt with thread_send to {id} (tool.thread_send({{ thread, message }}) in an eval cell)."
        )
    );
    assert_eq!(
        error["details"],
        json!({ "gateway_schema": 17, "binary_schema": 16, "update_command": "omo update", "thread": id })
    );
}

#[test]
fn a_store_failure_while_waiting_for_the_start_is_the_calls_failure() {
    let fixture = fixture();
    *fixture.surface.delivery_answer.lock().expect("answer") = Ok(sent_answer(
        json!({ "kind": "queued", "queue_position": 1 }),
    ));
    *fixture.surface.start_answer.lock().expect("start") = Err("store is gone".to_owned());
    let input =
        ThreadCreateInput::parse(&json!({ "name": "gone", "prompt": "go" })).expect("parses");
    let call = block_on(fixture.surface.view()).expect("view");
    assert_eq!(
        block_on(create_thread(&call, &input, &fixture.surface)),
        Err(ControlError::Transport("store is gone".to_owned()))
    );
}

#[test]
fn the_schema_refusal_names_both_versions_and_the_name_check_names_the_thread_it_hit() {
    let refusal = gateway_schema_refusal(17, 16);
    assert_eq!(refusal.code.as_str(), "gateway_schema_too_new");
    assert!(refusal.message.contains("schema 16"), "{}", refusal.message);
    assert!(refusal.message.contains("schema 17"), "{}", refusal.message);
    assert!(refusal.next_action.contains("`omo update`"));
    assert!(THREAD_ERROR_CODES.contains(&"prompt_not_started"));
    assert_eq!(
        thread_tool_failure("prompt_not_started", "m", "n", None)
            .code
            .as_str(),
        "prompt_not_started"
    );

    let entry = ThreadAddressEntry {
        thread_id: "dur-a".to_owned(),
        name: "Fix  The Build".to_owned(),
        cwd: "/work".to_owned(),
        status: ThreadStatus::Live,
        updated_at: None,
        session_ids: Vec::new(),
        device: None,
        aliases: Vec::new(),
        row: None,
    };
    assert!(check_name_conflict(std::slice::from_ref(&entry), "another").is_ok());
    let conflict = check_name_conflict(&[entry], "fix the build").expect_err("conflict");
    assert_eq!(conflict.code.as_str(), "name_conflict");
    assert!(conflict.message.contains("(dur-a)"), "{}", conflict.message);
    assert_eq!(
        check_name_conflict(&[], "  ")
            .expect_err("empty")
            .code
            .as_str(),
        "invalid_arguments"
    );
}

#[test]
fn the_opened_result_answers_ok_flat_with_the_thread_and_deduplicated() {
    let fixture = fixture();
    let created = run(&fixture, json!({ "name": "flat" }));
    let view = block_on(fixture.host.list_view()).expect("view");
    let session = view
        .target_session(&derived_id())
        .expect("the session the create made")
        .clone();
    let row = maho_omo_gateway::thread::control::session_host::session_summary(&session, NOW);
    let answer = opened_result(&row, true);
    assert_eq!(answer["kind"], "ok");
    assert_eq!(answer["thread"], created["thread"]);
    assert_eq!(answer["deduplicated"], true);
}

struct FakeRpc {
    cwd: String,
    sessions: Mutex<Vec<Value>>,
    records: Mutex<Vec<Value>>,
}

impl RpcTransport for FakeRpc {
    fn request(
        &self,
        _socket: &str,
        command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        let kind = command["type"].as_str().unwrap_or_default().to_owned();
        let ok = |data: Value| json!({ "type": "response", "command": kind, "success": true, "data": data });
        let frame = match kind.as_str() {
            "list_sessions" => {
                ok(json!({ "sessions": self.sessions.lock().expect("sessions").clone() }))
            }
            "open_session" => {
                self.sessions.lock().expect("sessions").push(json!({
                    "sessionId": "rpc-9",
                    "durableSessionId": command["durableSessionId"],
                    "cwd": command["cwd"],
                    "status": "open",
                }));
                ok(json!({ "sessionId": "rpc-9" }))
            }
            "set_session_name" => {
                for row in self.sessions.lock().expect("sessions").iter_mut() {
                    if row["sessionId"] == command["sessionId"] {
                        row["name"] = command["name"].clone();
                    }
                }
                json!({ "type": "response", "command": kind, "success": true })
            }
            _ => {
                json!({ "type": "response", "command": kind, "success": false, "error": "unsupported" })
            }
        };
        self.records.lock().expect("records").push(command);
        Box::pin(async move { Ok(frame) })
    }
}

fn component_over(
    dir: &tempfile::TempDir,
    rpc: Arc<FakeRpc>,
    cwd: String,
) -> GatewayThreadComponent {
    let surface: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: SOCKET.into(),
        transport: rpc,
        status_all: None,
        now: None,
    }));
    create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
        store: create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store"),
        runtime: SessionRuntimePort {
            phase: Arc::new(|| RuntimePhase::Idle),
            admission_gate: Arc::new(|| SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(|_| {
                Ok(AdmitResult {
                    kind: ExternalAdmissionKind::Steered,
                    turn_epoch: 1,
                })
            }),
            list_admitted_deliveries: Arc::new(AdmissionLedger::default),
        },
        durable_id: CALLER.into(),
        session_path: Arc::new(|| None),
        caller_session_id: Arc::new(|| CALLER.to_owned()),
        caller_name: None,
        caller_provenance: Arc::new(|| Provenance {
            workspace: "unclassified".into(),
            project: "test".into(),
            lane: None,
            session: None,
        }),
        resolve: Arc::new(|_, _| {
            Box::pin(async {
                GatewayResolution::Error(thread_tool_failure("not_found", "unused", "none", None))
            })
        }),
        endpoints: GatewayEndpointPort::inert(),
        notify: None,
        log: None,
        host: Some(surface),
        caller_workspace_root: Arc::new(move || cwd.clone()),
        sessions_directory: None,
        remote: None,
        caller_reply_targets: Arc::new(Vec::new),
        caller_reply_senders: Arc::new(Vec::new),
    }))
}

#[test]
fn the_registered_tool_creates_a_thread_through_the_live_surface_and_records_its_creator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let rpc = Arc::new(FakeRpc {
        cwd: cwd.clone(),
        sessions: Mutex::new(vec![json!({
            "sessionId": "rpc-1",
            "durableSessionId": CALLER,
            "cwd": cwd,
            "name": "caller",
            "status": "open",
        })]),
        records: Mutex::new(Vec::new()),
    });
    let component = component_over(&dir, rpc.clone(), rpc.cwd.clone());
    let created = block_on(
        component
            .execute("thread_create", "call-1", json!({ "name": "child" }))
            .expect("thread_create is registered"),
    );
    let id = created_thread_id(CALLER, "call:call-1");
    assert_eq!(created["kind"], "ok", "{created}");
    assert_eq!(created["thread"], id);
    assert_eq!(created["name"], "child");
    assert_eq!(created["prompt"], Value::Null);

    let records = rpc.records.lock().expect("records").clone();
    let opened: Vec<&Value> = records
        .iter()
        .filter(|record| record["type"] == "open_session")
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["durableSessionId"], id);
    assert_eq!(opened[0]["cwd"], rpc.cwd);
    assert_eq!(opened[0]["retain_on_disconnect"], true);
    assert!(
        records
            .iter()
            .any(|record| record["type"] == "set_session_name" && record["name"] == "child")
    );

    let replay = block_on(
        component
            .execute("thread_create", "call-1", json!({ "name": "child" }))
            .expect("thread_create is registered"),
    );
    assert_eq!(replay["thread"], id);
    assert_eq!(replay["deduplicated"], true, "{replay}");

    let connection =
        rusqlite::Connection::open(gateway_database_path(dir.path())).expect("store file");
    let creator: String = connection
        .query_row(
            "SELECT creator_durable_id FROM thread_creations WHERE created_durable_id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .expect("the creation is recorded");
    assert_eq!(creator, CALLER);
}

#[test]
fn the_registered_tool_refuses_a_field_it_does_not_take_before_it_reads_the_device() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rpc = Arc::new(FakeRpc {
        cwd: String::new(),
        sessions: Mutex::new(Vec::new()),
        records: Mutex::new(Vec::new()),
    });
    let component = component_over(&dir, rpc.clone(), String::new());
    let misnamed = block_on(
        component
            .execute(
                "thread_create",
                "call-1",
                json!({ "name": "x", "message": "build it" }),
            )
            .expect("thread_create is registered"),
    );
    assert_eq!(misnamed["error"]["code"], "invalid_arguments", "{misnamed}");
    assert_eq!(misnamed["error"]["details"]["field"], "message");
    assert_eq!(misnamed["error"]["details"]["suggestion"], "prompt");
    assert!(rpc.records.lock().expect("records").is_empty());
}
