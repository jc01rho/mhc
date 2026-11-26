use maho_ai::providers::faux::FauxModelDefinition;
use maho_rpc::connection_handler::handle_input_line;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
use serde_json::Value;

#[tokio::test]
async fn rpc_set_model_switches_the_session_without_rewriting_the_global_default() {
    let native = FauxSession::new(FauxScript {
        name: "rpc-set-model-scope".to_owned(),
        prompt: "hi".to_owned(),
        responses: Vec::new(),
    })
    .with_faux_models(vec![
        FauxModelDefinition {
            id: "faux-1".to_owned(),
            name: Some("Faux 1".to_owned()),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "faux-2".to_owned(),
            name: Some("Faux 2".to_owned()),
            ..Default::default()
        },
    ])
    .run_native_handle()
    .await
    .expect("native session");
    let session = native.agent_session();
    let line = r#"{"id":"m","type":"set_model","provider":"faux","modelId":"faux-2"}"#;
    let response = handle_input_line(session, line)
        .await
        .expect("dispatch")
        .expect("response");
    let response: Value = serde_json::from_str(&response).expect("wire");
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(session.model().id, "faux-2");
    assert!(
        session
            .with_settings_manager(|manager| manager.get_string("defaultModel"))
            .is_none()
    );
}
