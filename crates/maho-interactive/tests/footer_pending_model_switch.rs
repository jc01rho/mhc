use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, RegisterFauxProviderOptions,
    faux_assistant_message, faux_provider, faux_streams,
};
use maho_core::agent_session::{AgentSession, AgentSessionConfig};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use maho_core::settings_manager::{InMemorySettingsStorage, SettingsManager};
use maho_interactive::components::footer::{FooterComponent, FooterSnapshot};
use maho_interactive::interactive_mode::InteractiveMode;
use maho_interactive::theme::{ColorMode, Theme, ThemeColor};
use maho_tui::tui::Component;
use maho_tui::utils::{strip_terminal_sequences, visible_width};
use std::rc::Rc;
use std::sync::Arc;

const WIDE: usize = 180;
const NARROW: usize = 90;
const EIGHTY: usize = 80;
const SIXTY: usize = 60;
const LONG_TARGET: &str = "deepseek/deepseek-v4.1-flash-ultrafast";
const ELIDED_TARGET: &str = "…ash-ultrafast";

struct TestEditorHost;
impl maho_tui::components::editor::EditorTuiHost for TestEditorHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize {
        36
    }
}

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("dark theme")
}

fn render_footer_top(width: usize, snapshot: FooterSnapshot) -> String {
    let lines = FooterComponent::new(snapshot)
        .render(width, &theme())
        .expect("render footer");
    for line in &lines {
        assert!(visible_width(line) <= width, "width={width}: {line:?}");
    }
    lines.into_iter().next().unwrap_or_default()
}

fn base_snapshot(pending_model_id: Option<&str>) -> FooterSnapshot {
    FooterSnapshot {
        cwd: "/tmp/project".into(),
        branch: Some("main".into()),
        context_window: 200_000.0,
        context_percent: Some(12.3),
        model_id: Some("claude-opus-5-5".into()),
        provider: Some("anthropic".into()),
        reasoning: true,
        thinking_level: Some("medium".into()),
        provider_count: 1,
        pending_model_id: pending_model_id.map(str::to_owned),
        ..FooterSnapshot::default()
    }
}

#[test]
fn shows_running_model_held_target_and_marker_on_wide_terminal() {
    let plain =
        strip_terminal_sequences(&render_footer_top(WIDE, base_snapshot(Some(LONG_TARGET))));
    assert!(
        plain.contains(&format!(
            "claude-opus-5-5 medium → {LONG_TARGET} (after compact)"
        )),
        "{plain}"
    );
}

#[test]
fn keeps_full_held_target_when_width_allows_with_session_name() {
    let mut snapshot = base_snapshot(Some(LONG_TARGET));
    snapshot.session_name = Some("a-session-name-that-costs-width".into());
    let plain = strip_terminal_sequences(&render_footer_top(140, snapshot));
    assert!(
        plain.contains(&format!(
            "claude-opus-5-5 medium → {LONG_TARGET} (after compact)"
        )),
        "{plain}"
    );
    assert!(plain.contains("ctx 12%"), "{plain}");
}

#[test]
fn elides_held_target_and_keeps_context_meter_at_narrow_and_eighty_columns() {
    for (width, expected_model) in [(NARROW, "claude-opus-5-5"), (EIGHTY, "opus-5-5")] {
        let mut snapshot = base_snapshot(Some(LONG_TARGET));
        snapshot.session_name = Some("a-session-name-that-costs-width".into());
        let plain = strip_terminal_sequences(&render_footer_top(width, snapshot));
        assert!(
            plain.contains(&format!(
                "{expected_model} medium → {ELIDED_TARGET} (after compact)"
            )),
            "width={width}: {plain}"
        );
        assert!(plain.contains("ctx 12%"), "width={width}: {plain}");
        assert!(!plain.contains("deepseek/"), "width={width}: {plain}");
    }
}

#[test]
fn never_shows_mid_id_fragment_of_held_target_at_sixty_columns() {
    let plain =
        strip_terminal_sequences(&render_footer_top(SIXTY, base_snapshot(Some(LONG_TARGET))));
    assert!(
        plain.ends_with(&format!("{ELIDED_TARGET} (after compact)")),
        "{plain}"
    );
    assert!(!plain.contains("deepseek/"), "{plain}");
}

#[test]
fn keeps_short_held_target_whole_without_elision() {
    let plain = strip_terminal_sequences(&render_footer_top(
        EIGHTY,
        base_snapshot(Some("mock-small")),
    ));
    assert!(
        plain.contains("opus-5-5 medium → mock-small (after compact)"),
        "{plain}"
    );
    assert!(plain.contains("ctx 12%"), "{plain}");
    assert!(!plain.contains("…ock-small"), "{plain}");
}

