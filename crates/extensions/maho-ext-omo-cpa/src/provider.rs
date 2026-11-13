//! Port of omo-cpa `src/provider.ts`. CPA provider registration and catalog-to-runtime model
//! conversion.
//!
//! Two providers are registered: `cliproxyapi` for primary workhorses and `cliproxyapi-last` for
//! explicit last-resort routing. The latter opts out of implicit family expansion. Both refresh from
//! the same three-format catalog. The plugin reads no omo routing configuration: only the oauth
//! credential omo stores after `/login cliproxyapi`, `OMO_CPA_API_KEY`, and the endpoint root.

use crate::catalog::CatalogCache;
use crate::config::{DEFAULT_BASE_URL, load_config_from_process, to_root};
use crate::endpoint::{CatalogFetchResult, select_endpoint};
use crate::pricing::{self, ModelCost, ZERO_COST};
use crate::tier::{self, OverrideMap, TierReport};
use crate::tier_types::{CatalogModel, Endpoint, ModalitySource, Tier};
use maho_ai::auth::types::OAuthCredential;
use maho_ai::model::Model;
use maho_ai::models::{ModelsPublication, RefreshModelsContext};
use maho_ai::models_store::ModelsStoreEntry;
use maho_ai::oauth::{OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt};
use maho_ai::types::{
    InputModality, ModelCost as AiModelCost, ModelThinkingLevel, ThinkingLevelMap,
};
use maho_ext_api::{
    ExtensionApi, ExtensionFailure, ExtensionFuture, ExtensionOAuthConfig, ProviderConfig,
    ProviderModelConfig,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const PROVIDER_NAME: &str = "cliproxyapi";
/// Explicit-only provider: its hook removes it from senpi's implicit family expansion.
pub const LAST_RESORT_PROVIDER_NAME: &str = "cliproxyapi-last";

/// How long a model stays registered after the live list last named it.
pub const STICKY_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Stable, real CPA models used as the persisted last-resort chain tail.
pub const STABLE_LAST_RESORT_IDS: &[&str] = &["higher-coding", "lower-coding"];

pub const MAX_TOKENS_CEIL: u64 = 250_000;
pub const DEFAULT_CONTEXT_WINDOW: u64 = 8_192;
pub const DEFAULT_MAX_TOKENS: u64 = 4_096;
pub const DEFAULT_MAX_TOKENS_FOR_DEFAULT_CONTEXT: u64 = 2_048;
pub const CONTEXT_WINDOW_CEIL: u64 = 2_000_000;

/// One model as the provider registration describes it, before the ext-api mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub input: Vec<InputModality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub upstream_model_id: Option<String>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
}

impl ProviderModel {
    pub fn into_config(self) -> ProviderModelConfig {
        ProviderModelConfig {
            model_type: None,
            output: None,
            image_cost: None,
            input_limits: None,
            prompt_cache: None,
            supports_assistant_prefill: None,
            default_thinking_level: None,
            id: self.id,
            name: self.name,
            upstream_model_id: self.upstream_model_id,
            api: self.api,
            base_url: self.base_url,
            reasoning: self.reasoning,
            recover_text_tool_calls: None,
            thinking_level_map: self.thinking_level_map,
            input: self.input,
            cost: ai_cost(self.cost),
            context_window: self.context_window,
            max_tokens: self.max_tokens,
            headers: None,
            extra_body: None,
            compat: None,
        }
    }
}

fn ai_cost(cost: ModelCost) -> AiModelCost {
    AiModelCost {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        ..AiModelCost::default()
    }
}

