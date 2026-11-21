use maho_ai::auth::pool::slots::PooledCredential;
use maho_ai::providers::faux::{
    FauxModelDefinition, RegisterFauxProviderOptions, faux_provider, faux_streams,
};
use maho_core::agent_session::{AgentSession, AgentSessionConfig};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use maho_core::settings_manager::{InMemorySettingsStorage, SettingsManager};
use maho_interactive::components::footer::{
    AccountFooterOptions, FooterComponent, account_footer_suffix, account_footer_suffix_with,
};
use maho_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use maho_interactive::theme::{ColorMode, Theme};
use maho_tui::utils::strip_terminal_sequences;
use serde_json::json;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

struct TestEditorHost;
impl maho_tui::components::editor::EditorTuiHost for TestEditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize {
        36
    }
}

fn pooled(pinned: Option<&str>, blocked_until_by_name: &BTreeMap<&str, f64>) -> PooledCredential {
    let account = |name: &str, key: &str| {
        let mut obj = serde_json::Map::from_iter([
            ("name".to_owned(), json!(name)),
            ("key".to_owned(), json!(key)),
            ("source".to_owned(), json!("login")),
        ]);
        if let Some(until) = blocked_until_by_name.get(name) {
            obj.insert("blockedUntil".to_owned(), json!(until));
        }
        serde_json::Value::Object(obj)
    };
    let mut root = serde_json::Map::from_iter([
        ("type".to_owned(), json!("api_key")),
        ("key".to_owned(), json!("key-default")),
        (
            "accounts".to_owned(),
            json!([
                account("default", "key-default"),
                account("work", "key-work"),
            ]),
        ),
    ]);
    if let Some(pinned) = pinned {
        root.insert("pinned".to_owned(), json!(pinned));
    }
    serde_json::from_value(serde_json::Value::Object(root)).expect("pooled credential")
}

fn hhmm(until_ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(until_ms.round() as i64)
        .expect("valid timestamp")
        .with_timezone(&chrono::Local)
        .format("%H:%M")
        .to_string()
}

#[test]
fn names_serving_sibling_and_marks_blocked_pin() {
    let now = 1_793_858_400_000.0;
    let blocked_until = now + 3_600_000.0;
    let cred = pooled(
        Some("default"),
        &BTreeMap::from([("default", blocked_until)]),
    );
    let suffix = account_footer_suffix_with(
        Some(&cred),
        "session-5983",
        AccountFooterOptions {
            now_ms: Some(now),
            ..Default::default()
        },
    );
    assert_eq!(
        suffix,
        format!("@work (default blocked until {})", hhmm(blocked_until))
    );
}

#[test]
fn reflects_settings_pin_over_stored_pin() {
    let cred = pooled(Some("work"), &BTreeMap::new());
    let suffix = account_footer_suffix_with(
        Some(&cred),
        "session-5983",
        AccountFooterOptions {
            pinned_account: Some("default"),
            ..Default::default()
        },
    );
    assert_eq!(suffix, "@default");
}

#[test]
fn reflects_settings_pin_that_itself_is_blocked() {
    let now = 1_793_858_400_000.0;
    let blocked_until = now + 3_600_000.0;
    let cred = pooled(Some("work"), &BTreeMap::from([("default", blocked_until)]));
    let suffix = account_footer_suffix_with(
        Some(&cred),
        "session-5983",
        AccountFooterOptions {
            pinned_account: Some("default"),
            now_ms: Some(now),
        },
    );
    assert_eq!(
        suffix,
        format!("@work (default blocked until {})", hhmm(blocked_until))
    );
}

#[test]
fn skips_blocked_hrw_winner_for_next_usable_slot() {
    let now = 1_793_858_400_000.0;
    let blocked_until = now + 3_600_000.0;
    let unblocked = pooled(None, &BTreeMap::new());
    let winner = account_footer_suffix(Some(&unblocked), "session-5983-winner");
    let winner_name = winner.strip_prefix('@').expect("leading @");
    let other_name = if winner_name == "default" {
        "work"
    } else {
        "default"
    };
    let with_blocked = pooled(None, &BTreeMap::from([(winner_name, blocked_until)]));
    let suffix = account_footer_suffix_with(
        Some(&with_blocked),
        "session-5983-winner",
        AccountFooterOptions {
            now_ms: Some(now),
            ..Default::default()
        },
    );
    assert_eq!(suffix, format!("@{other_name}"));
}

#[test]
fn names_pin_with_blocked_marker_when_every_slot_is_blocked() {
    let now = 1_793_858_400_000.0;
    let blocked_until = now + 3_600_000.0;
    let cred = pooled(
        Some("default"),
        &BTreeMap::from([("default", blocked_until), ("work", blocked_until)]),
    );
    let suffix = account_footer_suffix_with(
        Some(&cred),
        "session-5983",
        AccountFooterOptions {
            now_ms: Some(now),
            ..Default::default()
        },
    );
    assert_eq!(
        suffix,
        format!("@default (blocked until {})", hhmm(blocked_until))
    );
}

#[test]
fn treats_expired_block_as_usable() {
    let now = 1_793_858_400_000.0;
    let cred = pooled(
        Some("default"),
        &BTreeMap::from([("default", now - 1_000.0)]),
    );
    let suffix = account_footer_suffix_with(
        Some(&cred),
        "session-5983",
        AccountFooterOptions {
            now_ms: Some(now),
            ..Default::default()
        },
    );
    assert_eq!(suffix, "@default");
}

