//! `/remote` (omo#13741, #14234): the device, session and action picker, and "Close anyway" after a
//! close refused because the session is running a turn.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::*;
use maho_omo_remote::RemoteCommandComponent;
use maho_omo_remote::verbs::NOT_RUNNING;

use crate::verbs_table::Pair;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers each select with the first option that starts or ends with the next scripted text, each
/// confirm with the next scripted answer, and records what was asked and notified.
#[derive(Default)]
struct ScriptedUi {
    picks: Mutex<VecDeque<String>>,
    confirms: Mutex<VecDeque<bool>>,
    asked: Mutex<Vec<String>>,
    notices: Mutex<Vec<(String, NotificationType)>>,
}

impl ScriptedUi {
    fn new(picks: &[&str], confirms: &[bool]) -> Arc<Self> {
        Arc::new(Self {
            picks: Mutex::new(picks.iter().map(|pick| (*pick).to_owned()).collect()),
            confirms: Mutex::new(confirms.iter().copied().collect()),
            ..Self::default()
        })
    }
}

impl ExtensionUi for ScriptedUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        lock(&self.asked).push(title.to_owned());
        let pick = lock(&self.picks).pop_front();
        let chosen = pick.and_then(|pick| {
            options
                .iter()
                .find(|option| option.starts_with(&pick) || option.ends_with(&pick))
                .cloned()
        });
        Box::pin(async move { chosen })
    }
    fn confirm<'a>(
        &'a self,
        title: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        lock(&self.asked).push(title.to_owned());
        let answer = lock(&self.confirms).pop_front().unwrap_or(false);
        Box::pin(async move { answer })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        lock(&self.notices).push((message.to_owned(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

struct TestSession;

impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "local"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestRegistry;

impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

fn context(ui: Arc<ScriptedUi>, agent_dir: PathBuf) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: agent_dir.clone(),
        agent_dir,
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

async fn run_command(state_dir: &Path, ui: Arc<ScriptedUi>, args: &str) {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("remote", state_dir.to_path_buf(), SourceInfo::default()),
        DEFAULT_EXTENSION_SESSION_PROFILE,
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    RemoteCommandComponent {
        version: "0.0.0-test".into(),
        state_dir: Some(state_dir.to_path_buf()),
    }
    .register(&mut api);
    let command = api
        .registered
        .commands
        .iter()
        .find(|command| command.name == "remote")
        .expect("/remote is registered");
    let ctx = context(ui, state_dir.join("agent"));
    (command.handler)(args, &ctx).await.expect("/remote runs");
}

#[tokio::test]
async fn a_close_refused_for_a_running_turn_offers_close_anyway() {
    let pair = Pair::start().await;
    *lock(&pair.beta_host.streaming) = true;
    let ui = ScriptedUi::new(&["beta (", "/b1", "Close"], &[true]);
    run_command(&pair.alpha_state, Arc::clone(&ui), "").await;

    let asked = lock(&ui.asked).clone();
    assert_eq!(asked[0], "Remote device");
    assert_eq!(asked[1], "Session");
    assert!(
        asked[2].ends_with("/b1"),
        "the action dialog names the session: {asked:?}"
    );
    assert_eq!(asked[3], "Close anyway?");
    assert_eq!(asked.len(), 4);
    let notices = lock(&ui.notices);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0].1, NotificationType::Error);
    assert!(notices[0].0.contains("is running a turn"), "{notices:?}");
    assert_eq!(
        notices[1],
        ("Closed beta/b1.".to_owned(), NotificationType::Info)
    );
    drop(notices);
    assert_eq!(pair.beta_host.methods(), vec!["get_state", "close_session"]);
    pair.stop().await;
}

#[tokio::test]
async fn an_idle_session_closes_without_asking_and_a_declined_close_sends_nothing() {
    let pair = Pair::start().await;
    let ui = ScriptedUi::new(&["beta (", "/b1", "Close"], &[]);
    run_command(&pair.alpha_state, Arc::clone(&ui), "").await;
    assert!(!lock(&ui.asked).iter().any(|title| title == "Close anyway?"));
    assert_eq!(
        lock(&ui.notices).last().cloned(),
        Some(("Closed beta/b1.".to_owned(), NotificationType::Info))
    );
    assert_eq!(pair.beta_host.methods(), vec!["get_state", "close_session"]);

    *lock(&pair.beta_host.streaming) = true;
    let declined = ScriptedUi::new(&["beta (", "/b1", "Close"], &[false]);
    run_command(&pair.alpha_state, Arc::clone(&declined), "").await;
    assert_eq!(
        lock(&declined.asked).last().map(String::as_str),
        Some("Close anyway?")
    );
    assert_eq!(
        pair.beta_host.methods(),
        vec!["get_state", "close_session", "get_state"],
        "declining sends no close"
    );
    pair.stop().await;
}

#[tokio::test]
async fn without_a_device_service_the_picker_says_so_and_verbs_run_inline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("none");
    let ui = ScriptedUi::new(&[], &[]);
    run_command(&state_dir, Arc::clone(&ui), "").await;
    assert_eq!(
        lock(&ui.notices).as_slice(),
        [(NOT_RUNNING.to_owned(), NotificationType::Error)]
    );
    assert!(lock(&ui.asked).is_empty(), "no picker without a service");

    let inline = ScriptedUi::new(&[], &[]);
    run_command(&state_dir, Arc::clone(&inline), "daemon").await;
    run_command(&state_dir, Arc::clone(&inline), "status").await;
    let notices = lock(&inline.notices);
    assert_eq!(
        notices[0].1,
        NotificationType::Info,
        "daemon belongs in a terminal"
    );
    assert_eq!(
        notices[1],
        (NOT_RUNNING.to_owned(), NotificationType::Error)
    );
}
