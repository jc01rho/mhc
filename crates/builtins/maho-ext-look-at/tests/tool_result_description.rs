use crate::image_placeholder::{
    GalleryFixture, blocks_of, boot_gallery, call_gallery, has_image, last_tool_result, model_def,
    reply, stored_tool_result, text_of,
};
use maho_ai::types::{ContentBlock, Message};
use maho_core::agent_session::PromptOptions;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const DESCRIPTION: &str = "A single dark pixel on a transparent background.";

#[tokio::test]
async fn puts_a_description_where_the_tool_result_image_was_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let runner: maho_ext_look_at::runner::VisionModelRunner = Arc::new(move |_, _, _, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(maho_ext_look_at::runner::LookAtRunResult {
                model: "faux/vision".into(),
                sources: vec!["base64 input".into()],
                mime_types: vec!["image/png".into()],
                text: DESCRIPTION.into(),
            })
        })
    });
    let handle = boot_gallery(GalleryFixture {
        models: vec![model_def("faux-1", false), model_def("vision", true)],
        image_count: 1,
        responses: vec![call_gallery("g1"), reply("done"), reply("still done")],
        runner,
        extra: None,
    })
    .await;
    handle
        .agent_session()
        .prompt("read shot.png", PromptOptions::default())
        .await
        .expect("p1");
    handle
        .agent_session()
        .prompt("and again", PromptOptions::default())
        .await
        .expect("p2");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let provider_calls = handle.provider_calls();
    assert_eq!(provider_calls.len(), 3);
    for call in &provider_calls[1..] {
        let msg = call
            .context
            .messages
            .iter()
            .rfind(|m| matches!(m, Message::ToolResult(_)))
            .expect("tr");
        assert!(text_of(msg).contains(DESCRIPTION) && text_of(msg).contains("look_at"));
        assert!(text_of(msg).contains("faux/vision") && !has_image(msg));
    }
    assert!(!has_image(&stored_tool_result(&handle)));
    handle.close().await;
}

#[tokio::test]
async fn describes_at_most_three_images_of_one_tool_result() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let runner: maho_ext_look_at::runner::VisionModelRunner = Arc::new(move |_, _, _, _| {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        let text = ["first", "second", "third"][n.min(2)].to_owned();
        Box::pin(async move {
            Ok(maho_ext_look_at::runner::LookAtRunResult {
                model: "faux/vision".into(),
                sources: vec!["base64 input".into()],
                mime_types: vec!["image/png".into()],
                text,
            })
        })
    });
    let handle = boot_gallery(GalleryFixture {
        models: vec![model_def("faux-1", false), model_def("vision", true)],
        image_count: 4,
        responses: vec![call_gallery("g4"), reply("done")],
        runner,
        extra: None,
    })
    .await;
    handle
        .agent_session()
        .prompt("show the gallery", PromptOptions::default())
        .await
        .expect("p4");

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let stored = stored_tool_result(&handle);
    let st_types: Vec<&str> = blocks_of(&stored)
        .iter()
        .map(ContentBlock::type_name)
        .collect();
    assert_eq!(st_types, vec!["text", "text", "text", "image"]);
    let received = last_tool_result(&handle);
    let rc_types: Vec<&str> = blocks_of(&received)
        .iter()
        .map(ContentBlock::type_name)
        .collect();
    assert_eq!(rc_types, vec!["text", "text", "text", "text"]);
    assert!(
        matches!(&blocks_of(&received)[3], ContentBlock::Text(t) if t.text.contains("look_at"))
    );
    handle.close().await;
}

#[tokio::test]
async fn names_no_tool_without_vision_and_passes_images_to_vision_models() {
    for (mode, vision) in [("no-vision", false), ("vision-main", true)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let runner: maho_ext_look_at::runner::VisionModelRunner = Arc::new(move |_, _, _, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("should not run")) })
        });
        let handle = boot_gallery(GalleryFixture {
            models: vec![model_def("faux-1", vision)],
            image_count: 1,
            responses: vec![call_gallery(mode), reply("done")],
            runner,
            extra: None,
        })
        .await;
        handle
            .agent_session()
            .prompt("read shot.png", PromptOptions::default())
            .await
            .expect("p");
        assert!(
            !handle
                .agent_session()
                .get_active_tool_names()
                .iter()
                .any(|n| n == "look_at")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let received = last_tool_result(&handle);
        assert!(has_image(&received) && !text_of(&received).contains("look_at"));
        handle.close().await;
    }
}
