//! Event wiring. No context-transform handler is registered: the engine never touches history.

use crate::engine::{Engine, InputRequest, TodoResultView};
use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext,
    ExtensionEvent, ExtensionHandler, ExtensionHandlerOptions, FlagType, FlagValue,
    InputEventResult, SessionCompactEvent, ToolContent, ToolResultEventResult,
};
use std::path::Path;
use std::sync::Arc;

/// Runtime flag that turns the whole engine off.
pub const DISABLE_FLAG: &str = "ultrawork-disabled";

/// Predicate consulted when an input carries a visible ultrawork keyword: returns `true` when the
/// effective `keyword_detector` settings turn `ultrawork` off.
pub type KeywordDisabledFn = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// Picks the contract text for the session's model id; `None` keeps the core contract.
pub type ModelContractFn = Arc<dyn Fn(&str) -> Option<&'static str> + Send + Sync>;

#[derive(Default, Clone)]
pub struct UltraworkExtension {
    engine: Engine,
    keyword_disabled: Option<KeywordDisabledFn>,
    model_contract: Option<ModelContractFn>,
    legacy_child: Option<bool>,
}

impl UltraworkExtension {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            keyword_disabled: None,
            model_contract: None,
            legacy_child: None,
        }
    }

    pub fn with_keyword_disabled(
        mut self,
        keyword_disabled: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.keyword_disabled = Some(Arc::new(keyword_disabled));
        self
    }

    pub fn with_model_contract(
        mut self,
        model_contract: impl Fn(&str) -> Option<&'static str> + Send + Sync + 'static,
    ) -> Self {
        self.model_contract = Some(Arc::new(model_contract));
        self
    }

    /// Loads activation policy from the registration directory and takes legacy child detection
    /// from the same host environment as the loader.
    #[must_use]
    pub fn with_keyword_policy(
        mut self,
        load: impl Fn(&Path) -> bool + Send + Sync + 'static,
        legacy_child: bool,
    ) -> Self {
        self.keyword_disabled = Some(Arc::new(move |cwd| !load(cwd)));
        self.legacy_child = Some(legacy_child);
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

fn session_of(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
}

/// Selected at delivery, so a model switch before the turn starts gets its own contract.
fn contract_for(model_contract: Option<&ModelContractFn>, ctx: &ExtensionContext) -> &'static str {
    model_contract
        .zip(ctx.model.as_ref())
        .and_then(|(select, model)| select(&model.id))
        .unwrap_or(crate::CORE_CONTRACT)
}