/// The runtime model a refresh round publishes, so the store round-trips the registry's own shape.
pub fn config_into_model(config: &ProviderModelConfig, provider: &str) -> Model {
    Model {
        id: config.id.clone(),
        name: config.name.clone(),
        api: config
            .api
            .clone()
            .unwrap_or_else(|| "openai-responses".to_owned()),
        provider: provider.to_owned(),
        base_url: config.base_url.clone().unwrap_or_default(),
        reasoning: config.reasoning,
        default_thinking_level: config.default_thinking_level,
        thinking_level_map: config.thinking_level_map.clone(),
        prompt_cache: config.prompt_cache.clone(),
        supports_assistant_prefill: config.supports_assistant_prefill,
        input: config.input.clone(),
        cost: config.cost.clone(),
        context_window: config.context_window,
        max_tokens: config.max_tokens,
        sampling_params: None,
        headers: config.headers.clone(),
        cache_retention: None,
        upstream_model_id: config.upstream_model_id.clone(),
        deprecated: None,
        service_tier: None,
        recover_text_tool_calls: config.recover_text_tool_calls,
        compat: config.compat.clone(),
        input_limits: config.input_limits.clone(),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderData {
    pub catalog: Vec<CatalogModel>,
    /// Per-model price by id, when the pricing catalog resolved one.
    pub costs: BTreeMap<String, ModelCost>,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderRegistrationData {
    pub catalog: Vec<CatalogModel>,
    pub costs: BTreeMap<String, ModelCost>,
    pub base_url: Option<String>,
    pub overrides: OverrideMap,
}

#[derive(Clone, Debug, Default)]
pub struct TieredProviderData {
    pub catalog: Vec<CatalogModel>,
    pub report: TierReport,
    pub primary_models: Vec<ProviderModelConfig>,
    pub last_models: Vec<ProviderModelConfig>,
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub real_context: usize,
    pub defaulted_context: usize,
    pub clamped_max_tokens: usize,
    pub overruled_context: usize,
    pub input_from_gemini: usize,
    pub input_from_codex: usize,
    pub input_from_default: usize,
}

/// The connection the catalog fetch uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredConnection {
    pub api_key: String,
    pub base_url: String,
}

/// Keep the stable last-resort tail even when a volatile catalog omits it.
pub fn append_stable_last_resorts(catalog: &[CatalogModel]) -> Vec<CatalogModel> {
    let mut models = catalog.to_vec();
    let present: BTreeSet<String> = models.iter().map(|model| model.id.clone()).collect();
    for id in STABLE_LAST_RESORT_IDS {
        if present.contains(*id) {
            continue;
        }
        models.push(CatalogModel {
            id: (*id).to_owned(),
            owned_by: None,
            display_name: Some((*id).to_owned()),
            context_length: Some(196_608),
            max_tokens: Some(65_536),
            input_modalities: Some(vec!["TEXT".to_owned(), "IMAGE".to_owned()]),
            input_modalities_source: None,
            output_modalities: Some(vec!["TEXT".to_owned()]),
            thinking: Some(true),
            upstream_model_id: None,
        });
    }
    models
}

/// Build both disjoint provider model sets from the one merged endpoint catalog.
pub fn build_provider_registration(data: &ProviderRegistrationData) -> TieredProviderData {
    let catalog = append_stable_last_resorts(&data.catalog);
    let report = tier::build_tier_report(&catalog, &data.overrides, &BTreeMap::new());
    let base_url = to_root(data.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL));
    let mut stats = Stats::default();
    let converted: BTreeMap<String, ProviderModelConfig> = catalog
        .iter()
        .map(|model| {
            (
                model.id.clone(),
                to_provider_model(
                    model,
                    &BTreeMap::new(),
                    &mut stats,
                    &base_url,
                    data.costs.get(&model.id).copied(),
                )
                .into_config(),
            )
        })
        .collect();

    let select = |rows: &[crate::tier_types::TierDecision]| -> Vec<ProviderModelConfig> {
        rows.iter()
            .filter_map(|row| converted.get(&row.id).cloned())
            .collect()
    };
    let primary_models = select(&report.primary);
    // Chat-unfit models remain explicitly selectable, but never enter primary or a chain.
    let last_models = report
        .last
        .iter()
        .chain(report.chat_unfit.iter())
        .filter_map(|row| converted.get(&row.id).cloned())
        .collect();

    TieredProviderData {
        catalog,
        report,
        primary_models,
        last_models,
        stats,
    }
}

/// Register both providers synchronously. A supplied snapshot takes effect immediately.
///
/// # Errors
/// Returns the registry's registration failure so the caller can decide whether to fail open.
pub fn register_cpa_provider(
    api: &ExtensionApi,
    data: Option<&ProviderRegistrationData>,
    runtime: Option<&ProviderRuntime>,
) -> Result<(), ExtensionFailure> {
    let tiered = data.map(build_provider_registration);
    let connection = read_connection_sync();
    let base_url = to_root(
        data.and_then(|data| data.base_url.clone())
            .as_deref()
            .unwrap_or(&connection.base_url),
    );

    api.register_provider(
        PROVIDER_NAME,
        provider_config(
            Tier::Primary,
            tiered
                .as_ref()
                .map(|tiered| tiered.primary_models.clone())
                .unwrap_or_default(),
            &connection.api_key,
            &base_url,
            runtime,
        ),
    )?;
    api.register_provider(
        LAST_RESORT_PROVIDER_NAME,
        provider_config(
            Tier::Last,
            tiered
                .as_ref()
                .map(|tiered| tiered.last_models.clone())
                .unwrap_or_default(),
            &connection.api_key,
            &base_url,
            runtime,
        ),
    )
}

/// The cache and HTTP client both providers share, so one refresh round fans out once.
#[derive(Clone, Default)]
pub struct ProviderRuntime {
    pub catalog: CatalogCache,
    pub client: reqwest::Client,
}

impl ProviderRuntime {
    pub fn new() -> Self {
        Self {
            catalog: CatalogCache::new(),
            client: reqwest::Client::new(),
        }
    }
}

/// Everything one tier's refresh lane closes over.
#[derive(Clone)]
struct RefreshLane {
    tier: Tier,
    connection_api_key: String,
    connection_base_url: String,
    runtime: ProviderRuntime,
    registered: Arc<Mutex<Vec<ProviderModelConfig>>>,
    state: Arc<Mutex<RefreshState>>,
}

fn provider_config(
    tier: Tier,
    models: Vec<ProviderModelConfig>,
    api_key: &str,
    base_url: &str,
    runtime: Option<&ProviderRuntime>,
) -> ProviderConfig {
    let lane = RefreshLane {
        tier,
        connection_api_key: api_key.to_owned(),
        connection_base_url: base_url.to_owned(),
        runtime: runtime.cloned().unwrap_or_default(),
        registered: Arc::new(Mutex::new(models.clone())),
        state: Arc::new(Mutex::new(RefreshState::default())),
    };
    let common = ProviderConfig {
        name: Some(provider_label(tier).to_owned()),
        base_url: Some(base_url.to_owned()),
        auth_header: Some(true),
        api_key: (!api_key.is_empty()).then(|| api_key.to_owned()),
        api: Some("openai-responses".to_owned()),
        models: Some(models),
        refresh_models: Some(Arc::new(move |context| {
            let lane = lane.clone();
            Box::pin(async move { Ok(refresh_models(context, lane).await) })
        })),
        ..ProviderConfig::default()
    };
    // Only the primary provider has oauth: one /login covers both tiers. The last-resort provider
    // borrows the credential stored for 'cliproxyapi' from auth.json instead of requiring a second
    // login.
    if tier == Tier::Last {
        return ProviderConfig {
            fallback_eligible: Some(Arc::new(|| false)),
            ..common
        };
    }
    ProviderConfig {
        oauth: Some(Arc::new(maho_ext_host::oauth::ExtensionOAuthAdapter::new(
            Arc::new(CpaOAuth),
        ))),
        ..common
    }
}

fn provider_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Primary => "CLI Proxy API (CPA)",
        Tier::Last => "CLI Proxy API (CPA Last Resort)",
    }
}

#[derive(Default)]
struct RefreshState {
    last_seen: BTreeMap<String, i64>,
}

