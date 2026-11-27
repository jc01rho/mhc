//! Port of `omo-senpi/src/extension/component-list.ts` at pin `77f3067f1`: the ordered component
//! registration list. Names and order are upstream's.
//!
//! Upstream `createOmoSenpiComponents(taskComponent)` always supplies both the `task` component
//! (its argument) and `memory` (`createMemoryComponent()`); the list has no optional entry. The
//! native `task` and `memory` factories are owned by todos 44 and 43/45, so this crate cannot build
//! them locally - but it must not silently drop them either, because `compose.ts` also registers a
//! disabled flag per list entry. [`omo_components`] therefore requires both, and
//! [`try_omo_components`] reports exactly which are missing instead of producing a short list.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::compose::OmoSenpiComponent;

/// Upstream `getBuiltinSkillsRoot()`: the packaged `skills/` directory beside the extension.
pub const SKILLS_ROOT_ENV: &str = "OMO_SENPI_SKILLS_ROOT";

/// Upstream `getBuiltinSkillsRoot(env)` (omo v5.1.52 telemetry/product-identity.ts): a valid
/// `OMO_BUNDLED_SKILLS_DIR` wins (an invalid one is reported once through the shared ledger), then
/// the native `OMO_SENPI_SKILLS_ROOT` / packaged-root default.
pub fn builtin_skills_root() -> PathBuf {
    skills_root_with_override(
        std::env::var(maho_omo_bundled_skills::BUNDLED_SKILLS_OVERRIDE_ENV).ok(),
        std::env::var(SKILLS_ROOT_ENV).ok(),
    )
}

fn skills_root_with_override(override_dir: Option<String>, env_root: Option<String>) -> PathBuf {
    let env: BTreeMap<String, String> = override_dir
        .map(|value| {
            (
                maho_omo_bundled_skills::BUNDLED_SKILLS_OVERRIDE_ENV.to_owned(),
                value,
            )
        })
        .into_iter()
        .collect();
    maho_omo_bundled_skills::resolve_bundled_skills_override(&env, None, None)
        .unwrap_or_else(|| skills_root_from(env_root))
}

/// The `OMO_SENPI_SKILLS_ROOT` contract: a set, non-empty value wins; otherwise the packaged
/// `skills/` directory beside the executable (or a relative `skills` when the path is unknown).
fn skills_root_from(env_root: Option<String>) -> PathBuf {
    if let Some(root) = env_root
        && !root.is_empty()
    {
        return PathBuf::from(root);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("skills")))
        .unwrap_or_else(|| PathBuf::from("skills"))
}

#[derive(Clone)]
pub struct OmoComponentOptions {
    pub skills_root: PathBuf,
    pub state_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub facts_read_only_bootstrap: bool,
    /// The engine side of the thread component: the wire to this device's session hosts and the session's
    /// `pi.session`. The default binds neither, so the session-control tools answer `host_unavailable`.
    pub thread_host: maho_omo_gateway::thread::native::NativeThreadHost,
}

impl Default for OmoComponentOptions {
    fn default() -> Self {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let telemetry_env: std::collections::HashMap<String, String> = env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Self {
            skills_root: builtin_skills_root(),
            state_dir: maho_omo_telemetry::product_identity::get_omo_native_state_dir(
                &telemetry_env,
            ),
            env,
            facts_read_only_bootstrap: false,
            thread_host: Default::default(),
        }
    }
}

impl OmoComponentOptions {
    pub fn with_skills_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.skills_root = root.into();
        self
    }

    pub fn with_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.state_dir = dir.into();
        self
    }

    fn skills_root_string(&self) -> String {
        let mut root = self.skills_root.to_string_lossy().into_owned();
        if !root.ends_with(std::path::MAIN_SEPARATOR) {
            root.push(std::path::MAIN_SEPARATOR);
        }
        root
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingComponents {
    pub names: Vec<&'static str>,
}

impl std::fmt::Display for MissingComponents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the omo component list requires {}",
            self.names.join(", ")
        )
    }
}

impl std::error::Error for MissingComponents {}

/// Wraps lane-43's `MemoryExtension` as a re-registration-safe list entry.
///
/// `MemoryExtension::new` consumes its `MemoryExtensionOptions`, so a shared instance registers once
/// and no-ops afterwards; `ExtensionRunner::recreate` re-registers the same extension objects, so a
/// single shared instance would leave memory unregistered after any reload. `build` is called once
/// per register and must return fresh options over the caller's ONE shared `MemoryComponent` /
/// `MemoryWiring` (never a second store).
pub fn memory_component_from(
    build: impl Fn() -> maho_omo_memory::composition::MemoryExtensionOptions + Send + Sync + 'static,
) -> OmoSenpiComponent {
    OmoSenpiComponent::from_factory("memory", move || {
        Box::new(maho_omo_memory::composition::MemoryExtension::new(build()))
    })
}