#[test]
fn colours_held_target_warning_and_marker_muted_apart_from_accent_running_model() {
    let mut snapshot = base_snapshot(Some(LONG_TARGET));
    snapshot.reasoning = false;
    let line = render_footer_top(WIDE, snapshot);
    let t = theme();
    assert!(line.contains(&t.fg(ThemeColor::Accent, "claude-opus-5-5")));
    assert!(line.contains(&t.fg(ThemeColor::Warning, &format!(" → {LONG_TARGET}"))));
    assert!(line.contains(&t.fg(ThemeColor::Muted, " (after compact)")));

    let mut narrow = base_snapshot(Some(LONG_TARGET));
    narrow.reasoning = false;
    let narrow_line = render_footer_top(EIGHTY, narrow);
    assert!(narrow_line.contains(&t.fg(ThemeColor::Warning, &format!(" → {ELIDED_TARGET}"))));
    assert!(narrow_line.contains(&t.fg(ThemeColor::Muted, " (after compact)")));
}

#[test]
fn renders_normal_label_when_no_switch_is_held() {
    let plain = strip_terminal_sequences(&render_footer_top(WIDE, base_snapshot(None)));
    assert!(plain.ends_with("claude-opus-5-5 medium"), "{plain}");
    assert!(!plain.contains('→'));
    assert!(!plain.contains("after compact"));
}

fn create_switch_mode() -> (InteractiveMode, tempfile::TempDir, Arc<AgentSession>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().into_owned();
    std::fs::write(
        dir.path().join("auth.json"),
        serde_json::json!({ "faux": { "type": "api_key", "key": "faux-key" } }).to_string(),
    )
    .expect("write auth");

    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some("faux".into()),
        models: Some(vec![
            FauxModelDefinition {
                id: "million".into(),
                context_window: Some(1_000_000),
                max_tokens: Some(32_000),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "372k".into(),
                context_window: Some(372_000),
                max_tokens: Some(32_000),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "900k".into(),
                context_window: Some(900_000),
                max_tokens: Some(32_000),
                ..Default::default()
            },
        ]),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("faux provider");

    let initial_model = provider.get_model(Some("million")).expect("million");
    let mut seed2 = faux_assistant_message(
        "still working",
        FauxAssistantMessageOptions {
            timestamp: Some(1),
            ..Default::default()
        },
    );
    seed2.usage.input = 350_000;
    seed2.usage.total_tokens = 351_000;
    provider.set_responses(vec![
        faux_assistant_message(
            "seeded",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
        seed2.into(),
        faux_assistant_message(
            "compact summary",
            FauxAssistantMessageOptions {
                timestamp: Some(2),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "answered on target model",
            FauxAssistantMessageOptions {
                timestamp: Some(3),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(dir.path().join("models.json")),
        auth_path: Some(dir.path().join("auth.json")),
        providers: Some(vec![provider.provider.clone()]),
        ..Default::default()
    })
    .expect("runtime");

    let session = Arc::new(
        AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                initial_state: Some(maho_agent::agent::PartialAgentState {
                    model: Some(initial_model),
                    ..Default::default()
                }),
                stream_fn: Some(stream_fn),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            ),
            settings_manager: SettingsManager::from_storage(
                Box::new(InMemorySettingsStorage::with_global_content(
                    r#"{"compaction":{"keepRecentTokens":1}}"#,
                )),
                false,
            ),
            cwd: cwd.clone(),
            agent_dir: Some(cwd),
            fallback_now: Some(Arc::new(|| 0.0)),
            retry_random: Some(Arc::new(|| 0.5)),
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: Default::default(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
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

    let mode = InteractiveMode::new(session.clone(), theme(), Rc::new(TestEditorHost));
    (mode, dir, session)
}

async fn seed_live_context(session: &AgentSession) {
    session
        .prompt(&"large live context ".repeat(75_000), Default::default())
        .await
        .expect("seed large turn");
    session
        .prompt("second message", Default::default())
        .await
        .expect("seed second turn");
}

#[tokio::test]
async fn interactive_mode_footer_shows_held_switch_and_clears_after_compaction_or_supersede() {
    let (mut mode, _dir, session) = create_switch_mode();
    seed_live_context(&session).await;
    let target = session.model_registry().find("faux", "372k").expect("372k");
    let roomy = session.model_registry().find("faux", "900k").expect("900k");

    session.set_model(target.clone()).await.expect("hold");
    mode.drain_events();
    assert_eq!(
        mode.footer_snapshot().pending_model_id.as_deref(),
        Some("372k")
    );
    let pending_render = strip_terminal_sequences(&mode.render(200).join("\n"));
    assert!(
        pending_render.contains("million → 372k (after compact)"),
        "{pending_render}"
    );

    session.set_model(roomy).await.expect("supersede");
    mode.drain_events();
    assert_eq!(mode.footer_snapshot().pending_model_id, None);
    let superseded_render = strip_terminal_sequences(&mode.render(200).join("\n"));
    assert!(superseded_render.contains("900k"), "{superseded_render}");
    assert!(!superseded_render.contains("after compact"));

    session.set_model(target).await.expect("hold again");
    mode.drain_events();
    assert_eq!(
        mode.footer_snapshot().pending_model_id.as_deref(),
        Some("372k")
    );
    session
        .prompt("continue", Default::default())
        .await
        .expect("prompt compacts and applies");
    mode.drain_events();
    assert_eq!(mode.footer_snapshot().pending_model_id, None);
    let applied_render = strip_terminal_sequences(&mode.render(200).join("\n"));
    assert!(applied_render.contains("372k"), "{applied_render}");
    assert!(!applied_render.contains("after compact"));
}
