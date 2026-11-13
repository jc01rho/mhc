//! Port of omo-cpa `src/extension.ts`. The extension entry: providers, the `/cpa` command surface,
//! event hooks, and the status line.
//!
//! CPA is registered as two in-memory providers: `cliproxyapi` for primary workhorses and
//! `cliproxyapi-last` for explicit last-resort routing. Both refresh from the same three-format
//! catalog; `/login cliproxyapi` remains available through oauth. The extension is fail-open by
//! design: every handler swallows its own errors so a plugin bug can never take down a session.
//!
//! Contract notes verified against senpi's extension type definitions:
//! - `model_select` CANNOT change the model, so it only refreshes the status line.
//! - `before_provider_request` carries the model, which is where CPA attribution is recorded.
//! - `after_provider_response` carries only `{status, headers}` with no provider identity, so CPA
//!   attribution uses the preceding request (see `HealthTracker`).

use crate::chain::{GenerateFallbackChainsInput, TierProviderNames, generate_fallback_chains};
use crate::config::{ConfigEnv, load_config};
use crate::health::HealthTracker;
use crate::management_key;
use crate::provider::{
    LAST_RESORT_PROVIDER_NAME, PROVIDER_NAME, ProviderRegistrationData, ProviderRuntime,
    STABLE_LAST_RESORT_IDS, TieredProviderData, build_provider_registration,
    default_models_store_path, load_provider_data, now_ms, prune_unrestorable_store_entries,
    register_cpa_provider,
};
use crate::redact::redact;
use crate::render::{ReportInput, TierCounts, render_report, render_status_line};
use crate::tier::{
    OverrideStore, TierCommand, clear_override, load_override_store, overrides_file,
    parse_tier_command, save_override_store, set_override, to_override_map,
};
use crate::tier_types::Tier;
use crate::types::{CpaConfig, HealthSnapshot, HealthState};
use crate::usage::fetch_usage;
use maho_ext_api::{
    CustomMessage, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext,
    ExtensionEvent, ExtensionMode, ExtensionWidgetAction, ExtensionWidgetOptions, NotificationType,
    SendMessageOptions, ToolContent, WidgetContent, WidgetPlacement,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const STATUS_KEY: &str = "omo-cpa";
pub const COMMAND_NAME: &str = "cpa";
pub const COMMAND_DESCRIPTION: &str = "CPA 상태·tier·fallback chain·계정 사용량 점검";
pub const COMMAND_ARGUMENT_HINT: &str = "[help|refresh|management [status|set|clear]|tier [promote|demote|reset] <model-id>|chains [apply]]";
pub const USAGE_TIMEOUT_MS: u64 = 10_000;

/// Paths the extension reads and writes. Injected so tests never touch the real home directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmoCpaOptions {
    pub override_path: PathBuf,
    pub management_key_path: PathBuf,
    pub models_store_path: PathBuf,
}