impl Extension for UltraworkExtension {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            DISABLE_FLAG,
            FlagType::Boolean {
                default: Some(false),
            },
            Some("Turn the ultrawork keyword engine off".into()),
        );
        let runtime = api.runtime.clone();
        let disabled = Arc::new(move || {
            matches!(
                runtime.get_flag(DISABLE_FLAG),
                Some(FlagValue::Boolean(true))
            )
        });

        let (engine, off) = (self.engine.clone(), Arc::clone(&disabled));
        let keyword_disabled = self.keyword_disabled.clone();
        let model_contract = self.model_contract.clone();
        let api_cwd = api.cwd.clone();
        let session_role = api.profile.session_context.get("role").cloned();
        let legacy_child = self.legacy_child;
        let on_input: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            let keyword_disabled = keyword_disabled.clone();
            let model_contract = model_contract.clone();
            let api_cwd = api_cwd.clone();
            let session_role = session_role
                .clone()
                .filter(|role| !role.is_empty())
                .or_else(|| match legacy_child {
                    Some(true) => Some("child".into()),
                    Some(false) => None,
                    None => std::env::var_os("SENPI_TASK_MEMBER")
                        .map(|_| "member".into())
                        .or_else(|| {
                            std::env::var("OMO_SENPI_TASK_RPC_CHILD")
                                .ok()
                                .filter(|value| value == "1")
                                .map(|_| "child".into())
                        }),
                });
            Box::pin(async move {
                let input = match event {
                    ExtensionEvent::Input(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                if off() {
                    return Ok(EventResult::Input(InputEventResult::Continue));
                }
                if let Some(check) = &keyword_disabled {
                    let cwd = if !ctx.cwd.as_os_str().is_empty() && ctx.cwd != Path::new("/tmp") {
                        ctx.cwd.as_path()
                    } else if !api_cwd.as_os_str().is_empty() {
                        api_cwd.as_path()
                    } else {
                        ctx.cwd.as_path()
                    };
                    if crate::has_visible_keyword(&input.text) && check(cwd) {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }
                }
                let session = session_of(ctx);
                let outcome = engine.on_input_with_contract(
                    InputRequest {
                        session: &session,
                        text: &input.text,
                        source: input.source,
                        streaming: input.streaming_behavior.is_some(),
                    },
                    false,
                    session_role.as_deref(),
                    contract_for(model_contract.as_ref(), ctx),
                );
                Ok(EventResult::Input(match outcome.transformed_text {
                    Some(text) => InputEventResult::Transform {
                        text,
                        images: input.images.clone(),
                    },
                    None => InputEventResult::Continue,
                }))
            })
        });
        api.on(EventKind::Input, on_input);

        let (engine, model_contract) = (self.engine.clone(), self.model_contract.clone());
        let on_turn: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, model_contract) = (engine.clone(), model_contract.clone());
            Box::pin(async move {
                let start = match event {
                    ExtensionEvent::BeforeAgentStart(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                Ok(
                    match engine.on_turn_start_with_contract(
                        &session_of(ctx),
                        start.preview,
                        contract_for(model_contract.as_ref(), ctx),
                    ) {
                        Some(message) => {
                            EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                                message: Some(message),
                                ..Default::default()
                            })
                        }
                        None => EventResult::None,
                    },
                )
            })
        });
        api.on_with_options(
            EventKind::BeforeAgentStart,
            on_turn,
            ExtensionHandlerOptions {
                preview_safe: true,
                mutates_messages: None,
            },
        );

        let engine = self.engine.clone();
        let on_compact: ExtensionHandler = Arc::new(move |event, ctx| {
            let engine = engine.clone();
            Box::pin(async move {
                if matches!(
                    event,
                    ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. })
                ) {
                    engine.on_compaction(&session_of(ctx));
                }
                Ok(EventResult::None)
            })
        });
        api.on(EventKind::SessionCompact, on_compact);

        let (engine, off) = (self.engine.clone(), disabled);
        let on_result: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            Box::pin(async move {
                let result = match event {
                    ExtensionEvent::ToolResult(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                let session = session_of(ctx);
                let view = TodoResultView {
                    session: &session,
                    tool_name: &result.tool_name,
                    is_error: result.is_error,
                    details: result.details.as_ref(),
                };
                Ok(match engine.on_tool_result(view, off()) {
                    Some(advisory) => {
                        let mut content = result.content.clone();
                        content.push(ToolContent::text(advisory));
                        EventResult::ToolResult(ToolResultEventResult {
                            content: Some(content),
                            ..Default::default()
                        })
                    }
                    None => EventResult::None,
                })
            })
        });
        api.on(EventKind::ToolResult, on_result);
    }
}
/// Applies the keyword lists from a validated `omo.jsonc` view.
pub fn keyword_activation_allowed(config: &serde_json::Value) -> bool {
    let contains_trigger = |values: &[serde_json::Value]| {
        values
            .iter()
            .any(|value| value.as_str() == Some("ultrawork"))
    };
    let settings = &config["keyword_detector"];
    !settings["disabled_keywords"]
        .as_array()
        .is_some_and(|values| contains_trigger(values))
        && settings["enabled_expansions"]
            .as_array()
            .is_none_or(|values| contains_trigger(values))
}
