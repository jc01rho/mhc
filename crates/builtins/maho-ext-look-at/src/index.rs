use maho_ai::{model::Model, types::InputModality};
pub struct LookAtExtension {
    pub runner: crate::runner::VisionModelRunner,
}
impl maho_ext_api::Extension for LookAtExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        use std::sync::{Arc, Mutex};
        let store = Arc::new(Mutex::new(crate::settings::create_look_at_store()));
        let runner = self.runner.clone();
        let execute_store = store.clone();
        let mut definition = maho_ext_api::ToolDefinition::new(
            "look_at",
            crate::prompts::LOOK_AT_DESCRIPTION,
            crate::arguments::parameters(),
            Arc::new(|_| {
                Box::pin(async {
                    Err(maho_ext_api::ToolError::Message(
                        "look_at requires full extension context".into(),
                    ))
                })
            }),
        );
        definition.label = "Look At".into();
        definition.prompt_snippet = Some(crate::prompts::LOOK_AT_PROMPT_SNIPPET.into());
        definition.prepare_arguments = Some(Arc::new(|args| {
            crate::arguments::prepare_look_at_arguments(&args).map_err(Into::into)
        }));
        api.register_tool_with_renderers(definition.clone(), crate::render::renderers())
            .unwrap_or_else(|error| std::panic::panic_any(error));
        let scope = api.runtime.registration_scope();
        let mut executor_api = maho_ext_api::ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            scope.clone(),
        );
        executor_api.register_tool_with_extension_context(definition,Arc::new(move |_,params,signal,_,ctx|{
            let runner=runner.clone();let store=execute_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            Box::pin(async move {
                let args=serde_json::from_value(params).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                let normalized=crate::arguments::normalize_look_at_args(args,None);
                if let Some(error)=crate::arguments::validate_look_at_args(&normalized){return Err(maho_ext_api::ExtensionFailure::new(error));}
                let result=runner(&normalized,ctx,&store,signal).await?;
                Ok(maho_ext_api::AgentToolResult {details:serde_json::json!({"model":result.model,"sources":result.sources,"mimeTypes":result.mime_types}),..maho_ext_api::AgentToolResult::text(result.text)})
            })
        })).unwrap_or_else(|error|std::panic::panic_any(error));
        scope
            .commit_registration()
            .unwrap_or_else(|error| std::panic::panic_any(error));
        register_activation_hooks(api, store.clone());
        register_image_placeholder_hook(api);
        register_tool_result_describer(api, self.runner.clone(), store.clone());
        let runtime = api.runtime.clone();
        let command_store = store.clone();
        crate::commands::register_look_at_command(
            api,
            store,
            Arc::new(move |ctx| {
                sync_tool_activation(
                    &runtime,
                    ctx,
                    &command_store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                )
            }),
        );
    }
}
pub fn register_activation_hooks(
    api: &mut maho_ext_api::ExtensionApi,
    store: std::sync::Arc<std::sync::Mutex<crate::settings::LookAtStore>>,
) {
    let runtime = api.runtime.clone();
    for event in [
        maho_ext_api::EventKind::SessionStart,
        maho_ext_api::EventKind::ModelSelect,
    ] {
        let runtime = runtime.clone();
        let store = store.clone();
        api.on(
            event,
            std::sync::Arc::new(move |_event, ctx| {
                let runtime = runtime.clone();
                let store = store.clone();
                Box::pin(async move {
                    sync_tool_activation(
                        &runtime,
                        ctx,
                        &store
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    )?;
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
    }
}
pub fn register_image_placeholder_hook(api: &mut maho_ext_api::ExtensionApi) {
    let runtime = api.runtime.clone();
    api.on_with_options(
        maho_ext_api::EventKind::Context,
        std::sync::Arc::new(move |event, ctx| {
            let result =
                (|| -> Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure> {
                    if !describes_images_for(&runtime, ctx)? {
                        return Ok(maho_ext_api::EventResult::None);
                    }
                    let (maho_ext_api::ExtensionEvent::Context { messages, .. }, Some(model)) =
                        (event, ctx.model.as_ref())
                    else {
                        return Ok(maho_ext_api::EventResult::None);
                    };
                    let Some(next) = crate::image_placeholder::try_replace_images_naming_tool(
                        messages, model, "look_at",
                    ) else {
                        return Ok(maho_ext_api::EventResult::None);
                    };
                    *messages = next.clone();
                    Ok(maho_ext_api::EventResult::Context {
                        messages: Some(next),
                    })
                })();
            Box::pin(async move { result })
        }),
        maho_ext_api::ExtensionHandlerOptions {
            mutates_messages: Some(false),
            ..Default::default()
        },
    );
}
/// senpi#7260: the image a tool returns to a model without image input is described by the vision
/// model before the result is stored, so the transcript holds the description and no later request
/// describes it again. An image that could not be described stays and gets the transport placeholder.
pub fn register_tool_result_describer(
    api: &mut maho_ext_api::ExtensionApi,
    runner: crate::runner::VisionModelRunner,
    store: std::sync::Arc<std::sync::Mutex<crate::settings::LookAtStore>>,
) {
    let runtime = api.runtime.clone();
    api.on(
        maho_ext_api::EventKind::ToolResult,
        std::sync::Arc::new(move |event, ctx| {
            let (runtime, runner, store) = (runtime.clone(), runner.clone(), store.clone());
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::ToolResult(result) = event else {
                    return Ok(maho_ext_api::EventResult::None);
                };
                if !describes_images_for(&runtime, ctx)? {
                    return Ok(maho_ext_api::EventResult::None);
                }
                let store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let (runner, store) = (&runner, &store);
                let described = crate::tool_result_images::describe_tool_result_images(
                    &result.content,
                    "look_at",
                    |mime_type, data| {
                        let args = crate::arguments::normalize_look_at_args(
                            crate::arguments::LookAtArgs {
                                image_data_list: Some(vec![format!(
                                    "data:{mime_type};base64,{data}"
                                )]),
                                goal: crate::prompts::TOOL_IMAGE_DESCRIPTION_GOAL.into(),
                                ..Default::default()
                            },
                            None,
                        );
                        async move {
                            if let Some(update) = &ctx.update_tool_hook_status {
                                update("look_at: describing image");
                            }
                            // The host drops this handler with the turn (tool_hook_wait), which ends the request.
                            runner(&args, ctx, store, None)
                                .await
                                .map(|run| crate::tool_result_images::ImageDescription {
                                    model: run.model,
                                    text: run.text,
                                })
                                .map_err(|error| error.message)
                        }
                    },
                )
                .await;
                if !described.failures.is_empty()
                    && !ctx
                        .signal
                        .as_ref()
                        .is_some_and(|signal| signal.is_aborted())
                {
                    let mut reasons: Vec<&str> = Vec::new();
                    for reason in &described.failures {
                        if !reasons.contains(&reason.as_str()) {
                            reasons.push(reason);
                        }
                    }
                    ctx.ui.notify(
                        &format!(
                            "look_at could not describe an image from {}: {}",
                            result.tool_name,
                            reasons.join("; ")
                        ),
                        maho_ext_api::NotificationType::Warning,
                    );
                }
                Ok(match described.content {
                    Some(content) => {
                        maho_ext_api::EventResult::ToolResult(maho_ext_api::ToolResultEventResult {
                            content: Some(content),
                            ..Default::default()
                        })
                    }
                    None => maho_ext_api::EventResult::None,
                })
            })
        }),
    );
}
pub fn offers_look_at(
    runtime: &maho_ext_api::ExtensionRuntime,
    ctx: &maho_ext_api::ExtensionContext,
) -> Result<bool, maho_ext_api::ExtensionFailure> {
    if ctx
        .model
        .as_ref()
        .is_none_or(|model| model.input.contains(&InputModality::Image))
    {
        return Ok(false);
    }
    Ok(runtime
        .session_actions()?
        .get_active_tools()?
        .iter()
        .any(|name| name == "look_at"))
}
/// look_at is offered only while the model has no image input and a vision model is available; with
/// images blocked the transport replaces them itself and look_at refuses every input.
fn describes_images_for(
    runtime: &maho_ext_api::ExtensionRuntime,
    ctx: &maho_ext_api::ExtensionContext,
) -> Result<bool, maho_ext_api::ExtensionFailure> {
    if !offers_look_at(runtime, ctx)? {
        return Ok(false);
    }
    Ok(!ctx.get_image_settings()?.block_images)
}
pub fn sync_tool_activation(
    runtime: &maho_ext_api::ExtensionRuntime,
    ctx: &maho_ext_api::ExtensionContext,
    store: &crate::settings::LookAtStore,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let actions = runtime.session_actions()?;
    let active = actions.get_active_tools()?;
    let enabled = match store.get_override().enabled {
        Some(enabled) => enabled,
        None => ctx.get_look_at_settings()?.enabled,
    };
    let vision_available = if enabled
        && ctx
            .model
            .as_ref()
            .is_some_and(|model| !model.input.contains(&InputModality::Image))
    {
        let chain = match &store.get_override().models {
            Some(models) => models.clone(),
            None => ctx.get_look_at_settings()?.models.unwrap_or_else(|| {
                crate::model_selector::DEFAULT_LOOK_AT_CHAIN
                    .map(String::from)
                    .to_vec()
            }),
        };
        crate::model_selector::resolve_vision_model(&chain, &ctx.model_registry.get_available())
            .is_some()
    } else {
        false
    };
    if let Some(next) = tool_activation(enabled, ctx.model.as_ref(), vision_available, &active) {
        actions.set_active_tools(next)?;
    }
    Ok(())
}
pub fn tool_activation(
    enabled: bool,
    model: Option<&Model>,
    vision_model_available: bool,
    active: &[String],
) -> Option<Vec<String>> {
    let should_be_active = enabled
        && model.is_some_and(|model| !model.input.contains(&InputModality::Image))
        && vision_model_available;
    let is_active = active.iter().any(|name| name == "look_at");
    if should_be_active && !is_active {
        let mut next = active.to_vec();
        next.push("look_at".into());
        Some(next)
    } else if !should_be_active && is_active {
        Some(
            active
                .iter()
                .filter(|name| name.as_str() != "look_at")
                .cloned()
                .collect(),
        )
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_activation_hooks_register_start_and_model_select() {
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "look-at",
                std::path::PathBuf::new(),
                Default::default(),
            ),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        register_activation_hooks(
            &mut api,
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::settings::create_look_at_store(),
            )),
        );
        assert_eq!(api.registered.handlers.len(), 2);
        assert_eq!(
            api.registered.handlers[&maho_ext_api::EventKind::SessionStart].len(),
            1
        );
        assert_eq!(
            api.registered.handlers[&maho_ext_api::EventKind::ModelSelect].len(),
            1
        );
    }
    #[test]
    fn absent_model_and_disabled_setting_remove_tool() {
        let active = ["read".into(), "look_at".into()];
        assert_eq!(
            tool_activation(true, None, true, &active),
            Some(vec!["read".into()])
        );
        assert_eq!(
            tool_activation(false, None, true, &active),
            Some(vec!["read".into()])
        );
        assert_eq!(tool_activation(true, None, true, &["read".into()]), None);
    }
}