fn component(
    name: &'static str,
    extension: impl maho_ext_api::Extension + 'static,
) -> OmoSenpiComponent {
    OmoSenpiComponent::new(name, Box::new(extension))
}

type KeywordDetectorCache = BTreeMap<PathBuf, Option<omo_config_core::OmoKeywordDetectorSettings>>;

/// Upstream `isUltraworkKeywordDisabled` (`components/ultrawork/keyword-detector-settings.ts`,
/// `#14389`): `disabled_keywords` is a denylist and `enabled_expansions` an allowlist, so either
/// turns the `ultrawork` keyword off for a project or a user without the per-launch flag.
pub fn is_ultrawork_keyword_disabled(
    settings: Option<&omo_config_core::OmoKeywordDetectorSettings>,
) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    if settings
        .disabled_keywords
        .as_ref()
        .is_some_and(|keywords| keywords.contains(&omo_config_core::KeywordType::Ultrawork))
    {
        return true;
    }
    settings
        .enabled_expansions
        .as_ref()
        .is_some_and(|expansions| !expansions.contains(&omo_config_core::KeywordType::Ultrawork))
}

/// Upstream `loadKeywordDetectorSettings`: `loadSenpiOmoConfig({ cwd }).config.keyword_detector`.
pub fn load_keyword_detector_settings(
    cwd: &std::path::Path,
    env: Option<&BTreeMap<String, String>>,
) -> Option<omo_config_core::OmoKeywordDetectorSettings> {
    let loaded = omo_config_core::load_omo_config(&omo_config_core::LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: env.cloned(),
        harness: Some("senpi".to_owned()),
        ..Default::default()
    });
    let value = loaded.config.get("keyword_detector")?;
    serde_json::from_value(value.clone()).ok()
}

/// Upstream `SENPI_GPT_ULTRAWORK_DIRECTIVE` (`skills/ultrawork/references/gpt.md` at v5.1.83, #14441).
pub const GPT_ULTRAWORK_CONTRACT: &str = include_str!("ultrawork_gpt.md");

/// Upstream `ultraworkVariantFor` (`components/ultrawork/directive-variants.ts`, #14441/#14442): GPT
/// models, and DeepSeek Flash whose core reacts to conflicting instructions the same way, get the GPT
/// prompt; every other model keeps the core contract.
pub fn ultrawork_contract_for_model(model_id: &str) -> Option<&'static str> {
    let name = model_id
        .rsplit('/')
        .next()
        .unwrap_or(model_id)
        .to_lowercase();
    (name.contains("gpt") || (name.contains("deepseek") && name.contains("flash")))
        .then_some(GPT_ULTRAWORK_CONTRACT)
}

/// Builds an [`maho_ultrawork::UltraworkExtension`] that caches one `keyword_detector` lookup per
/// `cwd` across the component's registration lifetime.
pub fn ultrawork_extension_for(
    env: BTreeMap<String, String>,
) -> maho_ultrawork::UltraworkExtension {
    let cache: std::sync::Arc<std::sync::Mutex<KeywordDetectorCache>> = std::sync::Arc::default();
    maho_ultrawork::UltraworkExtension::default()
        .with_keyword_disabled(move |cwd| {
            let mut guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let settings = guard
                .entry(cwd.to_path_buf())
                .or_insert_with(|| load_keyword_detector_settings(cwd, Some(&env)));
            is_ultrawork_keyword_disabled(settings.as_ref())
        })
        .with_model_contract(ultrawork_contract_for_model)
}