impl Default for OmoCpaOptions {
    fn default() -> Self {
        Self {
            override_path: overrides_file(),
            management_key_path: management_key::default_management_key_path(),
            models_store_path: default_models_store_path(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    const fn notification(self) -> NotificationType {
        match self {
            Self::Info => NotificationType::Info,
            Self::Warning => NotificationType::Warning,
            Self::Error => NotificationType::Error,
        }
    }
}

/// The mutable extension state. One instance per registration, shared by every handler.
#[derive(Default)]
pub struct State {
    pub config: Option<CpaConfig>,
    pub api_key: Option<String>,
    pub management_key: Option<String>,
    pub config_reason: Option<String>,
    pub tiers: Option<TierCounts>,
    pub tier_reason: Option<String>,
    pub health: HealthTracker,
    pub last_request_was_cpa: bool,
}

impl State {
    pub fn new() -> Self {
        Self {
            health: HealthTracker::default(),
            ..Self::default()
        }
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        self.health.snapshot()
    }
}

/// Shared, lockable extension state. Poisoning is never fatal: the fail-open contract wins.
pub type SharedState = Arc<Mutex<State>>;

fn lock(state: &SharedState) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Everything the command handlers close over: the shared state, the shared HTTP cache, the paths,
/// and the API handle a re-registration needs (command handlers only receive a context).
#[derive(Clone)]
pub struct HandlerEnv {
    pub state: SharedState,
    pub runtime: ProviderRuntime,
    pub options: OmoCpaOptions,
    pub api: Arc<ExtensionApi>,
}

/// Register the in-code CPA provider plus oauth. Fail-open: a malformed registration is ignored so
/// the built-in `/cpa` command remains the only surface.
pub fn register_provider_safely(
    api: &ExtensionApi,
    runtime: &ProviderRuntime,
    data: Option<&ProviderRegistrationData>,
) {
    let _ = register_cpa_provider(api, data, Some(runtime));
    crate::provider::migrate_config_background();
}

/// Load the live catalog and recompute tier sizes. Never fails.
pub async fn refresh(env: &HandlerEnv, force: bool) {
    let stored = crate::provider::read_stored_primary_connection_default();
    let loaded = load_config(
        stored
            .as_ref()
            .map(|connection| connection.api_key.as_str()),
        stored
            .as_ref()
            .map(|connection| connection.base_url.as_str()),
        &env.options.management_key_path,
        &ConfigEnv::from_process(),
    );

    let tiers = match load_tiered_provider_data(&env.runtime, &env.options, force).await {
        Ok(tiered) => Ok(TierCounts {
            primary: tiered.report.primary.len(),
            last: tiered.report.last.len(),
            chat_unfit: tiered.report.chat_unfit.len(),
        }),
        Err(error) => Err(format!("점검 실패: {}", redact(&error))),
    };

    let mut state = lock(&env.state);
    state.config = Some(loaded.config);
    state.api_key = loaded.api_key;
    state.management_key = loaded.management_key;
    state.config_reason = loaded.reason;
    match tiers {
        Ok(tiers) => {
            state.tiers = Some(tiers);
            state.tier_reason = None;
        }
        Err(reason) => {
            state.tiers = None;
            state.tier_reason = Some(reason);
        }
    }
}

/// Load the catalog and apply the override store. `Err` carries the reason for the report.
pub async fn load_tiered_provider_data(
    runtime: &ProviderRuntime,
    options: &OmoCpaOptions,
    force: bool,
) -> Result<TieredProviderData, String> {
    let data = load_provider_data(runtime, None, None, force).await?;
    let store = load_override_store(&options.override_path);
    Ok(build_provider_registration(&ProviderRegistrationData {
        catalog: data.catalog,
        costs: data.costs,
        base_url: data.base_url,
        overrides: to_override_map(&store),
    }))
}

/// `setStatus`: the footer line plus, for RPC sessions, the same line as a widget with actions.
pub fn set_status(state: &SharedState, context: &ExtensionContext) {
    let (tiers, health) = {
        let state = lock(state);
        (state.tiers, state.health.snapshot())
    };
    let line = render_status_line(tiers, &health);
    context.ui.set_status(STATUS_KEY, Some(&line));
    // The OmO desktop host ignores setStatus over RPC but renders widgets with action buttons, so
    // RPC sessions get the same line as a widget. Each desktop environment runs its own engine, so
    // the line names its machine.
    if context.mode == ExtensionMode::Rpc {
        context.ui.set_widget(
            STATUS_KEY,
            Some(WidgetContent::Lines(vec![format!(
                "{} · {line}",
                short_hostname()
            )])),
            ExtensionWidgetOptions {
                placement: WidgetPlacement::BelowEditor,
                actions: Some(vec![
                    ExtensionWidgetAction {
                        label: "점검".to_owned(),
                        command: "/cpa".to_owned(),
                    },
                    ExtensionWidgetAction {
                        label: "새로고침".to_owned(),
                        command: "/cpa refresh".to_owned(),
                    },
                ]),
            },
        );
    }
}

fn short_hostname() -> String {
    hostname().split('.').next().unwrap_or_default().to_owned()
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn notify(context: &ExtensionContext, message: &str, level: Level) {
    context.ui.notify(message, level.notification());
}

/// Post a block message through the registration's action handle. Command handlers only receive
/// a context, so the send goes through the shared `ExtensionApi` rather than the context.
fn send_block(env: &HandlerEnv, content: &str, custom_type: &str) {
    let message = CustomMessage {
        custom_type: custom_type.to_owned(),
        content: vec![ToolContent::text(redact(content))],
        display: true,
        details: None,
    };
    let _ = env.api.send_message(message, SendMessageOptions::default());
}

/// One-line-per-pool summary of the live tier split.
pub fn render_tier_summary(tiered: &TieredProviderData) -> String {
    let report = &tiered.report;
    let active_overrides: Vec<String> = report
        .decisions
        .iter()
        .filter(|decision| decision.overridden)
        .map(|decision| format!("{} → {}", decision.id, decision.tier.as_str()))
        .collect();
    let inactive: Vec<String> = report
        .inactive_overrides
        .iter()
        .map(|entry| format!("{} → {}", entry.id, entry.tier.as_str()))
        .collect();
    let row = |label: &str, entries: Vec<String>| -> String {
        if entries.is_empty() {
            format!("{label} (0) · 없음")
        } else {
            format!("{label} ({})\n  {}", entries.len(), entries.join("\n  "))
        }
    };
    [
        "CPA 모델 tier".to_owned(),
        row(
            "primary",
            report.primary.iter().map(|row| row.id.clone()).collect(),
        ),
        row(
            "last-resort",
            report.last.iter().map(|row| row.id.clone()).collect(),
        ),
        row(
            "chat-unfit (명시 선택만 가능, 체인 제외)",
            report.chat_unfit.iter().map(|row| row.id.clone()).collect(),
        ),
        row("활성 override", active_overrides),
        row("비활성 override", inactive),
    ]
    .join("\n\n")
}

pub async fn handle_tier_command(
    command: TierCommand,
    context: &ExtensionContext,
    env: &HandlerEnv,
) {
    match command {
        TierCommand::Error { message } => notify(context, &message, Level::Warning),
        TierCommand::List => {
            match load_tiered_provider_data(&env.runtime, &env.options, false).await {
                Ok(tiered) => send_block(env, &render_tier_summary(&tiered), "omo-cpa-tier"),
                Err(error) => notify(
                    context,
                    &format!("CPA 모델 목록 불러오기 실패 · {}", redact(&error)),
                    Level::Warning,
                ),
            }
        }
        TierCommand::Set { id, tier } => {
            apply_tier_change(context, env, &id, Some(tier)).await;
        }
        TierCommand::Clear { id } => {
            apply_tier_change(context, env, &id, None).await;
        }
    }
}

async fn apply_tier_change(
    context: &ExtensionContext,
    env: &HandlerEnv,
    id: &str,
    tier: Option<Tier>,
) {
    let store = load_override_store(&env.options.override_path);
    let next = match tier {
        Some(tier) => set_override(&store, id, tier, Some("/cpa tier"), now_ms()),
        None => clear_override(&store, id),
    };
    if !save_override_store(&next, &env.options.override_path) {
        notify(
            context,
            &format!(
                "tier 설정을 저장하지 못했습니다: {} (권한과 경로를 확인하세요)",
                env.options.override_path.display()
            ),
            Level::Error,
        );
        return;
    }

    let tiered = match load_provider_data(&env.runtime, None, None, false).await {
        Ok(data) => build_provider_registration(&ProviderRegistrationData {
            catalog: data.catalog,
            costs: data.costs,
            base_url: data.base_url,
            overrides: to_override_map(&next),
        }),
        Err(error) => {
            notify(
                context,
                &format!("CPA 모델 목록 불러오기 실패 · {}", redact(&error)),
                Level::Error,
            );
            return;
        }
    };
    // Re-register immediately so the new split takes effect without a reload.
    if let Err(error) = re_register(env, &next) {
        notify(
            context,
            &format!("CPA provider 재등록 실패: {}", redact(&error.message)),
            Level::Error,
        );
    }
    let action = match tier {
        Some(tier) => format!("{id} → {}", tier.as_str()),
        None => format!("{id} override 해제"),
    };
    notify(
        context,
        &format!(
            "CPA tier 적용 완료: {action} · primary {}, last {}",
            tiered.report.primary.len(),
            tiered.report.last.len()
        ),
        Level::Info,
    );
}

fn re_register(
    env: &HandlerEnv,
    overrides: &OverrideStore,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let migration = crate::provider::read_migration_source();
    let registration = ProviderRegistrationData {
        catalog: Vec::new(),
        costs: BTreeMap::new(),
        base_url: Some(migration.base_url),
        overrides: to_override_map(overrides),
    };
    register_cpa_provider(&env.api, Some(&registration), Some(&env.runtime))
}

pub fn help_text(management_key_path: &Path) -> String {
    [
        "/cpa 명령 도움말".to_owned(),
        "/cpa                         상태·tier·fallback chain·사용량 리포트".to_owned(),
        "/cpa help                    이 도움말".to_owned(),
        "/cpa refresh                 상태와 모델 카탈로그 새로고침".to_owned(),
        "/cpa management [status]     Management Key 설정 상태 확인".to_owned(),
        "/cpa management set          입력창에서 키 저장 (타이핑은 화면에 표시될 수 있음)"
            .to_owned(),
        "/cpa management clear        저장한 키 삭제".to_owned(),
        "/cpa tier                    모델 tier와 override 목록".to_owned(),
        "/cpa tier promote <model-id> 주력 tier로 지정".to_owned(),
        "/cpa tier demote <model-id>  최후 fallback tier로 지정".to_owned(),
        "/cpa tier reset <model-id>   수동 tier 지정 해제".to_owned(),
        "/cpa chains                  fallback chain 미리보기".to_owned(),
        "/cpa chains apply            현재 세션에 fallback chain 적용".to_owned(),
        String::new(),
        format!(
            "키는 {} 에 암호화 없이 사용자 전용 권한(0600)으로 저장됩니다.",
            management_key_path.display()
        ),
        "OMO_CPA_MANAGEMENT_KEY 환경변수가 저장된 키보다 우선합니다.".to_owned(),
    ]
    .join("\n")
}

async fn handle_management_command(
    action: Option<&str>,
    context: &ExtensionContext,
    env: &HandlerEnv,
) {
    match action {
        None | Some("status") => {
            let configured = env_management_key().is_some()
                || management_key::read_management_key(&env.options.management_key_path).is_some();
            notify(
                context,
                &format!(
                    "Management Key {}{}",
                    if configured { "설정됨" } else { "미설정" },
                    if configured {
                        ""
                    } else {
                        " · /cpa management set 으로 등록"
                    }
                ),
                Level::Info,
            );
        }
        Some("set") => {
            if !context.has_ui {
                notify(
                    context,
                    "이 모드에서는 비밀 입력 UI를 사용할 수 없습니다. OMO_CPA_MANAGEMENT_KEY 환경변수를 설정하세요.",
                    Level::Error,
                );
                return;
            }
            let key = context
                .ui
                .input(
                    "CPA Management Key 입력",
                    Some("입력 중 화면에 표시될 수 있습니다"),
                    Default::default(),
                )
                .await;
            let Some(key) = key else {
                notify(context, "Management Key 입력을 취소했습니다", Level::Info);
                return;
            };
            if key.trim().is_empty() {
                notify(
                    context,
                    "Management Key가 비어 있습니다. 기존 키는 유지했습니다.",
                    Level::Warning,
                );
                return;
            }
            match management_key::save_management_key(&key, &env.options.management_key_path) {
                Ok(()) => {
                    reload_keys(env);
                    notify(
                        context,
                        "Management Key를 사용자 전용 파일(0600)에 저장했습니다. 파일은 암호화되지 않습니다.",
                        Level::Info,
                    );
                }
                Err(error) => notify(
                    context,
                    &format!("Management Key 저장 실패: {}", redact(&error.to_string())),
                    Level::Error,
                ),
            }
        }
        Some("clear") => {
            if env_management_key().is_some() {
                notify(
                    context,
                    "환경변수 OMO_CPA_MANAGEMENT_KEY가 우선 적용 중입니다. 환경변수를 제거한 뒤 파일 키를 초기화하세요.",
                    Level::Warning,
                );
                return;
            }
            match management_key::clear_management_key(&env.options.management_key_path) {
                Ok(()) => {
                    reload_keys(env);
                    notify(context, "저장된 Management Key를 삭제했습니다", Level::Info);
                }
                Err(error) => notify(
                    context,
                    &format!("Management Key 삭제 실패: {}", redact(&error.to_string())),
                    Level::Error,
                ),
            }
        }
        Some(other) => notify(
            context,
            &format!(
                "알 수 없는 management 하위 명령 \"{other}\". 사용법: /cpa management [status|set|clear]"
            ),
            Level::Warning,
        ),
    }
}

fn env_management_key() -> Option<String> {
    std::env::var("OMO_CPA_MANAGEMENT_KEY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn reload_keys(env: &HandlerEnv) {
    let (api_key, root) = {
        let state = lock(&env.state);
        (
            state.api_key.clone(),
            state.config.as_ref().map(|config| config.root.clone()),
        )
    };
    let loaded = load_config(
        api_key.as_deref(),
        root.as_deref(),
        &env.options.management_key_path,
        &ConfigEnv::from_process(),
    );
    let mut state = lock(&env.state);
    state.management_key = loaded.management_key;
    state.config = Some(loaded.config);
}

async fn handle_chains_command(words: &[String], context: &ExtensionContext, env: &HandlerEnv) {
    let action = words.get(1).map(String::as_str);
    if action.is_some_and(|action| action != "apply") {
        notify(
            context,
            &format!(
                "알 수 없는 하위 명령 \"{}\". 사용법: /cpa chains [apply]",
                action.unwrap_or_default()
            ),
            Level::Warning,
        );
        return;
    }

    let tiered = match load_tiered_provider_data(&env.runtime, &env.options, false).await {
        Ok(tiered) => tiered,
        Err(error) => {
            notify(
                context,
                &format!("CPA 모델 목록 불러오기 실패 · {}", redact(&error)),
                Level::Error,
            );
            return;
        }
    };
    let stable_last: Vec<_> = tiered
        .report
        .last
        .iter()
        .filter(|decision| STABLE_LAST_RESORT_IDS.contains(&decision.id.as_str()))
        .cloned()
        .collect();
    let mut decisions: Vec<_> = tiered.report.primary.clone();
    decisions.extend(if stable_last.is_empty() {
        tiered.report.last.clone()
    } else {
        stable_last
    });
    let chains = generate_fallback_chains(&GenerateFallbackChainsInput {
        catalog: &tiered.catalog,
        decisions: &decisions,
        providers: TierProviderNames {
            primary: PROVIDER_NAME.to_owned(),
            last: LAST_RESORT_PROVIDER_NAME.to_owned(),
        },
        targets: tiered
            .report
            .primary
            .iter()
            .map(|decision| decision.id.clone())
            .collect(),
    });

    let settings = context.session_settings().ok();
    let current = settings
        .map(|settings| settings.get_retry_fallback_settings().chains)
        .unwrap_or_default();
    let proposed: BTreeMap<String, Vec<String>> = chains
        .iter()
        .map(|chain| (chain.target.clone(), chain.entries.clone()))
        .collect();
    let body = [
        "현재 fallback chains (변경 전)".to_owned(),
        pretty_json(&current),
        String::new(),
        if action == Some("apply") {
            "적용할 CPA chains".to_owned()
        } else {
            "CPA chains 미리보기 (/cpa chains apply 로 명시 적용)".to_owned()
        },
        pretty_json(&proposed),
    ]
    .join("\n");
    send_block(env, &body, "omo-cpa-chains");

    if action != Some("apply") {
        return;
    }
    let Some(settings) = settings else {
        notify(
            context,
            "현재 세션은 fallback 설정 API를 제공하지 않습니다",
            Level::Error,
        );
        return;
    };
    for chain in &chains {
        if let Err(error) = settings
            .set_fallback_chain(&chain.target, &chain.entries)
            .await
        {
            notify(
                context,
                &format!(
                    "CPA fallback chain 적용 실패 ({}): {}",
                    chain.target,
                    redact(&error.message)
                ),
                Level::Error,
            );
            return;
        }
    }
    notify(
        context,
        &format!(
            "CPA fallback chain {}개를 명시적으로 적용했습니다",
            chains.len()
        ),
        Level::Info,
    );
}

fn pretty_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
}

/// The `/cpa` handler body, split out so tests can drive the parsing without a live session.
pub async fn handle_cpa_command(args: &str, context: &ExtensionContext, env: &HandlerEnv) {
    let words: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
    if matches!(
        words.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        send_block(
            env,
            &help_text(&env.options.management_key_path),
            "omo-cpa-help",
        );
        return;
    }
    if words.first().map(String::as_str) == Some("management") {
        handle_management_command(words.get(1).map(String::as_str), context, env).await;
        return;
    }
    if let Some(tier_command) = parse_tier_command(&words) {
        handle_tier_command(tier_command, context, env).await;
        return;
    }
    if words.first().map(String::as_str) == Some("chains") {
        handle_chains_command(&words, context, env).await;
        return;
    }
    if let Some(first) = words.first().filter(|first| *first != "refresh") {
        notify(
            context,
            &format!("알 수 없는 하위 명령 \"{first}\". /cpa help 에서 지원 인자를 확인하세요."),
            Level::Warning,
        );
        return;
    }

    let force = args.trim() == "refresh";
    let needs_refresh = force || lock(&env.state).config.is_none();
    if needs_refresh {
        refresh(env, force).await;
    }

    let (config, management_key, tiers, tier_reason, health) = {
        let state = lock(&env.state);
        (
            state.config.clone(),
            state.management_key.clone(),
            state.tiers,
            state.tier_reason.clone(),
            state.health.snapshot(),
        )
    };
    let Some(config) = config else {
        let reason = lock(&env.state)
            .config_reason
            .clone()
            .unwrap_or_else(|| "이유 불명".to_owned());
        notify(
            context,
            &format!("CPA 설정을 찾을 수 없음 · {reason}"),
            Level::Error,
        );
        return;
    };

    let usage = fetch_usage(
        &env.runtime.client,
        &config.root,
        management_key.as_deref(),
        USAGE_TIMEOUT_MS,
    )
    .await;
    let report = render_report(&ReportInput {
        config: &config,
        tiers,
        tier_reason: tier_reason.as_deref(),
        health: &health,
        usage: &usage,
    });
    send_block(env, &report, "omo-cpa-report");
    set_status(&env.state, context);
}

/// The `OmoCpa` extension: `register` wires providers, the command, and the event hooks.
#[derive(Clone, Default)]
pub struct OmoCpaExtension {
    pub options: OmoCpaOptions,
    pub runtime: ProviderRuntime,
}

impl OmoCpaExtension {
    pub fn new(options: OmoCpaOptions, runtime: ProviderRuntime) -> Self {
        Self { options, runtime }
    }
}

impl Extension for OmoCpaExtension {
    fn register(&self, api: &mut ExtensionApi) {
        register_omo_cpa(api, self.options.clone(), self.runtime.clone());
    }
}

/// Wire everything `extension.ts` registers onto one API.
pub fn register_omo_cpa(api: &mut ExtensionApi, options: OmoCpaOptions, runtime: ProviderRuntime) {
    let state: SharedState = Arc::new(Mutex::new(State::new()));
    let _ = prune_unrestorable_store_entries(
        Some(&options.models_store_path),
        &[PROVIDER_NAME, LAST_RESORT_PROVIDER_NAME],
    );

    // Run registration immediately so `/login` and the model catalog are available as soon as the
    // runner binds context.
    register_provider_safely(api, &runtime, None);

    // A command handler receives only a context; this second handle shares the registration so a
    // tier change can re-register the providers immediately.
    let handler_api = Arc::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    let env = HandlerEnv {
        state: Arc::clone(&state),
        runtime: runtime.clone(),
        options: options.clone(),
        api: handler_api,
    };

    {
        let state = Arc::clone(&state);
        let env = env.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, context| {
                let state = Arc::clone(&state);
                let env = env.clone();
                let context = context.clone();
                Box::pin(async move {
                    // Never block session startup on the network.
                    tokio::spawn(async move {
                        let registry = context.model_registry.clone();
                        let _ = registry
                            .refresh(maho_ai::models::ModelsRefreshOptions {
                                allow_network: Some(true),
                                providers: Some(vec![
                                    PROVIDER_NAME.to_owned(),
                                    LAST_RESORT_PROVIDER_NAME.to_owned(),
                                ]),
                                force: None,
                                signal: None,
                            })
                            .await;
                        refresh(&env, false).await;
                        set_status(&state, &context);
                        let (tiers, reason) = {
                            let state = lock(&state);
                            (state.tiers, state.tier_reason.clone())
                        };
                        if tiers.is_none()
                            && let Some(reason) = reason
                        {
                            notify(
                                &context,
                                &format!("CPA 모델 목록 불러오기 실패 · {reason}"),
                                Level::Info,
                            );
                        }
                    });
                    Ok(EventResult::None)
                })
            }),
        );
    }

    {
        let state = Arc::clone(&state);
        api.on(
            EventKind::ModelSelect,
            Arc::new(move |_event, context| {
                let state = Arc::clone(&state);
                let context = context.clone();
                Box::pin(async move {
                    set_status(&state, &context);
                    Ok(EventResult::None)
                })
            }),
        );
    }

    {
        let state = Arc::clone(&state);
        api.on(
            EventKind::BeforeProviderRequest,
            Arc::new(move |event, _context| {
                let provider = match event {
                    ExtensionEvent::BeforeProviderRequest { model, .. } => {
                        model.as_ref().map(|model| model.provider.clone())
                    }
                    _ => None,
                };
                if let Some(provider) = provider {
                    lock(&state).last_request_was_cpa =
                        provider == PROVIDER_NAME || provider == LAST_RESORT_PROVIDER_NAME;
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }

    {
        let state = Arc::clone(&state);
        api.on(
            EventKind::AfterProviderResponse,
            Arc::new(move |event, context| {
                let state = Arc::clone(&state);
                let (status, headers) = match event {
                    ExtensionEvent::AfterProviderResponse { status, headers } => {
                        (*status, headers.clone())
                    }
                    _ => return Box::pin(async { Ok(EventResult::None) }),
                };
                let context = context.clone();
                Box::pin(async move {
                    let was_cpa = {
                        let mut state = lock(&state);
                        let was_cpa = state.last_request_was_cpa;
                        state.last_request_was_cpa = false;
                        was_cpa
                    };
                    if !was_cpa {
                        return Ok(EventResult::None);
                    }
                    let before = lock(&state).health.snapshot().state;
                    let after = {
                        let mut state = lock(&state);
                        state.health.record(status, &headers, now_ms()).state
                    };
                    set_status(&state, &context);
                    if before != after
                        && matches!(after, HealthState::Down | HealthState::RateLimited)
                    {
                        let detail = lock(&state).health.snapshot().detail;
                        notify(&context, &format!("CPA {detail}"), Level::Error);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    let command_env = env;
    api.register_command(
        COMMAND_NAME,
        Some(COMMAND_DESCRIPTION.to_owned()),
        Some(COMMAND_ARGUMENT_HINT.to_owned()),
        Arc::new(move |args, context| {
            let env = command_env.clone();
            Box::pin(async move {
                handle_cpa_command(args, context, &env).await;
                Ok(())
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> ExtensionApi {
        ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "omo-cpa",
                std::env::current_dir().expect("cwd"),
                Default::default(),
            ),
            Default::default(),
            Default::default(),
            Default::default(),
        )
    }

    fn temp_options(dir: &Path) -> OmoCpaOptions {
        OmoCpaOptions {
            override_path: dir.join("tier-overrides.json"),
            management_key_path: dir.join("cpa-management-key"),
            models_store_path: dir.join("models-store.json"),
        }
    }

    #[test]
    fn registration_wires_the_command_hooks_and_both_providers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut api = api();
        register_omo_cpa(&mut api, temp_options(dir.path()), ProviderRuntime::new());
        let names: Vec<&str> = api
            .registered
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, vec![COMMAND_NAME]);
        let command = &api.registered.commands[0];
        assert_eq!(command.description.as_deref(), Some(COMMAND_DESCRIPTION));
        assert_eq!(
            command.argument_hint.as_deref(),
            Some(COMMAND_ARGUMENT_HINT)
        );
        for kind in [
            EventKind::SessionStart,
            EventKind::ModelSelect,
            EventKind::BeforeProviderRequest,
            EventKind::AfterProviderResponse,
        ] {
            assert!(
                api.registered.handlers.contains_key(&kind),
                "{kind:?} must have a handler"
            );
        }
    }

    #[test]
    fn the_tier_summary_lists_every_pool_and_override_state() {
        use crate::tier::{InactiveOverride, TierReport};
        use crate::tier_types::TierDecision;

        let decision = |id: &str, tier: Tier, overridden: bool| TierDecision {
            id: id.to_owned(),
            tier,
            family: None,
            reason: String::new(),
            overridden,
        };
        let tiered = TieredProviderData {
            catalog: Vec::new(),
            report: TierReport {
                decisions: vec![
                    decision("claude-opus-5", Tier::Primary, true),
                    decision("gpt-5.5", Tier::Primary, false),
                ],
                primary: vec![decision("claude-opus-5", Tier::Primary, true)],
                last: vec![decision("junk", Tier::Last, false)],
                chat_unfit: vec![decision("lyria-3", Tier::Last, false)],
                inactive_overrides: vec![InactiveOverride {
                    id: "absent".into(),
                    tier: Tier::Primary,
                    reason: "gone".into(),
                }],
            },
            primary_models: Vec::new(),
            last_models: Vec::new(),
            stats: crate::provider::Stats::default(),
        };
        let summary = render_tier_summary(&tiered);
        assert!(summary.starts_with("CPA 모델 tier"));
        assert!(summary.contains("primary (1)\n  claude-opus-5"));
        assert!(summary.contains("last-resort (1)\n  junk"));
        assert!(summary.contains("chat-unfit (명시 선택만 가능, 체인 제외) (1)\n  lyria-3"));
        assert!(summary.contains("활성 override (1)\n  claude-opus-5 → primary"));
        assert!(summary.contains("비활성 override (1)\n  absent → primary"));
    }

    #[test]
    fn an_empty_pool_reads_as_none() {
        let summary = render_tier_summary(&TieredProviderData::default());
        assert!(summary.contains("primary (0) · 없음"));
        assert!(summary.contains("활성 override (0) · 없음"));
    }

    #[test]
    fn the_help_text_documents_every_supported_argument() {
        let help = help_text(Path::new("/home/u/.omo/agent/cpa-management-key"));
        for expected in [
            "/cpa help",
            "/cpa refresh",
            "/cpa management [status]",
            "/cpa management set",
            "/cpa management clear",
            "/cpa tier",
            "/cpa tier promote <model-id>",
            "/cpa tier demote <model-id>",
            "/cpa tier reset <model-id>",
            "/cpa chains",
            "/cpa chains apply",
            "0600",
            "/home/u/.omo/agent/cpa-management-key",
            "OMO_CPA_MANAGEMENT_KEY 환경변수가 저장된 키보다 우선합니다.",
        ] {
            assert!(help.contains(expected), "help must mention {expected}");
        }
    }

    #[test]
    fn the_status_line_reads_the_shared_state() {
        let state: SharedState = Arc::new(Mutex::new(State::new()));
        {
            let mut state = lock(&state);
            state.tiers = Some(TierCounts {
                primary: 4,
                last: 6,
                chat_unfit: 2,
            });
            state.health.record(200, &BTreeMap::new(), 1);
        }
        let (tiers, health) = {
            let state = lock(&state);
            (state.tiers, state.health.snapshot())
        };
        assert_eq!(
            render_status_line(tiers, &health),
            "CPA 정상 · 주력 4종 · 최후 6종"
        );
    }

    #[test]
    fn short_hostname_drops_the_domain() {
        assert!(!short_hostname().contains('.'));
    }
}