async fn refresh_models(
    context: RefreshModelsContext,
    lane: RefreshLane,
) -> Vec<ProviderModelConfig> {
    let current = lane
        .registered
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if context.signal.aborted() {
        return current;
    }

    let migration = read_migration_source();
    let api_key = credential_api_key(context.credential.as_ref())
        .or(migration.api_key)
        .unwrap_or_else(|| lane.connection_api_key.clone());
    let base_url = credential_base_url(context.credential.as_ref())
        .or(Some(migration.base_url))
        .unwrap_or_else(|| lane.connection_base_url.clone());

    // Restore the persisted catalog before anything can leave it empty. A restore that is skipped
    // hands the session an empty model list whenever the fetch cannot run, and every fallback chain
    // key then validates as an unknown selector.
    let restored = read_stored_models(context.stored.as_ref(), lane.tier);
    let current = if current.is_empty() && !restored.is_empty() {
        restored.clone()
    } else {
        current
    };

    if api_key.is_empty() {
        publish_models(&context, lane.tier, &current).await;
        return current;
    }
    // Reuse an existing snapshot during the restore phase.
    if !context.allow_network && !current.is_empty() {
        return current;
    }

    let now = now_ms();
    let fetched = lane
        .runtime
        .catalog
        .get(
            &lane.runtime.client,
            &base_url,
            &api_key,
            context.force == Some(true),
            now,
            crate::catalog::ttl_from_env(),
        )
        .await;
    if context.signal.aborted() {
        return current;
    }
    let CatalogFetchResult::Ok(fetched) = fetched else {
        publish_models(&context, lane.tier, &current).await;
        return current;
    };

    let pricing = pricing::fetch_pricing_index(
        &lane.runtime.client,
        &pricing::pricing_url(),
        pricing::DEFAULT_PRICING_TIMEOUT_MS,
    )
    .await;
    // An abort while pricing was pending must not publish a half-refreshed list.
    if context.signal.aborted() {
        return current;
    }
    let store = tier::load_override_store(&crate::tier::overrides_file());
    let built = build_provider_registration(&ProviderRegistrationData {
        catalog: fetched.models.clone(),
        costs: pricing
            .as_ref()
            .map(|index| pricing::resolve_catalog_costs(index, &fetched.models))
            .unwrap_or_default(),
        base_url: Some(base_url.clone()),
        overrides: tier::to_override_map(&store),
    });

    let mut next = match lane.tier {
        Tier::Primary => built.primary_models.clone(),
        Tier::Last => built.last_models.clone(),
    };
    let carried: BTreeSet<&str> = fetched.retained_ids.iter().map(String::as_str).collect();
    // Scoped so the guard is provably released before the publish await below.
    let prior_seen = lane
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last_seen
        .clone();
    let mut next_seen = BTreeMap::new();
    for model in &fetched.models {
        let seen_at = if carried.contains(model.id.as_str()) {
            prior_seen.get(&model.id).copied().unwrap_or(now)
        } else {
            now
        };
        next_seen.insert(model.id.clone(), seen_at);
    }
    let listed: BTreeSet<&str> = next.iter().map(|model| model.id.as_str()).collect();
    // A model the live list stopped naming stays for the sticky window, so a chain that names a
    // briefly hidden alias keeps validating.
    let sticky: Vec<ProviderModelConfig> = current
        .iter()
        .filter(|model| {
            !listed.contains(model.id.as_str())
                && prior_seen
                    .get(&model.id)
                    .is_some_and(|seen_at| now - seen_at <= STICKY_TTL_MS)
        })
        .cloned()
        .collect();
    for model in &sticky {
        if let Some(seen_at) = prior_seen.get(&model.id) {
            next_seen.insert(model.id.clone(), *seen_at);
        }
    }
    next.extend(sticky);

    // Some windows come from one list alone (grok's only from the Codex list). When that list misses
    // a round the model falls to the 8192 default, too small to start a session, and the store
    // spreads it to every later process. Keep the last window the server reported.
    let known: BTreeMap<&str, &ProviderModelConfig> = restored
        .iter()
        .chain(current.iter())
        .map(|model| (model.id.as_str(), model))
        .collect();
    let unreported: BTreeSet<&str> = built
        .catalog
        .iter()
        .filter(|model| model.context_length.is_none())
        .map(|model| model.id.as_str())
        .collect();
    let models: Vec<ProviderModelConfig> = next
        .into_iter()
        .map(|model| keep_known_window(model, &known, &unreported))
        .collect();
    lane.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last_seen = next_seen;

    publish_models(&context, lane.tier, &models).await;

    let mut registered = lane
        .registered
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *registered = models.clone();
    models
}

fn keep_known_window(
    model: ProviderModelConfig,
    known: &BTreeMap<&str, &ProviderModelConfig>,
    unreported: &BTreeSet<&str>,
) -> ProviderModelConfig {
    if !unreported.contains(model.id.as_str()) || model.context_window != DEFAULT_CONTEXT_WINDOW {
        return model;
    }
    match known.get(model.id.as_str()) {
        Some(prior) if prior.context_window > DEFAULT_CONTEXT_WINDOW => ProviderModelConfig {
            context_window: prior.context_window,
            max_tokens: prior.max_tokens,
            ..model
        },
        _ => model,
    }
}

/// Publishes a persisted catalog entry. An empty list is a failed or credential-less refresh, never
/// evidence that the provider lost its models, so nothing is persisted then.
async fn publish_models(
    context: &RefreshModelsContext,
    tier: Tier,
    models: &[ProviderModelConfig],
) {
    if models.is_empty() {
        let _ = context
            .publish(ModelsPublication {
                persist: None,
                update: None,
            })
            .await;
        return;
    }
    let _ = context
        .publish(ModelsPublication {
            persist: Some(Some(store_entry(tier, models, now_ms() / 1000))),
            update: None,
        })
        .await;
}

pub fn store_entry(
    tier: Tier,
    models: &[ProviderModelConfig],
    checked_at: i64,
) -> ModelsStoreEntry {
    ModelsStoreEntry {
        models: models
            .iter()
            .map(|model| config_into_model(model, provider_name(tier)))
            .collect(),
        checked_at: Some(checked_at),
        ..ModelsStoreEntry::default()
    }
}

fn provider_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Primary => PROVIDER_NAME,
        Tier::Last => LAST_RESORT_PROVIDER_NAME,
    }
}

/// Catalog models senpi persisted for this provider in an earlier session.
///
/// senpi's store is provider-keyed, so the entry names its own tier through the models' `provider`
/// field; an entry only counts when that matches. This is the native equivalent of the TS
/// `stored.tier` tag, which the ext-api store entry has no field for.
pub fn read_stored_models(
    stored: Option<&ModelsStoreEntry>,
    tier: Tier,
) -> Vec<ProviderModelConfig> {
    let Some(stored) = stored else {
        return Vec::new();
    };
    let provider = provider_name(tier);
    stored
        .models
        .iter()
        .filter(|model| model.provider == provider || model.provider.is_empty())
        .filter(|model| !model.id.is_empty() && model.context_window > 0 && model.max_tokens > 0)
        .map(provider_config_from_model)
        .collect()
}

