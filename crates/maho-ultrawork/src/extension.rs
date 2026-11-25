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

#[derive(Default, Clone)]
pub struct UltraworkExtension {
    engine: Engine,
    keyword_disabled: Option<KeywordDisabledFn>,
}

impl UltraworkExtension {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            keyword_disabled: None,
        }
    }

    pub fn with_keyword_disabled(
        mut self,
        keyword_disabled: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.keyword_disabled = Some(Arc::new(keyword_disabled));
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

fn session_of(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
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
        let api_cwd = api.cwd.clone();
        let session_role = api.profile.session_context.get("role").cloned();
        let on_input: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            let keyword_disabled = keyword_disabled.clone();
            let api_cwd = api_cwd.clone();
            let session_role = session_role
                .clone()
                .filter(|role| !role.is_empty())
                .or_else(|| std::env::var_os("SENPI_TASK_MEMBER").map(|_| "member".into()))
                .or_else(|| {
                    std::env::var("OMO_SENPI_TASK_RPC_CHILD")
                        .ok()
                        .filter(|value| value == "1")
                        .map(|_| "child".into())
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
                let outcome = engine.on_input_with_session_role(
                    InputRequest {
                        session: &session,
                        text: &input.text,
                        source: input.source,
                        streaming: input.streaming_behavior.is_some(),
                    },
                    false,
                    session_role.as_deref(),
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

        let engine = self.engine.clone();
        let on_turn: ExtensionHandler = Arc::new(move |event, ctx| {
            let engine = engine.clone();
            Box::pin(async move {
                let start = match event {
                    ExtensionEvent::BeforeAgentStart(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                Ok(
                    match engine.on_turn_start(&session_of(ctx), start.preview) {
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
