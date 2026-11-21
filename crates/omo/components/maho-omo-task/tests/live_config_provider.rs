use maho_omo_task::live_config_provider::{
    LiveConfigDiagnostic, LiveConfigLoad, LiveConfigProviderOptions, create_live_config_provider,
};
use maho_omo_task::planner::{ResolveOmoConfig, create_task_child_planner_live};
use senpi_task::host::{HostError, SenpiModelRegistry};
use senpi_task::manager::types::ManagerStartSpec;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

const USER_LAYER: &str = "/home/.omo/omo.jsonc";
const PROJECT_LAYER: &str = "/home/project/.omo/omo.jsonc";
const MERGED: &str = "(merged omo config)";

fn quick(model: &str) -> Value {
    json!({ "categories": { "quick": { "model": model } } })
}

fn load(config: Value, diagnostics: Vec<LiveConfigDiagnostic>) -> LiveConfigLoad {
    LiveConfigLoad {
        config,
        diagnostics,
        source_paths: vec![USER_LAYER.to_string(), PROJECT_LAYER.to_string()],
    }
}

fn diagnostic(kind: &str, path: &str, message: &str) -> LiveConfigDiagnostic {
    LiveConfigDiagnostic {
        kind: kind.to_string(),
        path: path.to_string(),
        message: message.to_string(),
    }
}

struct Fixture {
    clock: Arc<AtomicU64>,
    next: Arc<Mutex<Result<LiveConfigLoad, String>>>,
    loads: Arc<AtomicUsize>,
    warnings: Arc<Mutex<Vec<String>>>,
    provider: ResolveOmoConfig,
}

impl Fixture {
    fn new(initial: LiveConfigLoad, cache_ttl_ms: u64) -> Self {
        let clock = Arc::new(AtomicU64::new(1_000));
        let next = Arc::new(Mutex::new(Ok(initial.clone())));
        let loads = Arc::new(AtomicUsize::new(0));
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let (load_next, load_count, warned, now) =
            (next.clone(), loads.clone(), warnings.clone(), clock.clone());
        let provider = create_live_config_provider(LiveConfigProviderOptions {
            load_config: Arc::new(move || {
                load_count.fetch_add(1, Ordering::SeqCst);
                load_next
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            }),
            initial,
            merged_config_path: MERGED.to_string(),
            warn: Arc::new(move |message: &str, _meta: Value| {
                warned
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(message.to_string());
            }),
            cache_ttl_ms,
            now: Arc::new(move || now.load(Ordering::SeqCst)),
        });
        Self {
            clock,
            next,
            loads,
            warnings,
            provider,
        }
    }

    fn reload_with(&self, next: Result<LiveConfigLoad, String>) -> Value {
        *self.next.lock().unwrap_or_else(PoisonError::into_inner) = next;
        self.clock.fetch_add(20, Ordering::SeqCst);
        (self.provider)()
    }

