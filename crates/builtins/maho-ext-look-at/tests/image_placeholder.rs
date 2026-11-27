use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, faux_assistant_message, faux_tool_call,
};
use maho_ai::types::{
    AssistantMessage, ContentBlock, ImageContent, InputModality, Message, StopReason, UserContent,
};
use maho_core::agent_session::PromptOptions;
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use maho_test_support::{
    faux::FauxScript,
    faux_session::{FauxSession, NativeSession},
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl3T2QAAAAASUVORK5CYII=";

pub(crate) struct GalleryLook {
    pub(crate) runner: maho_ext_look_at::runner::VisionModelRunner,
    pub(crate) image_count: usize,
}

impl Extension for GalleryLook {
    fn register(&self, api: &mut ExtensionApi) {
        let runner = self.runner.clone();
        maho_ext_look_at::index::LookAtExtension { runner }.register(api);
        let count = self.image_count;
        api.register_tool(maho_ext_api::ToolDefinition::new(
            "gallery",
            "returns PNG images",
            json!({"type": "object"}),
            Arc::new(move |_| {
                Box::pin(async move {
                    let img = maho_ext_api::ToolContent::Image {
                        data: PNG.into(),
                        mime_type: "image/png".into(),
                    };
                    Ok(maho_ext_api::ToolResult {
                        content: vec![img; count],
                        ..maho_ext_api::ToolResult::text("")
                    })
                })
            }),
        ));
    }
}

struct ContextEntryObserver(Arc<Mutex<Vec<Option<String>>>>);

impl Extension for ContextEntryObserver {
    fn register(&self, api: &mut ExtensionApi) {
        let recorded = self.0.clone();
        let options = maho_ext_api::ExtensionHandlerOptions {
            mutates_messages: Some(false),
            ..Default::default()
        };
        api.on_with_options(
            EventKind::Context,
            Arc::new(move |event, _| {
                if let ExtensionEvent::Context { entry_ids, .. } = event {
                    *recorded.lock().expect("lock") = entry_ids.clone();
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
            options,
        );
    }
}

pub(crate) struct GalleryFixture {
    pub(crate) models: Vec<FauxModelDefinition>,
    pub(crate) image_count: usize,
    pub(crate) responses: Vec<AssistantMessage>,
    pub(crate) runner: maho_ext_look_at::runner::VisionModelRunner,
    pub(crate) extra: Option<Box<dyn Extension>>,
}

pub(crate) fn model_def(id: &str, vision: bool) -> FauxModelDefinition {
    let input = if vision {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };
    FauxModelDefinition {
        id: id.into(),
        input: Some(input),
        ..Default::default()
    }
}

pub(crate) fn call_gallery(id: &str) -> AssistantMessage {
    let options = FauxAssistantMessageOptions {
        stop_reason: Some(StopReason::ToolUse),
        ..Default::default()
    };
    faux_assistant_message(
        faux_tool_call("gallery", Default::default(), Some(id)),
        options,
    )
}

pub(crate) fn reply(text: &str) -> AssistantMessage {
    faux_assistant_message(text, FauxAssistantMessageOptions::default())
}

pub(crate) fn blocks_of(message: &Message) -> &[ContentBlock] {
    match message {
        Message::User(u) => match &u.content {
            UserContent::Blocks(b) => b.as_slice(),
            UserContent::Text(_) => panic!("expected blocks"),
        },
        Message::ToolResult(r) => r.content.as_slice(),
        _ => panic!("expected user or toolResult"),
    }
}

pub(crate) fn text_of(message: &Message) -> String {
    blocks_of(message)
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn has_image(message: &Message) -> bool {
    blocks_of(message)
        .iter()
        .any(|b| matches!(b, ContentBlock::Image(_)))
}

pub(crate) fn last_tool_result(handle: &NativeSession) -> Message {
    let calls = handle.provider_calls();
    let msgs = &calls.last().expect("call").context.messages;
    msgs.iter()
        .rfind(|m| matches!(m, Message::ToolResult(_)))
        .expect("toolResult")
        .clone()
}

pub(crate) fn stored_tool_result(handle: &NativeSession) -> Message {
    handle
        .messages()
        .into_iter()
        .find(|m| matches!(m, Message::ToolResult(_)))
        .expect("stored")
}

pub(crate) async fn boot_gallery(fixture: GalleryFixture) -> NativeSession {
    let script = FauxScript {
        name: "look-at".into(),
        prompt: "run".into(),
        responses: vec![],
    };
    let info = maho_ext_api::SourceInfo {
        source: "builtin".into(),
        ..Default::default()
    };
    let ext = Box::new(GalleryLook {
        runner: fixture.runner,
        image_count: fixture.image_count,
    });
    let factory = maho_ext_host::loader::NativeExtensionFactory {
        path: "builtin:look-at".into(),
        source_info: info.clone(),
        extension: ext,
    };
    let mut session = FauxSession::new(script)
        .with_faux_models(fixture.models)
        .with_native_extension(factory)
        .with_native_responses(fixture.responses);
    if let Some(extension) = fixture.extra {
        let obs = maho_ext_host::loader::NativeExtensionFactory {
            path: "test:observer".into(),
            source_info: info,
            extension,
        };
        session = session.with_native_extension(obs);
    }
    session.run_native_handle().await.expect("boot")
}

#[tokio::test]
async fn falls_back_to_naming_look_at_on_failure_and_for_user_images_preserving_entry_id() {
    let observed_ids = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let runner: maho_ext_look_at::runner::VisionModelRunner = Arc::new(move |_, _, _, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("vision backend down")) })
    });
    let handle = boot_gallery(GalleryFixture {
        models: vec![model_def("faux-1", false), model_def("vision", true)],
        image_count: 1,
        responses: vec![reply("done"), call_gallery("g1"), reply("done2")],
        runner,
        extra: Some(Box::new(ContextEntryObserver(observed_ids.clone()))),
    })
    .await;
    let img = ImageContent {
        data: PNG.into(),
        mime_type: "image/png".into(),
    };
    let opts = PromptOptions {
        images: Some(vec![img]),
        ..Default::default()
    };
    handle
        .agent_session()
        .prompt("what is this", opts)
        .await
        .expect("user prompt");
    let first_call = &handle.provider_calls()[0].context.messages;
    let received_user = first_call
        .iter()
        .rfind(|m| matches!(m, Message::User(_)))
        .expect("user");
    assert!(text_of(received_user).contains("look_at") && !has_image(received_user));
    let user_entry_id = handle
        .entries()
        .into_iter()
        .find(|e| e["type"] == "message" && e["message"]["role"] == "user")
        .and_then(|e| e["id"].as_str().map(str::to_owned))
        .expect("entry id");
    assert_eq!(
        *observed_ids.lock().expect("lock"),
        vec![Some(user_entry_id)]
    );

    handle
        .agent_session()
        .prompt("read shot.png", PromptOptions::default())
        .await
        .expect("tool prompt");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let received = last_tool_result(&handle);
    assert!(
        text_of(&received).contains("look_at")
            && !text_of(&received).contains("vision backend down")
    );
    assert!(!has_image(&received));
    let stored = stored_tool_result(&handle);
    assert!(matches!(&stored, Message::ToolResult(r) if !r.is_error) && has_image(&stored));
    handle.close().await;
}
