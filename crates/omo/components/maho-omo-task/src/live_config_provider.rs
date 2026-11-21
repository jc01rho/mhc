//! Port of `components/task/live-config-provider.ts`: the task planner's per-plan omo config.
//!
//! - Caches reads within `cache_ttl_ms`, so one plan does not re-read and re-validate the files.
//! - Rejects a reload only for an error-severity diagnostic the adopted config did not already
//!   carry: the session already runs on an error accepted at startup (or an earlier adoption), so it
//!   never blocks a later edit.
//! - On a rejected or failed reload, keeps the last adopted config and warns once per distinct cause.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::planner::ResolveOmoConfig;

pub const DEFAULT_LIVE_CONFIG_CACHE_TTL_MS: u64 = 250;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveConfigDiagnostic {
    pub kind: String,
    pub path: String,
    pub message: String,
}

/// One load of the merged config: the view, every diagnostic, and the path of each layer read.
#[derive(Debug, Clone, Default)]
pub struct LiveConfigLoad {
    pub config: Value,
    pub diagnostics: Vec<LiveConfigDiagnostic>,
    pub source_paths: Vec<String>,
}

pub type LoadLiveConfig = Arc<dyn Fn() -> Result<LiveConfigLoad, String> + Send + Sync>;
pub type LiveConfigWarn = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub type LiveConfigClock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct LiveConfigProviderOptions {
    pub load_config: LoadLiveConfig,
    pub initial: LiveConfigLoad,
    /// The path a validation diagnostic of the merged config names (`MERGED_OMO_CONFIG_PATH`).
    pub merged_config_path: String,
    pub warn: LiveConfigWarn,
    pub cache_ttl_ms: u64,
    pub now: LiveConfigClock,
}

pub fn system_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

// Error severity: the loader could not apply what the file says (a layer that failed to read or
// parse, or a file or the merged config rejected whole by validation), or a model catalog reference
// cycles. Adopting such a reload could silently swap the user's categories for defaults. Every other
// kind still loads and applies: an ignored unknown key, a pruned invalid value, a deprecated alias,
// a missing profile, and the model-reference `validation` notices, which name a config key rather
// than a file the loader read and drop nothing.
fn is_error_diagnostic(
    diagnostic: &LiveConfigDiagnostic,
    load: &LiveConfigLoad,
    merged_config_path: &str,
) -> bool {
    match diagnostic.kind.as_str() {
        "parse" | "read" | "model_catalog_cycle" => true,
        "validation" => {
            diagnostic.path == merged_config_path || load.source_paths.contains(&diagnostic.path)
        }
        _ => false,
    }
}

fn fingerprint(diagnostic: &LiveConfigDiagnostic) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        diagnostic.kind, diagnostic.path, diagnostic.message
    )
}

fn error_fingerprints(load: &LiveConfigLoad, merged_config_path: &str) -> BTreeSet<String> {
    load.diagnostics
        .iter()
        .filter(|diagnostic| is_error_diagnostic(diagnostic, load, merged_config_path))
        .map(fingerprint)
        .collect()
}

struct LiveConfigState {
    last_good: Value,
    adopted_errors: BTreeSet<String>,
    cached: Value,
    cached_at: u64,
    last_warning: Option<String>,
}

impl LiveConfigState {
    fn warning_once(
        &mut self,
        key: String,
        message: &'static str,
        meta: Value,
    ) -> Option<(&'static str, Value)> {
        if self.last_warning.as_deref() == Some(key.as_str()) {
            return None;
        }
        self.last_warning = Some(key);
        Some((message, meta))
    }
}

pub fn create_live_config_provider(options: LiveConfigProviderOptions) -> ResolveOmoConfig {
    let LiveConfigProviderOptions {
        load_config,
        initial,
        merged_config_path,
        warn,
        cache_ttl_ms,
        now,
    } = options;
    let state = Mutex::new(LiveConfigState {
        adopted_errors: error_fingerprints(&initial, &merged_config_path),
        last_good: initial.config.clone(),
        cached: initial.config,
        cached_at: now(),
        last_warning: None,
    });
    Arc::new(move || {
        let (config, warning) = {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            let current = now();
            if current.saturating_sub(state.cached_at) < cache_ttl_ms {
                return state.cached.clone();
            }
            state.cached_at = current;
            let warning = match load_config() {
                Ok(reloaded) => {
                    let introduced: Vec<&LiveConfigDiagnostic> = reloaded
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| {
                            is_error_diagnostic(diagnostic, &reloaded, &merged_config_path)
                                && !state.adopted_errors.contains(&fingerprint(diagnostic))
                        })
                        .collect();
                    if introduced.is_empty() {
                        state.adopted_errors = error_fingerprints(&reloaded, &merged_config_path);
                        state.last_good = reloaded.config.clone();
                        state.cached = reloaded.config;
                        state.last_warning = None;
                        None
                    } else {
                        let mut causes: Vec<String> = introduced
                            .iter()
                            .map(|diagnostic| fingerprint(diagnostic))
                            .collect();
                        causes.sort();
                        let meta = json!({ "diagnostics": introduced });
                        state.cached = state.last_good.clone();
                        state.warning_once(
                            format!("errors\u{1}{}", causes.join("\u{1}")),
                            "omo-senpi task: live config reload encountered errors; retaining last good configuration",
                            meta,
                        )
                    }
                }
                Err(error) => {
                    state.cached = state.last_good.clone();
                    let meta = json!({ "error": error });
                    state.warning_once(
                        format!("thrown\u{1}{error}"),
                        "omo-senpi task: live config reload failed; retaining last good configuration",
                        meta,
                    )
                }
            };
            (state.cached.clone(), warning)
        };
        if let Some((message, meta)) = warning {
            warn(message, meta);
        }
        config
    })
}