    fn warning_count(&self) -> usize {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[test]
fn reads_inside_the_cache_window_serve_the_initial_config_without_reloading() {
    let fixture = Fixture::new(load(quick("initial/model-a"), vec![]), 250);
    assert_eq!((fixture.provider)(), quick("initial/model-a"));
    assert_eq!((fixture.provider)(), quick("initial/model-a"));
    assert_eq!(fixture.loads.load(Ordering::SeqCst), 0);
}

#[test]
fn a_clean_reload_past_the_window_adopts_the_edit() {
    let fixture = Fixture::new(load(quick("initial/model-a"), vec![]), 10);
    let reloaded = fixture.reload_with(Ok(load(quick("updated/model-b"), vec![])));
    assert_eq!(reloaded, quick("updated/model-b"));
    assert_eq!(fixture.loads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.warning_count(), 0);
}

#[test]
fn a_reload_that_introduces_a_parse_error_keeps_the_last_good_config_and_warns_once_per_cause() {
    let fixture = Fixture::new(load(quick("good/model"), vec![]), 10);
    let broken = load(
        json!({}),
        vec![diagnostic(
            "parse",
            PROJECT_LAYER,
            "JSONC syntax error: unexpected token",
        )],
    );
    assert_eq!(fixture.reload_with(Ok(broken.clone())), quick("good/model"));
    assert_eq!(fixture.reload_with(Ok(broken)), quick("good/model"));
    assert_eq!(fixture.warning_count(), 1);
    let unreadable = load(
        json!({}),
        vec![diagnostic("read", USER_LAYER, "EACCES: permission denied")],
    );
    assert_eq!(fixture.reload_with(Ok(unreadable)), quick("good/model"));
    assert_eq!(fixture.warning_count(), 2);
}

#[test]
fn warning_level_diagnostics_never_block_an_edit() {
    let fixture = Fixture::new(load(quick("a/one"), vec![]), 10);
    let edited = load(
        quick("b/two"),
        vec![
            diagnostic(
                "unknown-keys",
                USER_LAYER,
                "ignored unknown key(s): not_a_real_key",
            ),
            diagnostic("invalid-value", USER_LAYER, "pruned an invalid value"),
            diagnostic("deprecated-keys", USER_LAYER, "a deprecated alias"),
            diagnostic(
                "validation",
                "model_profiles.capable",
                "a profile shadows a catalog entry",
            ),
        ],
    );
    assert_eq!(fixture.reload_with(Ok(edited)), quick("b/two"));
    assert_eq!(fixture.warning_count(), 0);
}

#[test]
fn validation_of_a_whole_layer_or_of_the_merged_config_is_an_error() {
    for path in [USER_LAYER, MERGED] {
        let fixture = Fixture::new(load(quick("good/model"), vec![]), 10);
        let rejected = load(
            json!({}),
            vec![diagnostic("validation", path, "rejected whole")],
        );
        assert_eq!(
            fixture.reload_with(Ok(rejected)),
            quick("good/model"),
            "{path}"
        );
        assert_eq!(fixture.warning_count(), 1, "{path}");
    }
}

#[test]
fn an_error_already_present_at_startup_never_freezes_a_later_edit() {
    let startup_error = diagnostic("parse", PROJECT_LAYER, "unexpected end of input");
    let fixture = Fixture::new(load(quick("a/one"), vec![startup_error.clone()]), 10);
    let edited = load(quick("b/two"), vec![startup_error]);
    assert_eq!(fixture.reload_with(Ok(edited)), quick("b/two"));
    assert_eq!(fixture.warning_count(), 0);
}

#[test]
fn a_failed_reload_keeps_the_last_good_config_and_warns_again_after_a_recovery() {
    let fixture = Fixture::new(load(quick("good/model"), vec![]), 10);
    let failure = Err("EPERM: disk read collision".to_string());
    assert_eq!(fixture.reload_with(failure.clone()), quick("good/model"));
    assert_eq!(fixture.warning_count(), 1);
    assert_eq!(
        fixture.reload_with(Ok(load(quick("next/model"), vec![]))),
        quick("next/model")
    );
    assert_eq!(fixture.reload_with(failure), quick("next/model"));
    assert_eq!(fixture.warning_count(), 2);
}

struct Registry(Vec<Value>);

impl SenpiModelRegistry for Registry {
    fn get_available(&self) -> Result<Value, HostError> {
        Ok(Value::Array(self.0.clone()))
    }

    fn find(&self, provider: &str, id: &str) -> Option<Value> {
        self.0
            .iter()
            .find(|model| model["provider"] == provider && model["id"] == id)
            .cloned()
    }
}

fn lane(model: &str) -> Value {
    json!({ "categories": { "lane": { "model": model } } })
}

#[test]
fn a_live_planner_resolves_each_spawn_against_the_config_read_for_that_plan() {
    let live = Arc::new(Mutex::new(lane("lab/first")));
    let reader = live.clone();
    let registry = Arc::new(Registry(vec![
        json!({ "provider": "lab", "id": "first" }),
        json!({ "provider": "lab", "id": "second" }),
    ]));
    let planner = create_task_child_planner_live(
        Arc::new(move || {
            reader
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }),
        vec![],
        Arc::new(move || Some(registry.clone())),
        Arc::new(|| None),
        None,
    );
    let spawn = || {
        planner(&ManagerStartSpec {
            prompt: "fixture".into(),
            parent_session_id: "parent".into(),
            category: Some("lane".into()),
            ..Default::default()
        })
        .expect("resolved")
        .model
    };
    assert_eq!(spawn(), "lab/first");
    *live.lock().unwrap_or_else(PoisonError::into_inner) = lane("lab/second");
    assert_eq!(spawn(), "lab/second");
}