fn provider_config_from_model(model: &Model) -> ProviderModelConfig {
    ProviderModelConfig {
        model_type: None,
        output: None,
        image_cost: None,
        input_limits: model.input_limits.clone(),
        prompt_cache: model.prompt_cache.clone(),
        supports_assistant_prefill: model.supports_assistant_prefill,
        default_thinking_level: model.default_thinking_level,
        id: model.id.clone(),
        name: model.name.clone(),
        upstream_model_id: model.upstream_model_id.clone(),
        api: (!model.api.is_empty()).then(|| model.api.clone()),
        base_url: (!model.base_url.is_empty()).then(|| model.base_url.clone()),
        reasoning: model.reasoning,
        recover_text_tool_calls: model.recover_text_tool_calls,
        thinking_level_map: model.thinking_level_map.clone(),
        input: if model.input.is_empty() {
            vec![InputModality::Text]
        } else {
            model.input.clone()
        },
        cost: model.cost.clone(),
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        headers: model.headers.clone(),
        extra_body: None,
        compat: model.compat.clone(),
    }
}

/// Drop our own stored catalog entry when senpi cannot restore it.
///
/// senpi replays the persisted payload through `entry.models.filter(...)` before it calls
/// `refreshModels`, so an entry without a `models` array throws there. Removing it costs one catalog
/// fetch. Other providers' entries are never touched.
pub fn prune_unrestorable_store_entries(path: Option<&Path>, provider_ids: &[&str]) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(Value::Object(mut store)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let removed: Vec<String> = provider_ids
        .iter()
        .filter(|id| {
            store
                .get(**id)
                .is_some_and(|entry| !has_restorable_models(entry))
        })
        .map(|id| (*id).to_owned())
        .collect();
    if removed.is_empty() {
        return Vec::new();
    }
    for id in &removed {
        store.remove(id);
    }
    let text = serde_json::to_string_pretty(&Value::Object(store)).unwrap_or_default();
    if std::fs::write(path, text).is_err() {
        return Vec::new();
    }
    removed
}

fn has_restorable_models(entry: &Value) -> bool {
    entry
        .as_object()
        .is_some_and(|object| object.get("models").is_some_and(Value::is_array))
}

/// `~/.omo/agent/models-store.json`, or `<CODING_AGENT_DIR>/models-store.json`.
pub fn default_models_store_path() -> PathBuf {
    crate::management_key::agent_dir().join("models-store.json")
}

/// Read the connection that omo stored after `/login cliproxyapi`.
pub fn read_stored_primary_connection(path: &Path) -> Option<StoredConnection> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let credential = parsed.get(PROVIDER_NAME)?;
    let api_key = credential_api_key(Some(credential))?;
    let base_url = credential_base_url(Some(credential))?;
    (!api_key.is_empty() && !base_url.is_empty()).then_some(StoredConnection { api_key, base_url })
}

/// The same connection read from the default auth.json.
pub fn read_stored_primary_connection_default() -> Option<StoredConnection> {
    read_stored_primary_connection(&crate::management_key::agent_dir().join("auth.json"))
}

pub fn read_stored_primary_credential(path: Option<&Path>) -> Option<String> {
    path.map_or_else(read_stored_primary_connection_default, |path| {
        read_stored_primary_connection(path)
    })
    .map(|connection| connection.api_key)
}

/// The inference key and endpoint, before oauth supplies either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationSource {
    pub api_key: Option<String>,
    pub base_url: String,
    pub has_api_key: bool,
}

pub fn read_migration_source() -> MigrationSource {
    let connection = read_connection_sync();
    MigrationSource {
        has_api_key: !connection.api_key.is_empty(),
        api_key: (!connection.api_key.is_empty()).then_some(connection.api_key),
        base_url: connection.base_url,
    }
}

/// Resolve the connection from the environment and the stored credential, without any network.
pub fn connection_from(
    env_api_key: Option<&str>,
    env_base_url: Option<&str>,
    stored: Option<&StoredConnection>,
    config_root: &str,
) -> StoredConnection {
    let api_key = trimmed(env_api_key)
        .or_else(|| stored.map(|connection| connection.api_key.clone()))
        .unwrap_or_default();
    let base_url = trimmed(env_base_url)
        .or_else(|| stored.map(|connection| connection.base_url.clone()))
        .unwrap_or_else(|| config_root.to_owned());
    StoredConnection {
        api_key,
        base_url: to_root(&base_url),
    }
}