#[test]
fn resolve_account_pin_applies_only_to_anthropic_subscription_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let far_future = 4_102_444_800_000.0;
    let codex_cred = json!({
        "type": "oauth",
        "access": "fake-a",
        "refresh": "fake-r",
        "expires": far_future,
        "pinned": "work",
        "accounts": [
            {"name": "default", "access": "fake-a", "refresh": "fake-r", "expires": far_future, "source": "login"},
            {"name": "work", "access": "fake-a", "refresh": "fake-r", "expires": far_future, "source": "login"}
        ]
    });
    let anthropic_cred = json!({
        "type": "oauth",
        "access": "fake-a",
        "refresh": "fake-r",
        "expires": far_future,
        "pinned": "work",
        "accounts": [
            {"name": "default", "access": "fake-a", "refresh": "fake-r", "expires": far_future, "source": "login", "blockedUntil": far_future},
            {"name": "work", "access": "fake-a", "refresh": "fake-r", "expires": far_future, "source": "login"}
        ]
    });
    std::fs::write(
        dir.path().join("auth.json"),
        json!({
            "openai-codex": codex_cred,
            "anthropic-subscription": anthropic_cred,
        })
        .to_string(),
    )
    .expect("auth.json");

    let codex_provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some("openai-codex".into()),
        models: Some(vec![FauxModelDefinition {
            id: "gpt-5-codex".into(),
            ..Default::default()
        }]),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("codex faux");
    let anthropic_provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some("anthropic-subscription".into()),
        models: Some(vec![FauxModelDefinition {
            id: "claude-sonnet-5-5".into(),
            ..Default::default()
        }]),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("anthropic faux");

    let streams = faux_streams(codex_provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let context = context.clone();
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(dir.path().join("models.json")),
        auth_path: Some(dir.path().join("auth.json")),
        providers: Some(vec![
            codex_provider.provider.clone(),
            anthropic_provider.provider.clone(),
        ]),
        ..Default::default()
    })
    .expect("runtime");

    let codex_model = codex_provider
        .get_model(Some("gpt-5-codex"))
        .expect("codex");
    let anthropic_model = anthropic_provider
        .get_model(Some("claude-sonnet-5-5"))
        .expect("anthropic");

    let build_mode = |initial_model: maho_ai::model::Model| {
        let session = Arc::new(
            AgentSession::new(AgentSessionConfig {
                agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                    initial_state: Some(maho_agent::agent::PartialAgentState {
                        model: Some(initial_model),
                        ..Default::default()
                    }),
                    stream_fn: Some(stream_fn.clone()),
                    ..Default::default()
                }),
                session_manager: maho_core::session_manager::SessionManager::in_memory(
                    &cwd, None, None,
                ),
                settings_manager: SettingsManager::from_storage(
                    Box::new(InMemorySettingsStorage::default()),
                    false,
                ),
                cwd: cwd.clone(),
                agent_dir: Some(cwd.clone()),
                fallback_now: Some(Arc::new(|| 0.0)),
                retry_random: Some(Arc::new(|| 0.5)),
                scoped_models: Vec::new(),
                favorite_models: Vec::new(),
                flag_values: Default::default(),
                custom_tools: Vec::new(),
                model_runtime: Some(runtime.clone()),
                model_registry: None,
                uses_default_stream_function: Some(false),
                initial_active_tool_names: None,
                default_tool_names: None,
                eval_only_tool_names: None,
                allowed_tool_names: None,
                excluded_tool_names: None,
                base_tools_override: None,
                session_start_event: None,
                auto_title_sessions: Some(false),
            })
            .expect("session"),
        );
        InteractiveMode::new_with_options(
            session,
            Theme::builtin("dark", ColorMode::Truecolor).expect("dark"),
            Rc::new(TestEditorHost),
            InteractiveModeOptions {
                resolve_account_pin: Some(Arc::new(|_, _| Some("default".to_owned()))),
                ..Default::default()
            },
        )
    };

    let codex_mode = build_mode(codex_model);
    let codex_snapshot = codex_mode.footer_snapshot();
    assert_eq!(codex_snapshot.account_suffix, "@work");
    let codex_line = strip_terminal_sequences(
        &FooterComponent::new(codex_snapshot)
            .render(
                160,
                &Theme::builtin("dark", ColorMode::Truecolor).expect("dark"),
            )
            .expect("render")[0],
    );
    assert!(codex_line.contains("openai-codex@work"), "{codex_line}");
    assert!(!codex_line.contains("@default"), "{codex_line}");

    let anthropic_mode = build_mode(anthropic_model);
    let anthropic_snapshot = anthropic_mode.footer_snapshot();
    assert_eq!(
        anthropic_snapshot.account_suffix,
        format!("@work (default blocked until {})", hhmm(far_future))
    );
    let anthropic_line = strip_terminal_sequences(
        &FooterComponent::new(anthropic_snapshot)
            .render(
                200,
                &Theme::builtin("dark", ColorMode::Truecolor).expect("dark"),
            )
            .expect("render")[0],
    );
    assert!(
        anthropic_line.contains(&format!(
            "anthropic-subscription@work (default blocked until {})",
            hhmm(far_future)
        )),
        "{anthropic_line}"
    );
}
