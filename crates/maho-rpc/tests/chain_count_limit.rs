//! senpi#5899 / #5923 (decision K8): a thread call from another session arrives as RPC input carrying its chain's hop
//! count. It is admitted against the user's `threads.chainCountLimit`, read from the settings at each admission: past
//! the limit the input is refused with the typed reason `chain_count_limit`, and a malformed count with
//! `invalid_chain_count`, before anything of it runs or is held.
use maho_core::{
    agent_session::AgentSession,
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_rpc::connection_handler::handle_input_line;
use serde_json::{Value, json};

struct Fixture {
    _temp: tempfile::TempDir,
    _provider: Box<dyn std::any::Any>,
    session: AgentSession,
}

async fn fixture_with(settings: &str) -> Fixture {
    let temp = tempfile::tempdir().expect("root");
    let cwd = temp.path().to_string_lossy().into_owned();
    let directory = temp.path().join("sessions");
    let manager = SessionManager::create(&cwd, Some(directory.to_str().expect("directory")), None);
    let provider =
        maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("model");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        providers: Some(vec![provider.provider.clone()]),
        ..Default::default()
    })
    .expect("valid model runtime");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(cwd),
        model_runtime: Some(runtime),
        model: Some(model),
        session_manager: Some(manager),
        settings_manager: Some(SettingsManager::from_storage(
            Box::new(InMemorySettingsStorage::with_global_content(settings)),
            false,
        )),
        no_tools: Some(NoToolsMode::All),
        auto_title_sessions: Some(false),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;
    Fixture {
        _temp: temp,
        _provider: Box::new(provider),
        session,
    }
}

async fn command(session: &AgentSession, value: Value) -> Value {
    serde_json::from_str(
        &Box::pin(handle_input_line(session, &value.to_string()))
            .await
            .expect("dispatch")
            .expect("response"),
    )
    .expect("JSON response")
}

// Guards the limit comparison on every input entry: call N + 1 under a user-set limit N is refused with the typed
// reason, the count and the limit, and nothing of it is queued or recorded.
#[tokio::test]
async fn a_thread_call_past_the_users_limit_is_refused_before_anything_of_it_runs_or_is_held() {
    let fixture = fixture_with(r#"{"threads":{"chainCountLimit":2}}"#).await;
    for kind in ["prompt", "steer", "follow_up"] {
        let refused = command(
            &fixture.session,
            json!({"type": kind, "message": "from B", "chainCount": 3}),
        )
        .await;
        assert_eq!(refused["success"], false, "{kind}");
        assert_eq!(refused["errorCode"], "chain_count_limit", "{kind}");
        assert_eq!(
            refused["errorData"],
            json!({"count": 3, "limit": 2}),
            "{kind}"
        );
    }
    assert_eq!(fixture.session.pending_message_count(), 0);
    let messages = command(&fixture.session, json!({"type": "get_messages"})).await;
    assert!(
        messages["data"]["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .all(|message| message["role"] != "user")
    );
}

// Guards the shape check: a malformed count would never compare above the limit and would be admitted without it.
#[tokio::test]
async fn a_malformed_chain_count_is_refused_with_its_typed_reason() {
    let fixture = fixture_with("{}").await;
    for malformed in [
        json!(null),
        json!(0),
        json!(-3),
        json!(1.5),
        json!("abc"),
        json!(9_007_199_254_740_992u64),
    ] {
        let refused = command(
            &fixture.session,
            json!({"type": "steer", "message": "from B", "chainCount": malformed}),
        )
        .await;
        assert_eq!(refused["success"], false, "{malformed}");
        assert_eq!(refused["errorCode"], "invalid_chain_count", "{malformed}");
        assert_eq!(
            refused["errorData"],
            json!({"chainCount": malformed}),
            "{malformed}"
        );
    }
    assert_eq!(fixture.session.pending_message_count(), 0);
}

// The refusal names the limit the user set in the settings, in its typed data and in its message.
#[tokio::test]
async fn the_limit_the_answer_names_is_the_one_the_user_set() {
    let fixture = fixture_with(r#"{"threads":{"chainCountLimit":1}}"#).await;
    let refused = command(
        &fixture.session,
        json!({"type": "follow_up", "message": "from B", "chainCount": 2}),
    )
    .await;
    assert_eq!(refused["errorCode"], "chain_count_limit");
    assert_eq!(refused["errorData"], json!({"count": 2, "limit": 1}));
    assert_eq!(
        refused["error"],
        "This thread call would be hop 2 of its chain; your thread chain count limit is 1."
    );
}