/// Synchronous because provider registration itself is synchronous and in-memory.
pub fn read_connection_sync() -> StoredConnection {
    let stored = read_stored_primary_connection_default();
    let loaded = load_config_from_process(None, None);
    connection_from(
        std::env::var("OMO_CPA_API_KEY").ok().as_deref(),
        std::env::var("OMO_CPA_BASE_URL").ok().as_deref(),
        stored.as_ref(),
        &loaded.config.root,
    )
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Kept for call-site compatibility; the plugin reads no omo-owned configuration to migrate.
pub fn migrate_config_background() {}

/// Load the live merged catalog for commands that must re-register immediately.
///
/// # Errors
/// Returns the fetch reason when the catalog cannot be loaded, and a keyless message when no
/// inference key is available.
pub async fn load_provider_data(
    runtime: &ProviderRuntime,
    api_key: Option<&str>,
    base_url: Option<&str>,
    force: bool,
) -> Result<ProviderData, String> {
    let migration = read_migration_source();
    let api_key = api_key
        .map(str::to_owned)
        .or(migration.api_key)
        .unwrap_or_default();
    let base_url = to_root(base_url.unwrap_or(&migration.base_url));
    if api_key.is_empty() {
        return Err(
            "CPA 추론 키가 없습니다. /login cliproxyapi 또는 OMO_CPA_API_KEY를 설정하세요"
                .to_owned(),
        );
    }
    let result = runtime
        .catalog
        .get(
            &runtime.client,
            &base_url,
            &api_key,
            force,
            now_ms(),
            crate::catalog::ttl_from_env(),
        )
        .await;
    let fetched = match result {
        CatalogFetchResult::Ok(fetched) => fetched,
        CatalogFetchResult::Err { reason, .. } => return Err(reason),
    };
    let pricing = pricing::fetch_pricing_index(
        &runtime.client,
        &pricing::pricing_url(),
        pricing::DEFAULT_PRICING_TIMEOUT_MS,
    )
    .await;
    Ok(ProviderData {
        costs: pricing
            .as_ref()
            .map(|index| pricing::resolve_catalog_costs(index, &fetched.models))
            .unwrap_or_default(),
        catalog: fetched.models,
        base_url: Some(base_url),
    })
}

/// `toProviderModel`: the runtime model for one catalog row.
pub fn to_provider_model(
    model: &CatalogModel,
    curated: &BTreeMap<String, (u64, u64)>,
    stats: &mut Stats,
    base_url: &str,
    cost: Option<ModelCost>,
) -> ProviderModel {
    let stable_last = STABLE_LAST_RESORT_IDS.contains(&model.id.as_str());
    // Mistral models support reasoning_effort high/none, but CPA's /v1/models does not advertise a
    // thinking flag for them. Force reasoning support so omo exposes the thinking level.
    let is_mistral = model.id.to_lowercase().contains("mistral")
        || model
            .upstream_model_id
            .as_deref()
            .is_some_and(|upstream| upstream.to_lowercase().contains("mistral"));
    let curated_entry = curated.get(&model.id).copied();

    let mut context_window;
    if stable_last {
        context_window = 196_608;
        stats.real_context += 1;
    } else if let (Some(context_length), Some((curated_context, _))) =
        (model.context_length, curated_entry)
    {
        context_window = context_length.min(curated_context);
        if context_window != context_length || context_window != curated_context {
            stats.overruled_context += 1;
        }
        stats.real_context += 1;
    } else if let Some(context_length) = model.context_length {
        context_window = context_length;
        stats.real_context += 1;
    } else if let Some((curated_context, _)) = curated_entry {
        context_window = curated_context;
        stats.real_context += 1;
    } else {
        context_window = DEFAULT_CONTEXT_WINDOW;
        stats.defaulted_context += 1;
    }
    context_window = context_window.min(CONTEXT_WINDOW_CEIL);

    let reported_max = if stable_last {
        65_536
    } else {
        curated_entry
            .map(|(_, curated_max)| curated_max)
            .or(model.max_tokens)
            .unwrap_or(0)
    };
    let mut max_tokens = if reported_max > 0 {
        reported_max
            .min(context_window.saturating_sub(1))
            .min(MAX_TOKENS_CEIL)
    } else if context_window <= DEFAULT_CONTEXT_WINDOW {
        DEFAULT_MAX_TOKENS_FOR_DEFAULT_CONTEXT
    } else {
        DEFAULT_MAX_TOKENS
    };
    if max_tokens < reported_max {
        stats.clamped_max_tokens += 1;
    }
    if max_tokens == 0 {
        max_tokens = 1;
    }

    let input = if stable_last {
        vec![InputModality::Text, InputModality::Image]
    } else {
        input_modalities(model.input_modalities.as_deref())
    };
    // Provenance counts describe what the LISTS declared, so the two synthesized last-resort models
    // are left out rather than inflating a source they did not come from.
    if !stable_last {
        match (
            model.input_modalities.is_some(),
            model.input_modalities_source,
        ) {
            (true, Some(ModalitySource::Codex)) => stats.input_from_codex += 1,
            (true, _) => stats.input_from_gemini += 1,
            (false, _) => stats.input_from_default += 1,
        }
    }

    let endpoint = select_endpoint(model);
    let thinking_level_map = thinking_level_map(&model.id, is_mistral);

    ProviderModel {
        id: model.id.clone(),
        name: model
            .display_name
            .as_deref()
            .map(|name| name.trim_start_matches('*').to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| model.id.clone()),
        reasoning: stable_last || is_mistral || model.thinking == Some(true),
        input,
        cost: cost.unwrap_or(ZERO_COST),
        context_window,
        max_tokens,
        api: Some(api_for_endpoint(endpoint.endpoint).to_owned()),
        base_url: Some(format!("{base_url}{}", endpoint.base_url_suffix)),
        upstream_model_id: model.upstream_model_id.clone(),
        thinking_level_map,
    }
}

fn api_for_endpoint(endpoint: Endpoint) -> &'static str {
    match endpoint {
        Endpoint::Anthropic => "anthropic-messages",
        Endpoint::Gemini => "google-generative-ai",
        Endpoint::Openai => "openai-responses",
    }
}

fn thinking_level_map(id: &str, is_mistral: bool) -> Option<ThinkingLevelMap> {
    let mut map = ThinkingLevelMap::new();
    if id.starts_with("gpt-5.6") {
        for (key, value) in [
            (ModelThinkingLevel::Off, "none"),
            (ModelThinkingLevel::Minimal, "minimal"),
            (ModelThinkingLevel::Low, "low"),
            (ModelThinkingLevel::Medium, "medium"),
            (ModelThinkingLevel::High, "high"),
            (ModelThinkingLevel::Xhigh, "xhigh"),
            (ModelThinkingLevel::Max, "max"),
        ] {
            map.insert(key, Some(value.to_owned()));
        }
        return Some(map);
    }
    if STABLE_LAST_RESORT_IDS.contains(&id) {
        for (key, value) in [
            (ModelThinkingLevel::Low, Some("low".to_owned())),
            (ModelThinkingLevel::Medium, Some("medium".to_owned())),
            (ModelThinkingLevel::High, Some("high".to_owned())),
            (ModelThinkingLevel::Xhigh, Some("xhigh".to_owned())),
            (ModelThinkingLevel::Max, None),
        ] {
            map.insert(key, value);
        }
        return Some(map);
    }
    if is_mistral {
        // Mistral only accepts reasoning_effort "high" or "none". Expose only "high" so omo never
        // sends an unsupported intermediate level.
        for key in ModelThinkingLevel::ALL {
            map.insert(
                key,
                (key == ModelThinkingLevel::High).then(|| "high".to_owned()),
            );
        }
        return Some(map);
    }
    None
}