/// Upstream `createOmoSenpiComponents(taskComponent)` with the selected latest additions.
pub fn omo_components(
    task: OmoSenpiComponent,
    memory: OmoSenpiComponent,
    options: &OmoComponentOptions,
) -> Vec<OmoSenpiComponent> {
    let skills_root = options.skills_root.clone();
    let mut components = Vec::with_capacity(24);
    let config_startup = if options.facts_read_only_bootstrap {
        maho_omo_config_startup::ConfigStartupComponent {
            run_migration: Some(std::sync::Arc::new(|_| Default::default())),
            load_config: None,
        }
    } else {
        maho_omo_config_startup::ConfigStartupComponent::default()
    };
    components.push(component("config-startup", config_startup));
    components.push(component(
        "model-profile",
        maho_omo_model_profile::ModelProfileComponent::new(
            maho_omo_model_profile::ModelProfileComponentOptions {
                load_config: None,
                env: Some(
                    options
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
            },
        ),
    ));
    components.push(component(
        "bundled-skills",
        maho_omo_bundled_skills::BundledSkillsComponent::new(
            maho_omo_bundled_skills::BundledSkillsComponentOptions {
                env: Some(options.env.clone()),
                skills_dir: Some(skills_root.clone()),
            },
        ),
    ));
    let skill_commands_env = options.env.clone();
    let skill_commands_root = skills_root.clone();
    components.push(OmoSenpiComponent::from_context_register(
        "skill-commands",
        move |api, runtime| {
            use maho_ext_api::Extension;
            let component = maho_omo_skill_commands::SkillCommandsComponent::new(
                maho_omo_skill_commands::SkillCommandsComponentOptions {
                    skills_dir: Some(skill_commands_root.clone()),
                    env: Some(skill_commands_env.clone()),
                    logger: Some(runtime.logger()),
                },
            );
            component.register(api);
            if let Err(error) = crate::claude_compat::register(api, &skill_commands_env) {
                maho_ext_api::diagnostic(&format!("Claude compatibility loading failed: {error}"));
            }
        },
    ));
    components.push(component(
        "onboarding",
        maho_omo_onboarding::OnboardingComponent {
            state_dir: options.state_dir.clone(),
            skills_root: options.skills_root_string(),
        }
        .with_dependencies(maho_omo_onboarding::component::OnboardingDependencies {
            applier: Some(std::sync::Arc::new(
                crate::onboarding_memory_applier::PublishedMemoryFactsApplier,
            )),
            ..Default::default()
        }),
    ));
    components.push(component(
        "init-deep-advisor",
        maho_omo_init_deep_advisor::index::InitDeepAdvisorComponent::new(skills_root.clone()),
    ));
    components.push(component(
        "onboarding-scan",
        maho_omo_onboarding::scan_component::OnboardingScanComponent::default(),
    ));
    components.push(component(
        "side-panel",
        maho_omo_side_panel::native::NativeSidePanel,
    ));
    // Upstream registers telemetry with `ctx.logger`, so an invalid skills override reaches the component
    // log; bind the shared runtime logger at register time (same pattern as config-watch below).
    let telemetry = std::sync::Arc::new(std::sync::Mutex::new(
        maho_omo_telemetry::omo_native_component::OmoNativeTelemetryComponent::with_default_config(
            maho_omo_telemetry::index::SenpiTelemetryOptions {
                env: Some(
                    options
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
                state_dir: Some(options.state_dir.clone()),
                ..Default::default()
            },
            skills_root.clone(),
        ),
    ));
    components.push(OmoSenpiComponent::from_context_register(
        "telemetry",
        move |api, runtime| {
            use maho_ext_api::Extension;
            let mut telemetry = telemetry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            telemetry.logger = Some(runtime.logger());
            telemetry.register(api);
        },
    ));
    components.push(component(
        "ultrawork",
        ultrawork_extension_for(options.env.clone()),
    ));
    components.push(component(
        "skill-pointers",
        maho_omo_skill_pointers::SkillPointersComponent::new(options.skills_root_string()),
    ));
    components.push(component(
        "ulw-execute-continuation",
        maho_omo_start_work_continuation::StartWorkContinuationComponent::default(),
    ));
    // The loop's registration-time log has no context; bind the shared runtime logger at
    // construction (same pattern as config-watch below) so the production path logs too.
    components.push(OmoSenpiComponent::from_context_register(
        "ulw-loop",
        move |api, runtime| {
            use maho_ext_api::Extension;
            let component = maho_omo_ulw_loop::index::UlwLoopComponent {
                logger: Some(runtime.logger()),
                ..Default::default()
            };
            component.register(api);
        },
    ));
    components.push(component(
        maho_omo_js_kernel_guard::JS_KERNEL_GUARD_COMPONENT_NAME,
        maho_omo_js_kernel_guard::JsKernelGuardComponent,
    ));
    components.push(component(
        "question-gates",
        maho_omo_question_gates::create_question_gates_component(),
    ));
    components.push(component(
        "claim-receipt",
        maho_omo_claim_receipt::ClaimReceiptComponent::new(options.env.clone()),
    ));
    components.push(component(
        "git-master",
        maho_omo_git_master::GitMasterAttributionComponent::new(
            maho_omo_git_master::GitMasterAttributionComponentOptions {
                load_settings: None,
                env: Some(
                    options
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
            },
        ),
    ));
    components.push(component(
        "fallback-architect",
        maho_omo_fallback_architect::FallbackArchitectComponent::default(),
    ));
    components.push(component(
        "comment-checker",
        maho_omo_comment_checker::CommentCheckerComponent::default(),
    ));
    components.push(component(
        "ast-grep",
        maho_omo_ast_grep::AstGrepComponent::default(),
    ));
    components.push(component(
        "builtin-mcps",
        maho_omo_builtin_mcps::BuiltinMcpsComponent::from_env(&options.env),
    ));
    let x_search_env = options.env.clone();
    components.push(OmoSenpiComponent::from_context_register(
        "x-search",
        move |api, runtime| {
            use maho_ext_api::Extension;
            maho_omo_x_search::XSearchComponent::new(maho_omo_x_search::XSearchComponentOptions {
                env: Some(x_search_env.clone()),
                logger: Some(runtime.logger()),
                ..Default::default()
            })
            .register(api);
        },
    ));
    components.push(component(
        "computer-use",
        maho_omo_computer_use::ComputerUseComponent::new(
            maho_omo_computer_use::ComputerUseComponentOptions {
                env: Some(options.env.clone()),
                ..Default::default()
            },
        ),
    ));
    let feedback_env = options.env.clone();
    let feedback_state_dir = options.state_dir.clone();
    components.push(OmoSenpiComponent::from_context_register(
        "feedback",
        move |api, runtime| {
            use maho_ext_api::Extension;
            maho_omo_feedback::FeedbackComponent::new(
                maho_omo_feedback::FeedbackComponentOptions {
                    env: Some(feedback_env.clone()),
                    state_dir: Some(feedback_state_dir.clone()),
                    omo_version: Some(env!("CARGO_PKG_VERSION").into()),
                    logger: Some(runtime.logger()),
                    ..Default::default()
                },
            )
            .register(api);
        },
    ));
    components.push(component(
        "browser-bridge",
        maho_omo_browser_bridge::BrowserBridgeComponent::default(),
    ));
    components.push(component(
        "desktop-toolkit",
        maho_omo_desktop_toolkit::DesktopToolkitComponent::default(),
    ));
    components.push(component(
        "claude-code",
        maho_omo_claude_code::ClaudeCodeComponent,
    ));
    components.push(OmoSenpiComponent::from_context_register(
        "lsp",
        |api, runtime| {
            maho_omo_lsp::LspComponent.register_with_startup_deferral(
                api,
                maho_omo_lsp::index::LspComponentOptions::default(),
                Some(runtime.defer_macrotask()),
            );
        },
    ));
    components.push(task);
    components.push(component(
        "thread",
        maho_omo_gateway::thread::native::NativeThreadComponent {
            host: options.thread_host.clone(),
        },
    ));
    components.push(memory);
    components.push(component(
        "gateway",
        maho_omo_gateway::GatewayComponent::new(Default::default()),
    ));
    let watch = std::sync::Arc::new(std::sync::Mutex::new(
        maho_omo_config_watch::index::ConfigWatchComponent::default(),
    ));
    components.push(OmoSenpiComponent::from_context_register(
        "config-watch",
        move |api, runtime| {
            use maho_ext_api::Extension;
            let mut watch = watch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            watch.options.logger = Some(runtime.logger());
            watch.register(api);
        },
    ));
    components.push(component(
        "remote",
        maho_omo_remote::RemoteCommandComponent {
            version: env!("CARGO_PKG_VERSION").into(),
            state_dir: None,
        },
    ));
    components
}

pub fn try_omo_components(
    task: Option<OmoSenpiComponent>,
    memory: Option<OmoSenpiComponent>,
    options: &OmoComponentOptions,
) -> Result<Vec<OmoSenpiComponent>, MissingComponents> {
    let mut missing = Vec::new();
    if task.is_none() {
        missing.push("task");
    }
    if memory.is_none() {
        missing.push("memory");
    }
    if !missing.is_empty() {
        return Err(MissingComponents { names: missing });
    }
    let (Some(task), Some(memory)) = (task, memory) else {
        return Err(MissingComponents { names: missing });
    };
    Ok(omo_components(task, memory, options))
}

pub fn omo_component_names() -> Vec<&'static str> {
    [
        "config-startup",
        "model-profile",
        "bundled-skills",
        "skill-commands",
        "onboarding",
        "init-deep-advisor",
        "onboarding-scan",
        "side-panel",
        "telemetry",
        "ultrawork",
        "skill-pointers",
        "ulw-execute-continuation",
        "ulw-loop",
        "js-kernel-guard",
        "question-gates",
        "claim-receipt",
        "git-master",
        "fallback-architect",
        "comment-checker",
        "ast-grep",
        "builtin-mcps",
        "x-search",
        "computer-use",
        "feedback",
        "browser-bridge",
        "desktop-toolkit",
        "claude-code",
        "lsp",
        "task",
        "thread",
        "memory",
        "gateway",
        "config-watch",
        "remote",
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_non_empty_env_root_overrides_the_packaged_skills_dir() {
        assert_eq!(
            skills_root_from(Some("/custom/omo/skills".to_owned())),
            PathBuf::from("/custom/omo/skills")
        );
    }

    #[test]
    fn a_valid_bundled_skills_override_wins_over_the_env_root() {
        let dir = tempfile::tempdir().expect("override dir");
        let override_dir = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            skills_root_with_override(Some(override_dir), Some("/custom/omo/skills".to_owned())),
            dir.path()
        );
        assert_eq!(
            skills_root_with_override(
                Some("relative/skills".to_owned()),
                Some("/custom/omo/skills".to_owned())
            ),
            PathBuf::from("/custom/omo/skills"),
            "an invalid override falls back to the env root"
        );
    }

    #[test]
    fn an_unset_or_empty_env_root_falls_back_to_the_packaged_skills_dir() {
        let fallback = skills_root_from(None);
        assert!(
            fallback.ends_with("skills"),
            "packaged fallback stays under a skills dir: {fallback:?}"
        );
        assert_eq!(
            skills_root_from(Some(String::new())),
            fallback,
            "an empty override is treated as unset"
        );
    }

    #[test]
    fn ultrawork_honors_keyword_detector_from_omo_config() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("create home");
        let env: BTreeMap<String, String> =
            [("HOME".to_owned(), home.to_string_lossy().into_owned())]
                .into_iter()
                .collect();

        let write_project = |name: &str, config: Option<&str>| -> PathBuf {
            let dir = root.path().join(name);
            let omo_dir = dir.join(".omo");
            std::fs::create_dir_all(&omo_dir).expect("create .omo");
            if let Some(raw) = config {
                std::fs::write(omo_dir.join("omo.jsonc"), raw).expect("write omo.jsonc");
            }
            dir
        };

        let denied = write_project(
            "denied",
            Some(r#"{ "keyword_detector": { "disabled_keywords": ["ultrawork"] } }"#),
        );
        assert!(is_ultrawork_keyword_disabled(
            load_keyword_detector_settings(&denied, Some(&env)).as_ref()
        ));

        let allowlist = write_project(
            "allowlist",
            Some(r#"{ "[native]": { "keyword_detector": { "enabled_expansions": ["team"] } } }"#),
        );
        assert!(is_ultrawork_keyword_disabled(
            load_keyword_detector_settings(&allowlist, Some(&env)).as_ref()
        ));

        for (name, config) in [
            ("kept-none", None),
            (
                "kept-other-denied",
                Some(r#"{ "keyword_detector": { "disabled_keywords": ["team"] } }"#),
            ),
            (
                "kept-ulw-allowed",
                Some(r#"{ "keyword_detector": { "enabled_expansions": ["ultrawork"] } }"#),
            ),
        ] {
            let dir = write_project(name, config);
            assert!(
                !is_ultrawork_keyword_disabled(
                    load_keyword_detector_settings(&dir, Some(&env)).as_ref()
                ),
                "case {name} should keep ultrawork enabled"
            );
        }

        let loaded = omo_config_core::load_omo_config(&omo_config_core::LoadOmoConfigOptions {
            cwd: Some(denied.to_string_lossy().into_owned()),
            env: Some(env),
            harness: Some("senpi".to_owned()),
            ..Default::default()
        });
        assert!(
            loaded
                .diagnostics
                .iter()
                .all(|d| d.kind != omo_config_core::DIAGNOSTIC_UNKNOWN_KEYS)
        );
    }

    #[test]
    fn gpt_family_and_deepseek_flash_get_the_gpt_ultrawork_contract() {
        for model in [
            "gpt-6.2",
            "openai/gpt-5.5-codex",
            "GPT-6-astra-fast",
            "deepseek/deepseek-v4.1-flash",
            "DeepSeek-V4-Flash",
        ] {
            assert_eq!(
                ultrawork_contract_for_model(model),
                Some(GPT_ULTRAWORK_CONTRACT),
                "{model}"
            );
        }
        for model in [
            "claude-opus-4-6",
            "deepseek-v4-pro",
            "gemini-3-flash",
            "openrouter/gpt-pro/kimi-k2",
        ] {
            assert_eq!(ultrawork_contract_for_model(model), None, "{model}");
        }
    }
}