fn input_modalities(raw: Option<&[String]>) -> Vec<InputModality> {
    let mut out: Vec<InputModality> = Vec::new();
    for item in raw.unwrap_or_default() {
        let modality = match item.to_lowercase().as_str() {
            "text" => InputModality::Text,
            "image" => InputModality::Image,
            "video" => InputModality::Video,
            _ => continue,
        };
        if !out.contains(&modality) {
            out.push(modality);
        }
    }
    if out.is_empty() {
        out.push(InputModality::Text);
    }
    out
}

fn credential_api_key(credential: Option<&Value>) -> Option<String> {
    let credential = credential?.as_object()?;
    match credential.get("type").and_then(Value::as_str) {
        Some("api_key") => credential
            .get("key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        Some("oauth") | None => credential
            .get("access")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        Some(_) => None,
    }
}

fn credential_base_url(credential: Option<&Value>) -> Option<String> {
    credential?
        .as_object()?
        .get("baseUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(to_root)
}

fn credential_for(key: String, base_url: String) -> OAuthCredentials {
    let expires = (now_ms() + 1000 * 60 * 60 * 24 * 365) as f64;
    let mut credential = OAuthCredential::new(key.clone(), key, expires);
    credential
        .extra
        .insert("baseUrl".to_owned(), Value::String(base_url));
    credential
}

/// Validate a `/login cliproxyapi` base URL exactly as the TS flow does.
pub fn parse_login_base_url(raw: &str) -> Result<String, ExtensionFailure> {
    let parsed = url::Url::parse(raw.trim())
        .map_err(|_| ExtensionFailure::new("CPA BASE URL 형식이 올바르지 않습니다"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.port().is_none() {
        return Err(ExtensionFailure::new(
            "CPA BASE URL은 http(s)와 명시적 포트를 포함해야 합니다",
        ));
    }
    Ok(to_root(parsed.as_str()))
}

/// Build the stored credential for a validated key and root.
pub fn login_credential(base_url: &str, key: &str) -> Result<OAuthCredentials, ExtensionFailure> {
    let key = key.trim();
    if key.is_empty() {
        return Err(ExtensionFailure::new("빈 키"));
    }
    Ok(credential_for(key.to_owned(), base_url.to_owned()))
}

/// The oauth flow behind `/login cliproxyapi`: prompt for the base URL and the inference key.
pub struct CpaOAuth;

impl ExtensionOAuthConfig for CpaOAuth {
    fn name(&self) -> &str {
        "CLI Proxy API (CPA)"
    }

    fn is_subscription(&self) -> bool {
        false
    }

    fn login<'a>(
        &'a self,
        callbacks: &'a dyn OAuthLoginCallbacks,
    ) -> ExtensionFuture<'a, OAuthCredentials> {
        Box::pin(async move {
            let raw_base_url = callbacks
                .on_prompt(OAuthPrompt {
                    message: "CPA BASE URL을 입력하세요 (포트 포함)".to_owned(),
                    placeholder: Some("http://host:8317".to_owned()),
                    ..Default::default()
                })
                .await;
            let base_url = parse_login_base_url(&raw_base_url)?;
            let key = callbacks
                .on_prompt(OAuthPrompt {
                    message: "CPA 추론 키(Inference Key)를 입력하세요".to_owned(),
                    placeholder: Some("senpi-... (CPA 추론 키)".to_owned()),
                    ..Default::default()
                })
                .await;
            if !key.trim().starts_with("senpi-") {
                maho_ext_api::diagnostic("[omo-cpa] 수집된 키가 senpi- 프리픽스와 다릅니다");
            }
            login_credential(&base_url, &key)
        })
    }

    fn refresh_token<'a>(
        &'a self,
        credentials: &'a OAuthCredentials,
        _signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> ExtensionFuture<'a, OAuthCredentials> {
        Box::pin(async move { Ok(credentials.clone()) })
    }

    fn get_api_key(&self, credentials: &OAuthCredentials) -> String {
        credentials.access.clone()
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(entries: &[(&str, Option<&str>)]) -> Vec<CatalogModel> {
        entries
            .iter()
            .map(|(id, context)| CatalogModel {
                context_length: context.map(str::parse).transpose().expect("context"),
                ..CatalogModel::new(*id)
            })
            .collect()
    }

    fn data(catalog: Vec<CatalogModel>) -> ProviderRegistrationData {
        ProviderRegistrationData {
            catalog,
            ..ProviderRegistrationData::default()
        }
    }

    #[test]
    fn stable_last_resorts_are_synthesized_and_never_duplicated() {
        let models =
            append_stable_last_resorts(&catalog(&[("higher-coding", None), ("gpt-5.5", None)]));
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["higher-coding", "gpt-5.5", "lower-coding"]);
        let lower = models
            .iter()
            .find(|model| model.id == "lower-coding")
            .expect("lower");
        assert_eq!(lower.context_length, Some(196_608));
        assert_eq!(lower.max_tokens, Some(65_536));
        assert_eq!(lower.thinking, Some(true));
    }

    #[test]
    fn the_tier_split_drives_both_model_sets() {
        let tiered = build_provider_registration(&data(catalog(&[
            ("gpt-5.5", Some("400000")),
            ("vendor/free-model:free", Some("8000")),
            ("google/lyria-3", Some("8000")),
        ])));
        let primary: Vec<&str> = tiered
            .primary_models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(primary, vec!["gpt-5.5"]);
        let last: Vec<&str> = tiered
            .last_models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(last.contains(&"vendor/free-model:free"));
        // Chat-unfit models stay explicitly selectable but never enter the primary pool.
        assert!(last.contains(&"google/lyria-3"));
        assert!(last.contains(&"higher-coding"));
    }

    #[test]
    fn provider_models_carry_endpoints_limits_and_costs() {
        let mut catalog = catalog(&[
            ("claude-opus-5", Some("200000")),
            ("gemini-3.1-pro", Some("1000000")),
            ("gpt-5.5", Some("400000")),
        ]);
        catalog[0].max_tokens = Some(64_000);
        catalog[0].display_name = Some("*Opus 5".into());
        catalog[2].max_tokens = Some(128_000);
        catalog[2].thinking = Some(true);
        let mut costs = BTreeMap::new();
        costs.insert(
            "gpt-5.5".to_owned(),
            ModelCost {
                input: 1.25,
                output: 10.0,
                cache_read: 0.125,
                cache_write: 1.5,
            },
        );
        let tiered = build_provider_registration(&ProviderRegistrationData {
            catalog,
            costs,
            base_url: Some("http://host:8317/v1".into()),
            overrides: OverrideMap::new(),
        });

        let opus = tiered
            .primary_models
            .iter()
            .find(|model| model.id == "claude-opus-5")
            .expect("opus");
        assert_eq!(opus.name, "Opus 5");
        assert_eq!(opus.api.as_deref(), Some("anthropic-messages"));
        assert_eq!(opus.base_url.as_deref(), Some("http://host:8317"));
        assert_eq!(opus.max_tokens, 64_000);
        assert!(!opus.reasoning);

        let gemini = tiered
            .primary_models
            .iter()
            .find(|model| model.id == "gemini-3.1-pro")
            .expect("gemini");
        assert_eq!(gemini.api.as_deref(), Some("google-generative-ai"));
        assert_eq!(gemini.base_url.as_deref(), Some("http://host:8317/v1beta"));

        let gpt = tiered
            .primary_models
            .iter()
            .find(|model| model.id == "gpt-5.5")
            .expect("gpt");
        assert_eq!(gpt.api.as_deref(), Some("openai-responses"));
        assert_eq!(gpt.base_url.as_deref(), Some("http://host:8317/v1"));
        assert!(gpt.reasoning);
        assert_eq!(gpt.cost.input, 1.25);
        assert_eq!(tiered.stats.real_context, 5);
    }

    #[test]
    fn a_missing_context_falls_to_the_documented_default_and_clamps_output() {
        let tiered = build_provider_registration(&data(catalog(&[
            ("gpt-5.5", None),
            ("gpt-5.6-sol", None),
        ])));
        let gpt = tiered
            .primary_models
            .iter()
            .find(|model| model.id == "gpt-5.6-sol")
            .expect("model");
        assert_eq!(gpt.context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(gpt.max_tokens, DEFAULT_MAX_TOKENS_FOR_DEFAULT_CONTEXT);
        assert_eq!(tiered.stats.defaulted_context, 2);
        assert!(gpt.thinking_level_map.is_some());
    }

    #[test]
    fn an_output_limit_never_equals_the_context_window() {
        let mut model = catalog(&[("claude-opus-5", Some("1000"))]).remove(0);
        model.max_tokens = Some(1000);
        let tiered = build_provider_registration(&data(vec![model]));
        let opus = tiered
            .primary_models
            .iter()
            .find(|model| model.id == "claude-opus-5")
            .expect("opus");
        assert_eq!(opus.max_tokens, 999);
        assert_eq!(tiered.stats.clamped_max_tokens, 1);
    }

    #[test]
    fn mistral_forces_reasoning_and_a_high_only_level_map() {
        let mut model = catalog(&[("mistral-large-4", Some("200000"))]).remove(0);
        model.display_name = Some("Mistral Large 4".into());
        let tiered = build_provider_registration(&data(vec![model]));
        let mistral = tiered
            .last_models
            .iter()
            .find(|model| model.id == "mistral-large-4")
            .expect("mistral");
        assert!(mistral.reasoning);
        let map = mistral.thinking_level_map.as_ref().expect("map");
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_owned()))
        );
        assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&None));
    }

    #[test]
    fn stable_last_resorts_keep_their_thinking_map_and_modalities() {
        let tiered = build_provider_registration(&data(Vec::new()));
        let higher = tiered
            .last_models
            .iter()
            .find(|model| model.id == "higher-coding")
            .expect("higher");
        assert_eq!(higher.context_window, 196_608);
        assert_eq!(higher.max_tokens, 65_536);
        assert_eq!(
            higher.input,
            vec![InputModality::Text, InputModality::Image]
        );
        let map = higher.thinking_level_map.as_ref().expect("map");
        assert_eq!(map.get(&ModelThinkingLevel::Max), Some(&None));
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_owned()))
        );
    }

    #[test]
    fn modality_mapping_drops_unknowns_and_keeps_order() {
        let modalities = input_modalities(Some(&[
            "TEXT".to_owned(),
            "AUDIO".to_owned(),
            "IMAGE".to_owned(),
            "text".to_owned(),
        ]));
        assert_eq!(modalities, vec![InputModality::Text, InputModality::Image]);
        assert_eq!(input_modalities(None), vec![InputModality::Text]);
        assert_eq!(
            input_modalities(Some(&["audio".to_owned()])),
            vec![InputModality::Text]
        );
    }

    fn sample_model(id: &str, context_window: u64) -> ProviderModelConfig {
        ProviderModel {
            id: id.into(),
            name: id.into(),
            reasoning: false,
            input: vec![InputModality::Text],
            cost: ZERO_COST,
            context_window,
            max_tokens: 64_000,
            api: Some("openai-responses".into()),
            base_url: Some("http://host:8317/v1".into()),
            upstream_model_id: None,
            thinking_level_map: None,
        }
        .into_config()
    }

    #[test]
    fn stored_models_restore_only_for_their_own_tier() {
        let stored = store_entry(Tier::Primary, &[sample_model("gpt-5.5", 400_000)], 7);
        assert_eq!(read_stored_models(Some(&stored), Tier::Primary).len(), 1);
        assert!(read_stored_models(Some(&stored), Tier::Last).is_empty());
        assert!(read_stored_models(None, Tier::Primary).is_empty());
        let restored = &read_stored_models(Some(&stored), Tier::Primary)[0];
        assert_eq!(restored.id, "gpt-5.5");
        assert_eq!(restored.context_window, 400_000);
    }

    #[test]
    fn invalid_stored_rows_are_skipped() {
        let mut entry = store_entry(Tier::Primary, &[sample_model("gpt-5.5", 400_000)], 7);
        entry.models.push(Model {
            id: String::new(),
            context_window: 0,
            max_tokens: 0,
            provider: PROVIDER_NAME.into(),
            ..entry.models[0].clone()
        });
        assert_eq!(read_stored_models(Some(&entry), Tier::Primary).len(), 1);
    }

    #[test]
    fn a_defaulted_window_is_replaced_by_the_last_reported_one() {
        let known = sample_model("grok-4.7", 500_000);
        let mut known_map = BTreeMap::new();
        known_map.insert(known.id.as_str(), &known);
        let unreported: BTreeSet<&str> = ["grok-4.7"].into_iter().collect();
        let candidate = sample_model("grok-4.7", DEFAULT_CONTEXT_WINDOW);
        let kept = keep_known_window(candidate, &known_map, &unreported);
        assert_eq!(kept.context_window, 500_000);
        assert_eq!(kept.max_tokens, 64_000);

        // A reported window is never overridden, and an unreported-but-unchanged default stays.
        let reported = sample_model("grok-4.7", 300_000);
        assert_eq!(
            keep_known_window(reported.clone(), &known_map, &unreported).context_window,
            300_000
        );
        assert_eq!(
            keep_known_window(
                sample_model("grok-4.7", DEFAULT_CONTEXT_WINDOW),
                &known_map,
                &BTreeSet::new()
            )
            .context_window,
            DEFAULT_CONTEXT_WINDOW
        );
    }

    #[test]
    fn unrecoverable_store_entries_are_pruned_and_valid_ones_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models-store.json");
        std::fs::write(
            &path,
            r#"{"cliproxyapi":{"models":[]},"cliproxyapi-last":{"checkedAt":1},"other":{"x":1}}"#,
        )
        .expect("write");
        let removed = prune_unrestorable_store_entries(
            Some(&path),
            &[PROVIDER_NAME, LAST_RESORT_PROVIDER_NAME],
        );
        assert_eq!(removed, vec![LAST_RESORT_PROVIDER_NAME.to_owned()]);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("cliproxyapi"));
        assert!(!text.contains("cliproxyapi-last"));
        assert!(text.contains("other"));
    }

    #[test]
    fn a_missing_or_corrupt_store_file_prunes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            prune_unrestorable_store_entries(Some(&dir.path().join("absent")), &[PROVIDER_NAME])
                .is_empty()
        );
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "[1,2,3]").expect("write");
        assert!(prune_unrestorable_store_entries(Some(&path), &[PROVIDER_NAME]).is_empty());
        assert!(prune_unrestorable_store_entries(None, &[PROVIDER_NAME]).is_empty());
    }

    #[test]
    fn the_stored_connection_needs_both_halves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"cliproxyapi":{"type":"oauth","access":"senpi-key","baseUrl":"http://host:8317/v1"}}"#,
        )
        .expect("write");
        let connection = read_stored_primary_connection(&path).expect("connection");
        assert_eq!(connection.api_key, "senpi-key");
        assert_eq!(connection.base_url, "http://host:8317");

        std::fs::write(
            &path,
            r#"{"cliproxyapi":{"type":"oauth","access":"senpi-key"}}"#,
        )
        .expect("write");
        assert!(read_stored_primary_connection(&path).is_none());
        assert!(read_stored_primary_connection(&dir.path().join("absent")).is_none());
    }

    #[test]
    fn api_keys_are_read_from_both_credential_shapes() {
        let oauth = serde_json::json!({"type": "oauth", "access": "a"});
        assert_eq!(credential_api_key(Some(&oauth)).as_deref(), Some("a"));
        let api_key = serde_json::json!({"type": "api_key", "key": "k"});
        assert_eq!(credential_api_key(Some(&api_key)).as_deref(), Some("k"));
        let unknown = serde_json::json!({"type": "other", "access": "a"});
        assert_eq!(credential_api_key(Some(&unknown)), None);
        assert_eq!(credential_base_url(Some(&oauth)), None);
    }

    #[test]
    fn the_connection_prefers_env_then_the_stored_credential_then_config() {
        let stored = StoredConnection {
            api_key: "stored-key".into(),
            base_url: "http://stored:8317".into(),
        };
        let connection = connection_from(
            Some(" env-key "),
            Some("http://env:1111/v1"),
            Some(&stored),
            "http://config:8317",
        );
        assert_eq!(connection.api_key, "env-key");
        assert_eq!(connection.base_url, "http://env:1111");

        let connection = connection_from(None, Some("  "), Some(&stored), "http://config:8317");
        assert_eq!(connection.api_key, "stored-key");
        assert_eq!(connection.base_url, "http://stored:8317");

        let connection = connection_from(None, None, None, "http://config:8317/v1");
        assert_eq!(connection.api_key, "");
        assert_eq!(connection.base_url, "http://config:8317");
    }

    #[test]
    fn the_login_flow_validates_the_root_and_the_key() {
        assert_eq!(
            parse_login_base_url("http://host:8317/v1").expect("root"),
            "http://host:8317"
        );
        assert!(parse_login_base_url("host:8317").is_err());
        assert!(parse_login_base_url("ftp://host:8317").is_err());
        assert!(parse_login_base_url("http://host").is_err());

        let credential = login_credential("http://host:8317", " senpi-key ").expect("credential");
        assert_eq!(credential.access, "senpi-key");
        assert_eq!(credential.refresh, "senpi-key");
        assert_eq!(
            credential.get_extra_str("baseUrl"),
            Some("http://host:8317")
        );
        assert!(credential.expires > now_ms() as f64);
        assert_eq!(
            login_credential("http://host:8317", "   ")
                .err()
                .map(|error| error.message),
            Some("빈 키".to_owned())
        );
    }

    #[test]
    fn the_provider_config_declares_both_names_and_their_roles() {
        let primary = provider_config(Tier::Primary, Vec::new(), "", "http://host:8317", None);
        assert_eq!(primary.name.as_deref(), Some("CLI Proxy API (CPA)"));
        assert_eq!(primary.api.as_deref(), Some("openai-responses"));
        assert_eq!(primary.auth_header, Some(true));
        assert!(primary.api_key.is_none());
        assert!(primary.oauth.is_some());
        assert!(primary.refresh_models.is_some());
        assert!(primary.fallback_eligible.is_none());

        let last = provider_config(Tier::Last, Vec::new(), "key", "http://host:8317", None);
        assert_eq!(
            last.name.as_deref(),
            Some("CLI Proxy API (CPA Last Resort)")
        );
        assert_eq!(last.api_key.as_deref(), Some("key"));
        assert!(last.oauth.is_none());
        assert!(
            last.fallback_eligible
                .as_ref()
                .is_some_and(|eligible| !eligible())
        );
    }
}
